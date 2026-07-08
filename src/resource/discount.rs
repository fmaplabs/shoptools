//! Discounts. Two things make this the trickiest resource so far:
//!   1. `discount` is a GraphQL *union* — you read it with inline fragments.
//!   2. The shape you READ (export) is NOT the shape you WRITE (create). We
//!      capture the rich read shape here and translate it into the create input
//!      inside `import`. That translation is the interesting part — and for
//!      collection-scoped discounts it includes remapping collection *handles*
//!      to the target store's collection *ids*.
//!
//! Bulk export flattens the three nested connections (`discountNodes`, `codes`,
//! `customerGets…collections`) into separate JSONL lines. Because only the
//! `DiscountNode` root selects an `id`, both code and collection child lines
//! carry that root id as `__parentId`; `bulk::reassemble` routes them by
//! `__typename` to top-level `codes`/`collections` fields, and
//! [`reshape_discount_node`] then splices them back into the legacy nested
//! locations so on-disk files are unchanged.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Resource;
use crate::bulk::{self, ChildSpec};
use crate::client::ShopifyClient;

pub struct Discount;

/// One node from `discountNodes`: an id plus the nested discount union.
#[derive(Debug, Deserialize, Serialize)]
struct DiscountNodeRecord {
    id: String,
    discount: DiscountRecord,
}

