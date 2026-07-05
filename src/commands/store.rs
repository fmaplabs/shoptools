//! `shoptools store …` — manage credentials in the config file. Fully working and
//! network-free. Read this next to `config.rs` to see how a parsed command turns
//! into config changes.

use anyhow::{Result, bail};

use crate::cli::StoreCommand;
use crate::config::{Config, StoreCredential};

/// Entry point called from `lib::run`. Dispatches to one small function per
/// sub-verb — the same match-on-an-enum pattern as the top-level dispatch.
pub fn run(command: StoreCommand) -> Result<()> {
    match command {
        StoreCommand::Add { name, shop, token } => add(name, shop, token),
        StoreCommand::List => list(),
        StoreCommand::Use { name } => use_store(name),
        StoreCommand::Remove { name } => remove(name),
    }
}

fn add(name: String, shop: String, token: Option<String>) -> Result<()> {
    // The credential env vars are role-scoped (SHOPIFY_SOURCE_* / SHOPIFY_TARGET_*)
    // and describe a data flow, not a named config entry — so there's no generic
    // token fallback here; `add` requires an explicit --token.
    let token = match token {
        Some(t) => t,
        None => bail!("no token given; pass --token"),
    };

    let mut config = Config::load()?;
    let is_first = config.stores.is_empty();
    config
        .stores
        .insert(name.clone(), StoreCredential { shop, token });
    // The first store you add becomes the default automatically.
    if is_first {
        config.default = Some(name.clone());
    }
    config.save()?;

    let note = if is_first { " (now the default)" } else { "" };
    println!("Saved store '{name}'{note}");
    Ok(())
}

fn list() -> Result<()> {
    let config = Config::load()?;
    if config.stores.is_empty() {
        println!("No stores configured. Add one with:");
        println!("  shoptools store add <name> --shop <domain> --token <tok>");
        return Ok(());
    }
    // `&config.stores` iterates by reference so we don't consume the map.
    for (name, cred) in &config.stores {
        let marker = if config.default.as_deref() == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        println!("{marker} {name:<16} {}", cred.shop);
    }
    Ok(())
}

fn use_store(name: String) -> Result<()> {
    let mut config = Config::load()?;
    if !config.stores.contains_key(&name) {
        bail!("no store named '{name}'; run `shoptools store list` to see configured stores");
    }
    config.default = Some(name.clone());
    config.save()?;
    println!("Default store is now '{name}'");
    Ok(())
}

fn remove(name: String) -> Result<()> {
    let mut config = Config::load()?;
    // `.remove` returns the old value (Some) or None if it wasn't there.
    if config.stores.remove(&name).is_none() {
        bail!("no store named '{name}'");
    }
    // If we just removed the default, clear it.
    if config.default.as_deref() == Some(name.as_str()) {
        config.default = None;
    }
    config.save()?;
    println!("Removed store '{name}'");
    Ok(())
}
