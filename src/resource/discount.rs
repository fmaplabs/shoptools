//! Discounts. Two things make this the trickiest resource so far:
//!   1. `discount` is a GraphQL *union* — you read it with inline fragments.
//!   2. The shape you READ (export) is NOT the shape you WRITE (create). We
//!      capture the rich read shape here and translate it into the create input
//!      inside `import`. That translation is the interesting part.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Resource;
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

impl Resource for Discount {
    fn name(&self) -> &'static str {
        "discounts"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
        let data = client.graphql(
            r#"
            query {
              discountNodes(first: 50) {
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
                        items { __typename ... on AllDiscountItems { allItems } }
                      }
                    }
                    ... on DiscountAutomaticBasic {
                      title
                      status
                      startsAt
                      endsAt
                      customerGets {
                        value { __typename ... on DiscountPercentage { percentage } }
                        items { __typename ... on AllDiscountItems { allItems } }
                      }
                    }
                    ... on DiscountCodeFreeShipping { title status }
                    ... on DiscountAutomaticFreeShipping { title status }
                  }
                }
              }
            }
            "#,
            serde_json::json!({}),
        )?;
        // export's ONLY job: read and return the JSON array. No deserialize here.
        Ok(data["discountNodes"]["nodes"].clone())
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        // NOW we deserialize — this is import's job, and dry_run lives here.
        let nodes: Vec<DiscountNodeRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of discount nodes")?;

        println!("{} discount(s) to import", nodes.len());

        for node in &nodes {
            let d = &node.discount;
            let title = d.title.as_deref().unwrap_or("(untitled)");

            if dry_run {
                match percentage_of(d) {
                    Some(p) => {
                        println!("  would create: {title} [{}] — {}% off", d.typename, p * 100.0)
                    }
                    None => println!("  would create: {title} [{}]", d.typename),
                }
                continue;
            }

            // Dispatch on the concrete type — this is where __typename earns its
            // keep: different discount kinds use different create mutations.
            match d.typename.as_str() {
                "DiscountAutomaticBasic" => create_automatic_basic(client, d)?,
                "DiscountCodeBasic" => create_code_basic(client, d)?,
                other => bail!("cloning '{title}' isn't supported yet (type: {other})"),
            }
        }
        Ok(())
    }
}

/// Pull the percentage (0.0–1.0) out of the raw `customerGets`, if this is a
/// percentage discount. Returns None for fixed-amount or free-shipping discounts.
fn percentage_of(d: &DiscountRecord) -> Option<f64> {
    d.customer_gets.as_ref()?["value"]["percentage"].as_f64()
}

/// Translate the READ shape of `customerGets` into the CREATE input shape.
///   read:  { value: { __typename, percentage }, items: { __typename, allItems } }
///   write: { value: { percentage },            items: { all: true } }
fn build_customer_gets(d: &DiscountRecord) -> Result<Value> {
    let percentage =
        percentage_of(d).context("only percentage-value discounts are supported for now")?;

    let all_items = d
        .customer_gets
        .as_ref()
        .and_then(|cg| cg["items"]["allItems"].as_bool())
        .unwrap_or(false);
    if !all_items {
        bail!("only discounts that apply to *all* items are supported for now");
    }

    Ok(serde_json::json!({
        "value": { "percentage": percentage },
        "items": { "all": true },
    }))
}

fn create_automatic_basic(client: &ShopifyClient, d: &DiscountRecord) -> Result<()> {
    let input = serde_json::json!({
        "title": d.title,
        "startsAt": d.starts_at,
        "endsAt": d.ends_at,
        "customerGets": build_customer_gets(d)?,
    });
    let result = client.graphql(
        AUTOMATIC_BASIC_CREATE,
        serde_json::json!({ "automaticBasicDiscount": input }),
    )?;
    check_user_errors(&result["discountAutomaticBasicCreate"], d)?;
    println!("  created {} (automatic)", d.title.as_deref().unwrap_or("?"));
    Ok(())
}

fn create_code_basic(client: &ShopifyClient, d: &DiscountRecord) -> Result<()> {
    // We only handle "all customers" code discounts; targeting specific
    // customers/segments would need those IDs to already exist in the TARGET store.
    let all_customers = d
        .customer_selection
        .as_ref()
        .and_then(|s| s["__typename"].as_str())
        == Some("DiscountCustomerAll");
    if !all_customers {
        bail!("only 'all customers' code discounts are supported for now");
    }

    let code = d
        .codes
        .as_ref()
        .and_then(|c| c.nodes.first())
        .map(|n| n.code.clone())
        .context("code discount is missing its code")?;

    let input = serde_json::json!({
        "title": d.title,
        "code": code,
        "startsAt": d.starts_at,
        "endsAt": d.ends_at,
        "customerSelection": { "all": true },
        "customerGets": build_customer_gets(d)?,
    });
    let result = client.graphql(
        CODE_BASIC_CREATE,
        serde_json::json!({ "basicCodeDiscount": input }),
    )?;
    check_user_errors(&result["discountCodeBasicCreate"], d)?;
    println!("  created {} (code: {code})", d.title.as_deref().unwrap_or("?"));
    Ok(())
}

/// Both create mutations return `userErrors`; check them the same way.
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
