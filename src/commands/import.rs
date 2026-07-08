//! `shoptools import <resource> --file <f>` — push a JSON file into a store.
//!
//! Wiring is done; you implement the read + parse here, and the actual create
//! logic inside `resource/<type>.rs`. Honor `dry_run` (plan, don't write).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::client::ShopifyClient;
use crate::config::{self, Role};
use crate::resource;

pub fn run(
    resource_name: &str,
    file: &Path,
    store: Option<&str>,
    dry_run: bool,
    no_bulk: bool,
) -> Result<()> {
    let res = resource::by_name(resource_name)?;
    // Import writes *into* a store: target credentials.
    let cred = config::resolve(store, Role::Target)?;
    let client = ShopifyClient::new(cred)?;

    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let data: serde_json::Value = serde_json::from_str(&text)?;
    res.import(&client, &data, dry_run, no_bulk)?;
    Ok(())
}

/// Import every known resource type from `dir` (one `<resource>.json` each),
/// in dependency order. A missing file is skipped with a warning; a file that
/// fails to parse or import aborts the run.
pub fn run_all(
    store: Option<&str>,
    dir: Option<PathBuf>,
    dry_run: bool,
    no_bulk: bool,
) -> Result<()> {
    let dir = dir.unwrap_or_else(|| PathBuf::from("shoptools_exports"));
    let cred = config::resolve(store, Role::Target)?;
    let client = ShopifyClient::new(cred)?;

    for res in resource::all() {
        let path = dir.join(format!("{}.json", res.name()));
        if !path.exists() {
            eprintln!("skipping {}: {} not found", res.name(), path.display());
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let data: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        res.import(&client, &data, dry_run, no_bulk)?;
    }
    Ok(())
}
