//! `shoptools import <resource> --file <f>` — push a JSON file into a store.
//!
//! Wiring is done; you implement the read + parse here, and the actual create
//! logic inside `resource/<type>.rs`. Honor `dry_run` (plan, don't write).

use std::path::Path;

use anyhow::{Context, Result};

use crate::client::ShopifyClient;
use crate::config::{self, Role};
use crate::resource;

pub fn run(resource_name: &str, file: &Path, store: Option<&str>, dry_run: bool) -> Result<()> {
    let res = resource::by_name(resource_name)?;
    // Import writes *into* a store: target credentials.
    let cred = config::resolve(store, Role::Target)?;
    let client = ShopifyClient::new(cred)?;

    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let data: serde_json::Value = serde_json::from_str(&text)?;
    res.import(&client, &data, dry_run)?;
    Ok(())
}
