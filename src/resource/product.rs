//! Products — now with **options, variants, and prices**, not just the flat
//! title/handle/status of the first pass.
//!
//! This is a read-shape ≠ write-shape resource (like `discount.rs`):
//! * READ:  `options { name optionValues { name } }` and
//!   `variants { nodes { price sku selectedOptions { name value } … } }`
//! * WRITE: `productSet`'s `productOptions [{ name, values:[{name}] }]` and
//!   `variants [{ price, sku, optionValues:[{optionName, name}] }]`
//!
//! We use `productSet` (not the old `productCreate`) because it creates a product
//! together with its options and every variant in a single call. The translation
//! lives in the pure `build_product_set_input` — pure so it's unit-testable
//! without a live client (see the tests at the bottom).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Resource;
use crate::bulk::{self, ChildSpec};
use crate::client::ShopifyClient;

pub struct Product;

/// One product's data, deserialized from the exported JSON. The source `id` is
/// ignored on deserialize (products are matched across stores by `handle`).
#[derive(Debug, Deserialize, Serialize)]
struct ProductRecord {
    handle: String,
    title: Option<String>,
    status: Option<String>,
    #[serde(rename = "descriptionHtml")]
    description_html: Option<String>,
    #[serde(rename = "productType")]
    product_type: Option<String>,
    vendor: Option<String>,
    tags: Option<Vec<String>>,
    options: Option<Vec<OptionRecord>>,
    variants: VariantConnection,
}

/// A product option (e.g. "Color") and the values it can take. The read query's
/// `position` field is ignored by serde.
#[derive(Debug, Deserialize, Serialize)]
struct OptionRecord {
    name: String,
    #[serde(rename = "optionValues")]
    option_values: Vec<OptionValueRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OptionValueRecord {
    name: String,
}

/// The variants connection. We only care about `nodes`; `pageInfo` is ignored.
#[derive(Debug, Deserialize, Serialize)]
struct VariantConnection {
    nodes: Vec<VariantRecord>,
}

/// One variant. `selectedOptions` is the read shape; it becomes `optionValues`
/// (`{ optionName, name }`) in the create input.
#[derive(Debug, Deserialize, Serialize)]
struct VariantRecord {
    sku: Option<String>,
    price: Option<String>,
    #[serde(rename = "compareAtPrice")]
    compare_at_price: Option<String>,
    barcode: Option<String>,
    taxable: Option<bool>,
    #[serde(rename = "inventoryPolicy")]
    inventory_policy: Option<String>,
    #[serde(rename = "selectedOptions")]
    selected_options: Vec<SelectedOption>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SelectedOption {
    name: String,
    value: String,
}

/// `productSet` creates the product, its options, and all variants at once.
/// We pass `handle` in the input (not the `identifier` argument, whose `handle`
/// support isn't documented), so this creates; a re-run hits a duplicate-handle
/// userError which we skip.
const PRODUCT_SET: &str = r#"
mutation SetProduct($input: ProductSetInput!) {
  productSet(input: $input, synchronous: true) {
    product { id handle }
    userErrors { field message }
  }
}
"#;

/// Bulk export query (validated ✅ 2026-07). `edges { node { … } }` with no
/// cursors/`pageInfo`; the Bulk Operations API streams *every* product and
/// variant server-side, so there's no variant cap and no overflow re-fetch. The
/// `variants` children come back as separate JSONL lines tagged with `__parentId`
/// and `__typename`, which `bulk::reassemble` nests back under `variants.nodes`.
const BULK_PRODUCTS: &str = r#"
query BulkProducts {
  products {
    edges {
      node {
        id
        handle
        title
        status
        descriptionHtml
        productType
        vendor
        tags
        options { name position optionValues { name } }
        variants {
          edges {
            node {
              __typename
              id sku price compareAtPrice barcode taxable inventoryPolicy
              selectedOptions { name value }
            }
          }
        }
      }
    }
  }
}
"#;

impl Resource for Product {
    fn name(&self) -> &'static str {
        "products"
    }

    fn export(&self, client: &ShopifyClient, no_bulk: bool) -> Result<Value> {
        if no_bulk {
            return export_legacy(client);
        }

        let lines = bulk::bulk_query(client, BULK_PRODUCTS)?;
        let specs = [ChildSpec {
            typename: "ProductVariant",
            field: "variants",
        }];
        let mut products = bulk::reassemble(lines, &specs);

        // Guarantee the on-disk shape: a product with no variant lines still
        // gets `variants: { nodes: [] }` so re-import deserializes cleanly.
        for p in &mut products {
            if !p["variants"].is_object() {
                p["variants"] = json!({ "nodes": [] });
            }
        }

        Ok(Value::Array(products))
    }

