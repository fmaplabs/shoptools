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
        // TODO(you): query discounts and return them as a JSON array.
        let _ = client;
        todo!("export discounts")
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        // TODO(you): create each discount (or print in dry_run).
        let _ = (client, data, dry_run);
        todo!("import discounts")
    }
}
