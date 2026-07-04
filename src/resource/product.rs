//! Products. Implement this resource FIRST — it's the most familiar, and once
//! `export` works you can `shopli export products` end-to-end.

use anyhow::{Context, Result};
use serde::Deserialize;
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
#[derive(Debug, Deserialize)]
struct ProductRecord {
    title: String,
    handle: String,
    status: String,
}

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

            // TODO(you): send a productCreate mutation for `product` via
            // client.graphql(...). Get --dry-run right first, then wire this up.
            let _ = client;
            todo!("call productCreate for {}", product.title);
        }
        Ok(())
    }
}