/// The fields we read off a discount. Almost everything is `Option` because the
/// discount is a union: a free-shipping discount has no `customerGets`, an
/// automatic discount has no `codes`, and so on. `Option` = "may be absent."
#[derive(Debug, Deserialize, Serialize)]
struct DiscountRecord {
    #[serde(rename = "__typename")]
    typename: String,
    title: Option<String>,
    status: Option<String>,
    #[serde(rename = "startsAt")]
    starts_at: Option<String>,
    #[serde(rename = "endsAt")]
    ends_at: Option<String>,
    /// Present only on *code* discounts. A connection, so it nests.
    codes: Option<CodeConnection>,
    /// Who the discount is for (code discounts). Kept as raw JSON — we only
    /// peek at its `__typename` to confirm it targets all customers.
    #[serde(rename = "customerSelection")]
    customer_selection: Option<Value>,
    /// The value (e.g. 10% off) and what it applies to. Read shape ≠ write
    /// shape, so we hold it as raw JSON and translate in `import`.
    #[serde(rename = "customerGets")]
    customer_gets: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CodeConnection {
    nodes: Vec<CodeNode>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CodeNode {
    code: String,
}

/// Create mutations differ by discount type. Code discounts need a code +
/// customer selection; automatic discounts apply to everyone automatically.
const CODE_BASIC_CREATE: &str = r#"
mutation CreateCodeBasic($basicCodeDiscount: DiscountCodeBasicInput!) {
  discountCodeBasicCreate(basicCodeDiscount: $basicCodeDiscount) {
    codeDiscountNode { id }
    userErrors { field message }
  }
}
"#;

const AUTOMATIC_BASIC_CREATE: &str = r#"
mutation CreateAutomaticBasic($automaticBasicDiscount: DiscountAutomaticBasicInput!) {
  discountAutomaticBasicCreate(automaticBasicDiscount: $automaticBasicDiscount) {
    automaticDiscountNode { id }
    userErrors { field message }
  }
}
"#;

/// Bulk export query (validated ✅ 2026-07). `edges { node { … } }` with no
/// cursors/`pageInfo` on all three connections (`discountNodes`, `codes`,
/// `collections`) — 3 connections at depth 2, within the bulk limits. Only the
/// `DiscountNode` root selects `id`, so the `codes` (→ `DiscountRedeemCode`) and
/// `collections` (→ `Collection`) child lines both carry the root id as
/// `__parentId`; both child selections include `__typename` so `bulk::reassemble`
/// can route them, and [`reshape_discount_node`] splices them into place. The
/// `customerSelection` deprecation warning is pre-existing (kept for on-disk shape
/// parity with the legacy export).
const BULK_DISCOUNTS: &str = r#"
query BulkDiscounts {
  discountNodes {
    edges {
      node {
        id
        discount {
          __typename
          ... on DiscountCodeBasic {
            title
            status
            startsAt
            endsAt
            codes { edges { node { __typename code } } }
            customerSelection {
              __typename
              ... on DiscountCustomerAll { allCustomers }
            }
            customerGets {
              value { __typename ... on DiscountPercentage { percentage } }
              items {
                __typename
                ... on AllDiscountItems { allItems }
                ... on DiscountCollections {
                  collections { edges { node { __typename handle } } }
                }
              }
            }
          }
          ... on DiscountAutomaticBasic {
            title
            status
            startsAt
            endsAt
            customerGets {
              value { __typename ... on DiscountPercentage { percentage } }
              items {
                __typename
                ... on AllDiscountItems { allItems }
                ... on DiscountCollections {
                  collections { edges { node { __typename handle } } }
                }
              }
            }
          }
          ... on DiscountCodeFreeShipping { title status }
          ... on DiscountAutomaticFreeShipping { title status }
        }
      }
    }
  }
}
"#;

impl Resource for Discount {
    fn name(&self) -> &'static str {
        "discounts"
    }

    fn export(&self, client: &ShopifyClient, no_bulk: bool) -> Result<Value> {
        if no_bulk {
            return export_legacy(client);
        }

        let lines = bulk::bulk_query(client, BULK_DISCOUNTS)?;
        // `reassemble` routes children by `__typename` to *top-level* fields on
        // the DiscountNode root; `reshape_discount_node` then moves them into the
        // legacy nested locations (`discount.codes`, `…items.collections`).
        let specs = [
            ChildSpec {
                typename: "DiscountRedeemCode",
                field: "codes",
            },
            ChildSpec {
                typename: "Collection",
                field: "collections",
            },
        ];
        let nodes = bulk::reassemble(lines, &specs);
        let reshaped: Vec<Value> = nodes.into_iter().map(reshape_discount_node).collect();

        Ok(Value::Array(reshaped))
    }

    fn import(
        &self,
        client: &ShopifyClient,
        data: &Value,
        dry_run: bool,
        no_bulk: bool,
    ) -> Result<()> {
        // NOW we deserialize — this is import's job, and dry_run lives here.
        let nodes: Vec<DiscountNodeRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of discount nodes")?;

        println!("{} discount(s) to import", nodes.len());

        if dry_run {
            for node in &nodes {
                let d = &node.discount;
                let title = d.title.as_deref().unwrap_or("(untitled)");
                // dry-run never hits the network, so we just describe the source.
                match percentage_of(d) {
                    Some(p) => {
                        println!(
                            "  would create: {title} [{}] — {}% off {}",
                            d.typename,
                            p * 100.0,
                            scope_of(d)
                        )
                    }
                    None => println!("  would create: {title} [{}] {}", d.typename, scope_of(d)),
                }
            }
            return Ok(());
        }

        if no_bulk {
            return import_legacy(client, &nodes);
        }

        import_bulk(client, &nodes)
    }
}

/// Legacy cursor-paginated export (used with `--no-bulk`). Unlike the bulk path,
/// a discount's `collections` list is capped at the inline `first: 250`; the old
/// per-discount overflow re-fetch (`DISCOUNT_COLLECTIONS`) is gone — reach for
/// bulk if you have collection-scoped discounts spanning >250 collections.
fn export_legacy(client: &ShopifyClient) -> Result<Value> {
    let nodes = client.paginate(
        r#"
        query Discounts($cursor: String) {
          discountNodes(first: 50, after: $cursor) {
            nodes {
              id
              discount {
                __typename
                ... on DiscountCodeBasic {
                  title
                  status
                  startsAt
                  endsAt
                  codes(first: 1) { nodes { code } }
                  customerSelection {
                    __typename
                    ... on DiscountCustomerAll { allCustomers }
                  }
                  customerGets {
                    value { __typename ... on DiscountPercentage { percentage } }
                    items {
                      __typename
                      ... on AllDiscountItems { allItems }
                      ... on DiscountCollections {
                        collections(first: 250) {
                          nodes { handle }
                          pageInfo { hasNextPage endCursor }
                        }
                      }
                    }
                  }
                }
                ... on DiscountAutomaticBasic {
                  title
                  status
                  startsAt
                  endsAt
                  customerGets {
                    value { __typename ... on DiscountPercentage { percentage } }
                    items {
                      __typename
                      ... on AllDiscountItems { allItems }
                      ... on DiscountCollections {
                        collections(first: 250) {
                          nodes { handle }
                          pageInfo { hasNextPage endCursor }
                        }
                      }
                    }
                  }
                }
                ... on DiscountCodeFreeShipping { title status }
                ... on DiscountAutomaticFreeShipping { title status }
              }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
        "#,
        json!({}),
        "discountNodes",
    )?;

    Ok(Value::Array(nodes))
}

/// After `reassemble` routes the code and collection child lines to *top-level*
/// `codes`/`collections` fields on the DiscountNode root (they can only target a
/// top-level field), splice them back into the legacy nested locations:
///   * `codes`       → `discount.codes`
///   * `collections` → `discount.customerGets.items.collections`
///
/// Pure — no network — so it's unit-testable. Nodes without child lines (free
/// shipping, automatic-on-all-items) are returned unchanged.
fn reshape_discount_node(mut node: Value) -> Value {
    if let Some(codes) = node.as_object_mut().and_then(|o| o.remove("codes")) {
        node["discount"]["codes"] = codes;
    }
    if let Some(collections) = node.as_object_mut().and_then(|o| o.remove("collections")) {
        node["discount"]["customerGets"]["items"]["collections"] = collections;
    }
    node
}

/// Pull the percentage (0.0–1.0) out of the raw `customerGets`, if this is a
/// percentage discount. Returns None for fixed-amount or free-shipping discounts.
fn percentage_of(d: &DiscountRecord) -> Option<f64> {
    d.customer_gets.as_ref()?["value"]["percentage"].as_f64()
}

/// A short human description of what the discount applies to, for dry-run output.
fn scope_of(d: &DiscountRecord) -> String {
    let Some(cg) = d.customer_gets.as_ref() else {
        return String::new();
    };
    let items = &cg["items"];
    match items["__typename"].as_str() {
        Some("AllDiscountItems") => "on all items".to_string(),
        Some("DiscountCollections") => {
            let n = items["collections"]["nodes"].as_array().map_or(0, |a| a.len());
            format!("on {n} collection(s)")
        }
        _ => String::new(),
    }
}

/// Translate the READ shape of `customerGets` into the CREATE input shape.
///   read:  value { __typename, percentage }, items { __typename, allItems | collections { nodes { handle } } }
///   write: value { percentage },             items { all: true | collections { add: [<target ids>] } }
///
/// Collection scoping is the interesting case: the source stores collection
/// *ids*, which are meaningless in the target store, so we remap each collection
/// by its stable *handle*. `resolve` maps a handle to a target-store collection
/// id — the legacy path passes `client.resolve_collection`, the bulk path a
/// pre-built handle→id map lookup.
fn build_customer_gets(
    resolve: &dyn Fn(&str) -> Result<String>,
    d: &DiscountRecord,
) -> Result<Value> {
    let percentage =
        percentage_of(d).context("only percentage-value discounts are supported for now")?;

    let cg = d
        .customer_gets
        .as_ref()
        .context("discount is missing customerGets")?;
    let items = &cg["items"];

    let items_input = match items["__typename"].as_str() {
        Some("AllDiscountItems") => json!({ "all": true }),
        Some("DiscountCollections") => {
            let nodes = items["collections"]["nodes"]
                .as_array()
                .context("collection-scoped discount has no collections")?;
            let mut ids = Vec::new();
            for node in nodes {
                let handle = node["handle"]
                    .as_str()
                    .context("collection is missing its handle")?;
                ids.push(resolve(handle)?);
            }
            json!({ "collections": { "add": ids } })
        }
        other => bail!("unsupported discount item scope: {other:?}"),
    };

    Ok(json!({
        "value": { "percentage": percentage },
        "items": items_input,
    }))
}

/// Build the `automaticBasicDiscount` input (write shape) for a discount record.
/// Pure translation; `resolve` supplies target-store collection ids.
fn automatic_basic_input(
    resolve: &dyn Fn(&str) -> Result<String>,
    d: &DiscountRecord,
) -> Result<Value> {
    Ok(json!({
        "title": d.title,
        "startsAt": d.starts_at,
        "endsAt": d.ends_at,
        "customerGets": build_customer_gets(resolve, d)?,
    }))
}

/// Build the `basicCodeDiscount` input (write shape) for a discount record.
/// We only handle "all customers" code discounts; targeting specific
/// customers/segments would need those IDs to already exist in the TARGET store.
fn code_basic_input(resolve: &dyn Fn(&str) -> Result<String>, d: &DiscountRecord) -> Result<Value> {
    let all_customers = d
        .customer_selection
        .as_ref()
        .and_then(|s| s["__typename"].as_str())
        == Some("DiscountCustomerAll");
    if !all_customers {
        bail!("only 'all customers' code discounts are supported for now");
    }

    let code = code_of(d)?;

    Ok(json!({
        "title": d.title,
        "code": code,
        "startsAt": d.starts_at,
        "endsAt": d.ends_at,
        "customerSelection": { "all": true },
        "customerGets": build_customer_gets(resolve, d)?,
    }))
}

/// The redeem code of a code discount (its first code node).
fn code_of(d: &DiscountRecord) -> Result<String> {
    d.codes
        .as_ref()
        .and_then(|c| c.nodes.first())
        .map(|n| n.code.clone())
        .context("code discount is missing its code")
}

/// Legacy per-record import (used with `--no-bulk`): one create mutation per
/// discount, resolving collections per record against the target store.
fn import_legacy(client: &ShopifyClient, nodes: &[DiscountNodeRecord]) -> Result<()> {
    let resolve = |handle: &str| client.resolve_collection(handle);
    for node in nodes {
        let d = &node.discount;
        let title = d.title.as_deref().unwrap_or("(untitled)");
        match d.typename.as_str() {
            "DiscountAutomaticBasic" => {
                let input = automatic_basic_input(&resolve, d)?;
                let result = client
                    .graphql(AUTOMATIC_BASIC_CREATE, json!({ "automaticBasicDiscount": input }))?;
                check_user_errors(&result["discountAutomaticBasicCreate"], d)?;
                println!("  created {title} (automatic)");
            }
            "DiscountCodeBasic" => {
                let input = code_basic_input(&resolve, d)?;
                let code = code_of(d)?;
                let result =
                    client.graphql(CODE_BASIC_CREATE, json!({ "basicCodeDiscount": input }))?;
                check_user_errors(&result["discountCodeBasicCreate"], d)?;
                println!("  created {title} (code: {code})");
            }
            other => bail!("cloning '{title}' isn't supported yet (type: {other})"),
        }
    }
    Ok(())
}

/// Bulk import: pre-resolve every collection handle once, split records into the
/// two mutation-specific JSONL line-sets by `__typename`, then run the two bulk
/// mutations sequentially (only one bulk op runs per shop at a time). Records we
/// can't build (unsupported type, non-percentage, unresolvable collection, …) are
/// warned about and skipped, mirroring the legacy per-record best-effort behavior.
fn import_bulk(client: &ShopifyClient, nodes: &[DiscountNodeRecord]) -> Result<()> {
    // ONE pre-resolution pass: build a target-store handle→id map so per-record
    // collection remapping is a cheap local lookup instead of N network calls.
    let collection_map = build_collection_map(client)?;
    let resolve = |handle: &str| {
        collection_map
            .get(handle)
            .cloned()
            .with_context(|| format!("target store has no collection with handle '{handle}'"))
    };

    // Split by discount type: bulk mutations take exactly one mutation string, so
    // code and automatic discounts go through two separate bulk operations. `meta`
    // holds (title, success-suffix) per line so results can be reported in order.
    let mut code_lines: Vec<Value> = Vec::new();
    let mut code_meta: Vec<(String, String)> = Vec::new();
    let mut auto_lines: Vec<Value> = Vec::new();
    let mut auto_meta: Vec<(String, String)> = Vec::new();

    for node in nodes {
        let d = &node.discount;
        let title = d.title.clone().unwrap_or_else(|| "(untitled)".to_string());
        match d.typename.as_str() {
            "DiscountCodeBasic" => match code_basic_input(&resolve, d) {
                Ok(input) => {
                    let suffix = code_of(d)
                        .map(|c| format!("code: {c}"))
                        .unwrap_or_else(|_| "code".to_string());
                    code_lines.push(json!({ "basicCodeDiscount": input }));
                    code_meta.push((title, suffix));
                }
                Err(e) => println!("  skipped {title}: {e}"),
            },
            "DiscountAutomaticBasic" => match automatic_basic_input(&resolve, d) {
                Ok(input) => {
                    auto_lines.push(json!({ "automaticBasicDiscount": input }));
                    auto_meta.push((title, "automatic".to_string()));
                }
                Err(e) => println!("  skipped {title}: {e}"),
            },
            other => println!("  skipped {title}: cloning isn't supported yet (type: {other})"),
        }
    }

    // Two sequential bulk ops, skipping empty sets.
    if !code_lines.is_empty() {
        run_bulk_set(
            client,
            CODE_BASIC_CREATE,
            &code_lines,
            &code_meta,
            "discountCodeBasicCreate",
        )?;
    }
    if !auto_lines.is_empty() {
        run_bulk_set(
            client,
            AUTOMATIC_BASIC_CREATE,
            &auto_lines,
            &auto_meta,
            "discountAutomaticBasicCreate",
        )?;
    }
    Ok(())
}

/// Run one bulk mutation over `lines`, then report per record. Results arrive out
/// of order; `__lineNumber` indexes back into the input file, so sort by it
/// before zipping with `meta`. A non-empty `userErrors` is a per-record skip
/// (e.g. duplicate code on re-run), not a fatal error.
fn run_bulk_set(
    client: &ShopifyClient,
    mutation: &str,
    lines: &[Value],
    meta: &[(String, String)],
    field: &str,
) -> Result<()> {
    let mut results = bulk::bulk_mutation(client, mutation, lines)?;
    results.sort_by_key(|r| r["__lineNumber"].as_u64().unwrap_or(u64::MAX));

    for ((title, suffix), result) in meta.iter().zip(results.iter()) {
        let payload = mutation_payload(result, field);
        if let Some(errors) = payload["userErrors"].as_array()
            && !errors.is_empty()
        {
            println!("  skipped {title}: {}", payload["userErrors"]);
            continue;
        }
        println!("  created {title} ({suffix})");
    }
    Ok(())
}

/// Locate a mutation payload inside one bulk-mutation result line. Bulk results
/// wrap each line's output in `data`; fall back to the bare payload if absent.
fn mutation_payload<'a>(result: &'a Value, field: &str) -> &'a Value {
    if result.get("data").is_some() {
        &result["data"][field]
    } else {
        &result[field]
    }
}

/// Build a target-store collection handle→id map in one paginated pass. Replaces
/// the per-record `resolve_collection` calls in the bulk import path.
fn build_collection_map(client: &ShopifyClient) -> Result<HashMap<String, String>> {
    let collections = client.paginate(
        r#"
        query Collections($cursor: String) {
          collections(first: 250, after: $cursor) {
            nodes { handle id }
            pageInfo { hasNextPage endCursor }
          }
        }
        "#,
        json!({}),
        "collections",
    )?;

    let mut map = HashMap::new();
    for c in collections {
        if let (Some(handle), Some(id)) = (c["handle"].as_str(), c["id"].as_str()) {
            map.insert(handle.to_string(), id.to_string());
        }
    }
    Ok(map)
}

/// Both create mutations return `userErrors`; check them the same way (fatal on
/// the legacy per-record path).
fn check_user_errors(payload: &Value, d: &DiscountRecord) -> Result<()> {
    if let Some(errors) = payload["userErrors"].as_array() {
        if !errors.is_empty() {
            bail!(
                "could not create '{}': {}",
                d.title.as_deref().unwrap_or("?"),
                payload["userErrors"]
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A code discount: reassemble drops the code line under a top-level `codes`
    /// field; reshape must splice it back under `discount.codes`.
    #[test]
    fn reshape_splices_codes_under_discount() {
        let node = json!({
            "id": "gid://shopify/DiscountNode/1",
            "discount": {
                "__typename": "DiscountCodeBasic",
                "title": "Sale",
                "customerGets": { "value": { "percentage": 0.1 }, "items": { "__typename": "AllDiscountItems", "allItems": true } }
            },
            "codes": { "nodes": [{ "code": "SAVE10" }] }
        });

        let out = reshape_discount_node(node);

        // codes moved into discount.codes, no longer at top level.
        assert!(out.get("codes").is_none());
        assert_eq!(out["discount"]["codes"]["nodes"][0]["code"], "SAVE10");
        // The rest of discount is untouched.
        assert_eq!(out["discount"]["title"], "Sale");
    }

    /// A collection-scoped discount: reshape must nest collections under
    /// `discount.customerGets.items.collections`.
    #[test]
    fn reshape_splices_collections_into_customer_gets_items() {
        let node = json!({
            "id": "gid://shopify/DiscountNode/2",
            "discount": {
                "__typename": "DiscountAutomaticBasic",
                "title": "Coll Sale",
                "customerGets": {
                    "value": { "percentage": 0.2 },
                    "items": { "__typename": "DiscountCollections" }
                }
            },
            "collections": { "nodes": [{ "handle": "shoes" }, { "handle": "hats" }] }
        });

        let out = reshape_discount_node(node);

        assert!(out.get("collections").is_none());
        let cols = out["discount"]["customerGets"]["items"]["collections"]["nodes"]
            .as_array()
            .unwrap();
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0]["handle"], "shoes");
        assert_eq!(cols[1]["handle"], "hats");
    }

    /// Free-shipping / all-items discounts have no child lines; reshape is a
    /// no-op that leaves the node exactly as-is.
    #[test]
    fn reshape_no_children_is_noop() {
        let node = json!({
            "id": "gid://shopify/DiscountNode/3",
            "discount": { "__typename": "DiscountCodeFreeShipping", "title": "Ship", "status": "ACTIVE" }
        });

        let out = reshape_discount_node(node.clone());
        assert_eq!(out, node);
    }

    /// End-to-end shape: flat JSONL lines → reassemble → reshape must reproduce
    /// the legacy nested export shape the DTOs deserialize.
    #[test]
    fn reassemble_then_reshape_matches_legacy_shape() {
        let lines = vec![
            json!({
                "id": "gid://shopify/DiscountNode/1",
                "discount": {
                    "__typename": "DiscountCodeBasic",
                    "title": "Sale",
                    "customerSelection": { "__typename": "DiscountCustomerAll", "allCustomers": true },
                    "customerGets": {
                        "value": { "__typename": "DiscountPercentage", "percentage": 0.15 },
                        "items": { "__typename": "DiscountCollections" }
                    }
                }
            }),
            json!({ "__typename": "DiscountRedeemCode", "__parentId": "gid://shopify/DiscountNode/1", "code": "SAVE15" }),
            json!({ "__typename": "Collection", "__parentId": "gid://shopify/DiscountNode/1", "handle": "shoes" }),
        ];
        let specs = [
            ChildSpec {
                typename: "DiscountRedeemCode",
                field: "codes",
            },
            ChildSpec {
                typename: "Collection",
                field: "collections",
            },
        ];

        let nodes = bulk::reassemble(lines, &specs);
        let reshaped: Vec<Value> = nodes.into_iter().map(reshape_discount_node).collect();

        assert_eq!(reshaped.len(), 1);
        // Deserializes into the import DTO cleanly.
        let records: Vec<DiscountNodeRecord> =
            serde_json::from_value(Value::Array(reshaped.clone())).unwrap();
        let d = &records[0].discount;
        assert_eq!(d.typename, "DiscountCodeBasic");
        assert_eq!(d.codes.as_ref().unwrap().nodes[0].code, "SAVE15");
        // Collections nested in the legacy location.
        assert_eq!(
            reshaped[0]["discount"]["customerGets"]["items"]["collections"]["nodes"][0]["handle"],
            "shoes"
        );
    }

    #[test]
    fn build_customer_gets_all_items() {
        let d: DiscountRecord = serde_json::from_value(json!({
            "__typename": "DiscountAutomaticBasic",
            "title": "All",
            "customerGets": {
                "value": { "__typename": "DiscountPercentage", "percentage": 0.25 },
                "items": { "__typename": "AllDiscountItems", "allItems": true }
            }
        }))
        .unwrap();

        let resolve = |_h: &str| -> Result<String> { panic!("should not resolve for all-items") };
        let cg = build_customer_gets(&resolve, &d).unwrap();
        assert_eq!(cg["value"]["percentage"], 0.25);
        assert_eq!(cg["items"]["all"], true);
    }

    #[test]
    fn build_customer_gets_remaps_collection_handles() {
        let d: DiscountRecord = serde_json::from_value(json!({
            "__typename": "DiscountAutomaticBasic",
            "title": "Coll",
            "customerGets": {
                "value": { "__typename": "DiscountPercentage", "percentage": 0.1 },
                "items": {
                    "__typename": "DiscountCollections",
                    "collections": { "nodes": [{ "handle": "shoes" }, { "handle": "hats" }] }
                }
            }
        }))
        .unwrap();

        let resolve = |h: &str| -> Result<String> { Ok(format!("gid://shopify/Collection/{h}")) };
        let cg = build_customer_gets(&resolve, &d).unwrap();
        let add = cg["items"]["collections"]["add"].as_array().unwrap();
        assert_eq!(add[0], "gid://shopify/Collection/shoes");
        assert_eq!(add[1], "gid://shopify/Collection/hats");
    }

    #[test]
    fn build_customer_gets_reports_unresolvable_handle() {
        let d: DiscountRecord = serde_json::from_value(json!({
            "__typename": "DiscountAutomaticBasic",
            "title": "Coll",
            "customerGets": {
                "value": { "__typename": "DiscountPercentage", "percentage": 0.1 },
                "items": {
                    "__typename": "DiscountCollections",
                    "collections": { "nodes": [{ "handle": "ghost" }] }
                }
            }
        }))
        .unwrap();

        let resolve = |h: &str| -> Result<String> { bail!("target store has no collection with handle '{h}'") };
        let err = build_customer_gets(&resolve, &d).unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }

    #[test]
    fn code_basic_input_rejects_non_all_customers() {
        let d: DiscountRecord = serde_json::from_value(json!({
            "__typename": "DiscountCodeBasic",
            "title": "Targeted",
            "codes": { "nodes": [{ "code": "X" }] },
            "customerSelection": { "__typename": "DiscountCustomerSegments" },
            "customerGets": {
                "value": { "__typename": "DiscountPercentage", "percentage": 0.1 },
                "items": { "__typename": "AllDiscountItems", "allItems": true }
            }
        }))
        .unwrap();

        let resolve = |_h: &str| -> Result<String> { unreachable!() };
        let err = code_basic_input(&resolve, &d).unwrap_err();
        assert!(err.to_string().contains("all customers"));
    }

    #[test]
    fn code_basic_input_builds_write_shape() {
        let d: DiscountRecord = serde_json::from_value(json!({
            "__typename": "DiscountCodeBasic",
            "title": "Sale",
            "startsAt": "2024-01-01T00:00:00Z",
            "endsAt": null,
            "codes": { "nodes": [{ "code": "SAVE10" }] },
            "customerSelection": { "__typename": "DiscountCustomerAll", "allCustomers": true },
            "customerGets": {
                "value": { "__typename": "DiscountPercentage", "percentage": 0.1 },
                "items": { "__typename": "AllDiscountItems", "allItems": true }
            }
        }))
        .unwrap();

        let resolve = |_h: &str| -> Result<String> { unreachable!() };
        let input = code_basic_input(&resolve, &d).unwrap();
        assert_eq!(input["code"], "SAVE10");
        assert_eq!(input["customerSelection"]["all"], true);
        assert_eq!(input["customerGets"]["value"]["percentage"], 0.1);
        assert_eq!(input["customerGets"]["items"]["all"], true);
    }

    #[test]
    fn mutation_payload_unwraps_bulk_data_wrapper() {
        let wrapped = json!({ "data": { "discountCodeBasicCreate": { "userErrors": [] } }, "__lineNumber": 0 });
        assert!(mutation_payload(&wrapped, "discountCodeBasicCreate")["userErrors"]
            .as_array()
            .unwrap()
            .is_empty());

        let bare = json!({ "discountCodeBasicCreate": { "userErrors": [{ "message": "dup" }] } });
        assert_eq!(
            mutation_payload(&bare, "discountCodeBasicCreate")["userErrors"][0]["message"],
            "dup"
        );
    }
}
