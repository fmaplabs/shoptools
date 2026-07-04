//! Products. Implement this resource FIRST — it's the most familiar, and once
//! `export` works you can `shopli export products` end-to-end.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Resource;
use crate::client::ShopifyClient;

/// The resource *handler*. A unit struct — it holds no data; it just implements
/// `Resource` so the command layer can call `export`/`import` on it. This is the
/// type stored as `Box<dyn Resource>` in `resource::all()` / `by_name()`, which
/// is why it must be constructible with no fields.
pub struct Product;

/// One product's data, deserialized from the exported JSON. This is the typed
/// DTO that gives us dot-notation (`record.title`) instead of the dynamic
/// `value["title"].as_str()` dance. Fields we don't list here (e.g. the source
/// `id`) are simply ignored by serde during deserialization.
#[derive(Debug, Deserialize, Serialize)]
struct ProductRecord {
    title: String,
    handle: String,
    status: String,
}

/// The productCreate mutation. Takes a `ProductCreateInput` and returns the new
/// product plus any business-validation `userErrors`. Shape verified against the
/// official Shopify Admin `2026-07` docs.
const PRODUCT_CREATE: &str = r#"
mutation CreateProduct($product: ProductCreateInput!) {
  productCreate(product: $product) {
    product { id title handle status }
    userErrors { field message }
  }
}
"#;

impl Resource for Product {
    fn name(&self) -> &'static str {
        "products"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
        let data = client.graphql(
            "query { products(first: 50) { nodes { id title handle status } } }",
            serde_json::json!({}),
        )?;
        Ok(data["products"]["nodes"].clone())
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        // Deserialize the whole JSON array into borrow records in one step.
        // serde builds a Vec<ProductRecord>, erroring if the shape is wrong.
        let products: Vec<ProductRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of products")?;

        println!("{} product(s) to import", products.len());

        // &products borrows the Vec without consuming it, so `products` stays
        // valid after the loop. (Consuming it — `for product in products` — would
        // MOVE the Vec into the loop and drop it once the loop ends.)
        // Because we borrow, each `product` is a &ProductRecord (a shared borrow),
        // not a value owned by the loop.
        for product in &products {
            // Dot notation now — every field access is checked at compile time.
            if dry_run {
                println!(
                    "  would create: {} ({}, {})",
                    product.title, product.handle, product.status
                );
                continue;
            }

            // Serialize the typed record into the mutation input. Because
            // ProductRecord derives Serialize (the mirror of Deserialize), the
            // same struct that parsed the file becomes the GraphQL input —
            // title/handle/status map straight onto ProductCreateInput.
            let variables = serde_json::json!({
                "product": serde_json::to_value(product).context("serializing product input")?,
            });

            let result = client.graphql(PRODUCT_CREATE, variables)?;

            // Mutations have TWO error channels. `client.graphql` already bailed on
            // a top-level `errors` array (bad query / auth / throttle). Business-rule
            // failures — e.g. a handle already in use — arrive in `userErrors`.
            let payload = &result["productCreate"];
            if let Some(errors) = payload["userErrors"].as_array() {
                if !errors.is_empty() {
                    anyhow::bail!(
                        "could not create '{}': {}",
                        product.handle, payload["userErrors"]
                    );
                }
            }

            let created = &payload["product"];
            println!(
                "  created {} ({})",
                created["title"].as_str().unwrap_or("?"),
                created["id"].as_str().unwrap_or("?"),
            );
        }
        Ok(())
    }
}