    fn import(
        &self,
        client: &ShopifyClient,
        data: &Value,
        dry_run: bool,
        no_bulk: bool,
    ) -> Result<()> {
        let products: Vec<ProductRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of products")?;

        println!("{} product(s) to import", products.len());

        // dry-run: build the inputs and print, but touch no network.
        if dry_run {
            for p in &products {
                println!(
                    "  would create: {} ({}, {} variant(s))",
                    p.handle,
                    p.status.as_deref().unwrap_or("?"),
                    p.variants.nodes.len()
                );
            }
            return Ok(());
        }

        if no_bulk {
            return import_legacy(client, &products);
        }

        // Bulk import: one `productSet` invocation per JSONL line. Same mutation
        // string and input builder as the legacy path — only the transport differs.
        let lines: Vec<Value> = products
            .iter()
            .map(|p| json!({ "input": build_product_set_input(p) }))
            .collect();
        let mut results = bulk::bulk_mutation(client, PRODUCT_SET, &lines)?;

        // Results may arrive out of order; `__lineNumber` indexes back into the
        // input file, so sort by it before zipping with the products.
        results.sort_by_key(|r| r["__lineNumber"].as_u64().unwrap_or(u64::MAX));

        for (p, result) in products.iter().zip(results.iter()) {
            let variant_count = p.variants.nodes.len();
            let payload = product_set_payload(result);

            // Best-effort per product: a duplicate handle (re-run) or bad data
            // surfaces as a per-line userError skip, not a fatal error.
            if let Some(errors) = payload["userErrors"].as_array()
                && !errors.is_empty()
            {
                println!("  skipped {}: {}", p.handle, payload["userErrors"]);
                continue;
            }
            println!("  created {} ({} variant(s))", p.handle, variant_count);
        }
        Ok(())
    }
}

/// Legacy cursor-paginated export (used with `--no-bulk`). Unlike the bulk path,
/// a product's `variants` are capped at the inline `first: 100`; the old
/// per-product overflow re-fetch is gone — reach for bulk if you have >100.
fn export_legacy(client: &ShopifyClient) -> Result<Value> {
    let products = client.paginate(
        r#"
        query Products($cursor: String) {
          products(first: 50, after: $cursor) {
            nodes {
              id
              handle
              title
              status
              descriptionHtml
              productType
              vendor
              tags
              options { name position optionValues { name } }
              variants(first: 100) {
                nodes {
                  sku price compareAtPrice barcode taxable inventoryPolicy
                  selectedOptions { name value }
                }
                pageInfo { hasNextPage endCursor }
              }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
        "#,
        json!({}),
        "products",
    )?;

    Ok(Value::Array(products))
}

/// Legacy per-record import (used with `--no-bulk`): one `productSet` GraphQL
/// call per product, best-effort skipping on userErrors.
fn import_legacy(client: &ShopifyClient, products: &[ProductRecord]) -> Result<()> {
    for p in products {
        let variant_count = p.variants.nodes.len();
        let input = build_product_set_input(p);
        let result = client.graphql(PRODUCT_SET, json!({ "input": input }))?;
        let payload = &result["productSet"];

        if let Some(errors) = payload["userErrors"].as_array()
            && !errors.is_empty()
        {
            println!("  skipped {}: {}", p.handle, payload["userErrors"]);
            continue;
        }
        println!("  created {} ({} variant(s))", p.handle, variant_count);
    }
    Ok(())
}

/// Locate the `productSet` payload inside one bulk-mutation result line. Bulk
/// results wrap each line's mutation output in `data`; fall back to the bare
/// payload if that wrapper is ever absent.
fn product_set_payload(result: &Value) -> &Value {
    if result.get("data").is_some() {
        &result["data"]["productSet"]
    } else {
        &result["productSet"]
    }
}

/// Translate a `ProductRecord` (read shape) into `ProductSetInput` (write shape).
/// Pure — no network — so it's unit-testable. The two interesting mappings:
///   * `options[].optionValues[].name` → `productOptions[].values[].name`
///   * a variant's `selectedOptions {name,value}` → `optionValues {optionName,name}`
fn build_product_set_input(p: &ProductRecord) -> Value {
    let product_options: Vec<Value> = p
        .options
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|o| {
            json!({
                "name": o.name,
                "values": o
                    .option_values
                    .iter()
                    .map(|v| json!({ "name": v.name }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    let variants: Vec<Value> = p
        .variants
        .nodes
        .iter()
        .map(|v| {
            json!({
                "price": v.price,
                "compareAtPrice": v.compare_at_price,
                "sku": v.sku,
                "barcode": v.barcode,
                "taxable": v.taxable,
                "inventoryPolicy": v.inventory_policy,
                "optionValues": v
                    .selected_options
                    .iter()
                    .map(|s| json!({ "optionName": s.name, "name": s.value }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    json!({
        "handle": p.handle,
        "title": p.title,
        "status": p.status,
        "descriptionHtml": p.description_html,
        "productType": p.product_type,
        "vendor": p.vendor,
        "tags": p.tags,
        "productOptions": product_options,
        "variants": variants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_options_and_variant_selected_options() {
        let record: ProductRecord = serde_json::from_value(json!({
            "handle": "tee",
            "title": "Tee",
            "status": "ACTIVE",
            "options": [{ "name": "Size", "optionValues": [{ "name": "S" }, { "name": "M" }] }],
            "variants": { "nodes": [
                {
                    "sku": "TEE-S",
                    "price": "19.99",
                    "compareAtPrice": null,
                    "selectedOptions": [{ "name": "Size", "value": "S" }]
                }
            ] }
        }))
        .unwrap();

        let input = build_product_set_input(&record);

        assert_eq!(input["handle"], "tee");
        // options → productOptions [{ name, values:[{name}] }]
        assert_eq!(input["productOptions"][0]["name"], "Size");
        assert_eq!(input["productOptions"][0]["values"][1]["name"], "M");
        // variant price + sku carried through
        assert_eq!(input["variants"][0]["price"], "19.99");
        assert_eq!(input["variants"][0]["sku"], "TEE-S");
        // selectedOptions {name,value} → optionValues {optionName,name}
        assert_eq!(
            input["variants"][0]["optionValues"][0]["optionName"],
            "Size"
        );
        assert_eq!(input["variants"][0]["optionValues"][0]["name"], "S");
    }
}
