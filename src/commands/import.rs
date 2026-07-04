//! `shopli import <resource> --file <f>` — push a JSON file into a store.
//!
//! Wiring is done; you implement the read + parse here, and the actual create
//! logic inside `resource/<type>.rs`. Honor `dry_run` (plan, don't write).

use std::path::Path;

use anyhow::Result;

use crate::client::ShopifyClient;
use crate::config;
use crate::resource;

pub fn run(resource_name: &str, file: &Path, store: Option<&str>, dry_run: bool) -> Result<()> {
    let res = resource::by_name(resource_name)?;
    let cred = config::resolve(store)?;
    let client = ShopifyClient::new(cred)?;

    // TODO(you): read `file`, parse it as JSON, then hand it to the resource.
    //   let text = std::fs::read_to_string(file)
    //       .with_context(|| format!("reading {}", file.display()))?;  // needs anyhow::Context
    //   let data: serde_json::Value = serde_json::from_str(&text)?;
    //   res.import(&client, &data, dry_run)?;
    //   Ok(())
    let _ = (file, dry_run, &res, &client);
    todo!("read the file and import it")
}
