//! Products. Implement this resource FIRST — it's the most familiar, and once
//! `export` works you can `shopli export products` end-to-end.

use anyhow::Result;
use serde_json::Value;

use super::Resource;
use crate::client::ShopifyClient;

/// A unit struct: no data, it just carries the `Resource` implementation.
pub struct Product;

impl Resource for Product {
    fn name(&self) -> &'static str {
        "products"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
        // TODO(you): query products and return them as a JSON array.
        // Start simple (first 50), then add cursor pagination once it works.
        //
        //   let data = client.graphql(
        //       "query { products(first: 50) { nodes { id title handle status } } }",
        //       serde_json::json!({}),
        //   )?;
        //   Ok(data["products"]["nodes"].clone())
        let _ = client;
        todo!("export products")
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        // TODO(you): for each product in `data`, call the productCreate mutation
        // (or, when `dry_run`, just print what you would create).
        let _ = (client, data, dry_run);
        todo!("import products")
    }
}
