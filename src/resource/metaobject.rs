//! Metaobjects (and metafields). The trickiest resource: *definitions* must be
//! cloned before *values*, and other resources may reference these — which is
//! why `resource::all` lists this first. Save it for last.

use anyhow::Result;
use serde_json::Value;

use super::Resource;
use crate::client::ShopifyClient;

pub struct Metaobject;

impl Resource for Metaobject {
    fn name(&self) -> &'static str {
        "metaobjects"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
        // TODO(you): export metaobject *definitions* and their *values*.
        // Consider returning both so `import` can recreate definitions first.
        let _ = client;
        todo!("export metaobjects")
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        // TODO(you): create definitions first, then values (or print in dry_run).
        let _ = (client, data, dry_run);
        todo!("import metaobjects")
    }
}
