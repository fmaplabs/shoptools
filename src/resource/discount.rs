//! Discounts. Implement after products — the shape is the same, the GraphQL
//! differs (discountNodes / discountCodeBasicCreate, etc.).

use anyhow::Result;
use serde_json::Value;

use super::Resource;
use crate::client::ShopifyClient;

pub struct Discount;

impl Resource for Discount {
    fn name(&self) -> &'static str {
        "discounts"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
        let data = client.graphql(
            "query { discountNodes(first: 50){nodes {id}}}",
            serde_json::json!({}),
        )?;
        Ok(data["discounts"]["nodes"].clone())
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        // TODO(you): create each discount (or print in dry_run).
        let _ = (client, data, dry_run);
        todo!("import discounts")
    }
}
