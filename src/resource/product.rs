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

/// Follow-up query to page ONE product's variants when the inline `first: 100`
/// overflows. Keyed by product id (same idiom as `discount.rs`'s collections).
const PRODUCT_VARIANTS: &str = r#"
query ProductVariants($id: ID!, $cursor: String) {
  product(id: $id) {
    variants(first: 100, after: $cursor) {
      nodes {
        sku price compareAtPrice barcode taxable inventoryPolicy
        selectedOptions { name value }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
}
"#;

impl Resource for Product {
    fn name(&self) -> &'static str {
        "products"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
        let mut products = client.paginate(
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

        // Nested paging: if a product has more than 100 variants, refetch the
        // full list by product id and splice it back in.
        for p in &mut products {
            let truncated = p["variants"]["pageInfo"]["hasNextPage"].as_bool() == Some(true);
            if !truncated {
                continue;
            }
            let id = p["id"]
                .as_str()
                .context("product node missing id for variant paging")?
                .to_string();
            let all = client.paginate_nested(PRODUCT_VARIANTS, json!({ "id": id }), |d| {
                d["product"]["variants"].clone()
            })?;
            p["variants"] = json!({ "nodes": all });
        }

        Ok(Value::Array(products))
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        let products: Vec<ProductRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of products")?;

        println!("{} product(s) to import", products.len());

        for p in &products {
            let variant_count = p.variants.nodes.len();

            if dry_run {
                println!(
                    "  would create: {} ({}, {} variant(s))",
                    p.handle,
                    p.status.as_deref().unwrap_or("?"),
                    variant_count
                );
                continue;
            }

            let input = build_product_set_input(p);
            let result = client.graphql(PRODUCT_SET, json!({ "input": input }))?;
            let payload = &result["productSet"];

            // Best-effort per product: a duplicate handle (re-run) or bad data
            // skips just this product rather than aborting the whole import.
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
