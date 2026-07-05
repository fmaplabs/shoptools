//! Credentials and the config file. This is the ONE fully-worked module — read
//! it to learn the idioms the stubs expect you to follow:
//!   - `#[derive(Serialize, Deserialize)]` to move structs ↔ TOML/JSON
//!   - `anyhow::Result` + `.context(...)` + the `?` operator for errors
//!   - borrowing (`&self`, `&str`) vs. owning (`String`, `.clone()`)
//!   - a pure, testable core (`Config::get`) separated from I/O and env vars
//!
//! Nothing here touches the network, which is exactly why it's a good place to
//! start reading.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One store's credentials. `derive` gives us: debug printing, cheap cloning,
/// and (de)serialization to any format serde supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreCredential {
    /// The myshopify.com domain, e.g. "acme-dev.myshopify.com".
    pub shop: String,
    /// The offline Admin API access token.
    pub token: String,
}

/// The entire config file: a set of named stores plus which one is default.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Name of the default store, used when `--store` is omitted.
    /// `skip_serializing_if` keeps the file tidy when there's no default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,

    /// Named stores. A `BTreeMap` keeps them sorted, so the file has stable,
    /// diff-friendly ordering. `#[serde(default)]` means "empty map if missing".
    #[serde(default)]
    pub stores: BTreeMap<String, StoreCredential>,
}

impl Config {
    /// Load the config from its default path, returning an empty config if the
    /// file doesn't exist yet (a fresh install is not an error).
    pub fn load() -> Result<Config> {
        Config::load_from(&config_path()?)
    }

    /// Load from a specific path. Split out from `load` so tests can point it at
    /// a temp file instead of your real `~/.config`.
    pub fn load_from(path: &Path) -> Result<Config> {
        match fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            // "file not found" is fine — you just have no stores yet.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Save the config to its default path, creating parent dirs as needed.
    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path()?)
    }

    /// Save to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// Look up a store by name, falling back to the default store when `name`
    /// is `None`. This is the pure, network-free core the network commands call.
    ///
    /// Returns a *borrow* (`&StoreCredential`); callers that need to keep it can
    /// `.clone()` it (see `commands::clone`).
    pub fn get(&self, name: Option<&str>) -> Result<&StoreCredential> {
        let name = match name {
            Some(n) => n,
            None => self.default.as_deref().context(
                "no store specified and no default set; run `shoptools store use <name>`",
            )?,
        };
        self.stores.get(name).with_context(|| {
            format!("no store named '{name}'; run `shoptools store list` to see configured stores")
        })
    }
}

/// The path to the config file. Honors `$shoptools_CONFIG` so you (and the tests)
/// can override it; otherwise it's `<os config dir>/shoptools/config.toml`
/// (e.g. `~/.config/shoptools/config.toml` on Linux).
pub fn config_path() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("shoptools_CONFIG") {
        return Ok(PathBuf::from(custom));
    }
    let dir = dirs::config_dir().context("could not determine an OS config directory")?;
    Ok(dir.join("shoptools").join("config.toml"))
}

/// Resolve the credentials to use for a network command, in priority order:
///   1. `shoptools_TOKEN` + `shoptools_SHOP` environment variables
///   2. the named store (or default store) from the config file
///
/// (A future `--token` flag would slot in above these.)
pub fn resolve(store_name: Option<&str>) -> Result<StoreCredential> {
    if let (Ok(token), Ok(shop)) = (
        std::env::var("shoptools_TOKEN"),
        std::env::var("shoptools_SHOP"),
    ) {
        return Ok(StoreCredential { shop, token });
    }
    Config::load()?.get(store_name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        let mut stores = BTreeMap::new();
        stores.insert(
            "dev".to_string(),
            StoreCredential {
                shop: "dev.myshopify.com".into(),
                token: "shpat_dev".into(),
            },
        );
        stores.insert(
            "prod".to_string(),
            StoreCredential {
                shop: "prod.myshopify.com".into(),
                token: "shpat_prod".into(),
            },
        );
        Config {
            default: Some("dev".to_string()),
            stores,
        }
    }

    #[test]
    fn get_named_store() {
        assert_eq!(
            sample().get(Some("prod")).unwrap().shop,
            "prod.myshopify.com"
        );
    }

    #[test]
    fn get_falls_back_to_default() {
        assert_eq!(sample().get(None).unwrap().token, "shpat_dev");
    }

    #[test]
    fn get_unknown_store_is_error() {
        assert!(sample().get(Some("staging")).is_err());
    }

    #[test]
    fn get_with_no_default_is_error() {
        assert!(Config::default().get(None).is_err());
    }

    #[test]
    fn round_trips_through_toml() {
        let text = toml::to_string_pretty(&sample()).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.default.as_deref(), Some("dev"));
        assert_eq!(parsed.stores.len(), 2);
        assert_eq!(parsed.stores["prod"].shop, "prod.myshopify.com");
    }
}
