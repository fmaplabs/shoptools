//! `shoptools export <resource>` — pull a resource from a store into a JSON file.
//!
//! The wiring (pick resource → resolve store → build client → export) is done;
//! your job is the file write at the end, plus the actual export logic inside
//! `resource/<type>.rs`.
use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::client::ShopifyClient;
use crate::config::{self, Role};
use crate::resource;

pub fn run(
    resource_name: &str,
    store: Option<&str>,
    out: Option<PathBuf>,
    no_bulk: bool,
) -> Result<()> {
    let res = resource::by_name(resource_name)?;
    // Export reads *from* a store: source credentials.
    let cred = config::resolve(store, Role::Source)?;
    let client = ShopifyClient::new(cred)?;

    // Calls into resource/<type>.rs — implement `export` there.
    let data = res.export(&client, no_bulk)?;

    // Default the output filename to "<resource>.json".
    let path = out.unwrap_or_else(|| PathBuf::from(format!("{}.json", res.name())));

    // TODO(you): write `data` to `path` as pretty JSON, then print a confirmation.
    let text = serde_json::to_string_pretty(&data)?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?; // needs anyhow::Context
    println!("Exported {} to {}", res.name(), path.display());
    Ok(())
}
