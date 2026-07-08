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

    write_json(&data, &path, res.name())
}

/// Export every known resource type into `dir` (one `<resource>.json` each).
pub fn run_all(store: Option<&str>, dir: Option<PathBuf>, no_bulk: bool) -> Result<()> {
    let dir = dir.unwrap_or_else(|| PathBuf::from("shoptools_exports"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;

    let cred = config::resolve(store, Role::Source)?;
    let client = ShopifyClient::new(cred)?;

    for res in resource::all() {
        let data = res.export(&client, no_bulk)?;
        let path = dir.join(format!("{}.json", res.name()));
        write_json(&data, &path, res.name())?;
    }
    Ok(())
}

fn write_json(data: &serde_json::Value, path: &std::path::Path, name: &str) -> Result<()> {
    let text = serde_json::to_string_pretty(data)?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    println!("Exported {} to {}", name, path.display());
    Ok(())
}
