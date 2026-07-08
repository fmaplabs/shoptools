//! Command-line interface definitions.
//!
//! This module contains ONLY the shape of the CLI — the structs and enums that
//! `clap` turns into a parser. No business logic lives here. Each field's `///`
//! doc comment becomes its `--help` text automatically.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Top-level parser. `Cli::parse()` reads `std::env::args()` and fills this in,
/// or prints help/errors and exits.
#[derive(Parser)]
#[command(
    name = "shoptools",
    version,
    about = "A personal CLI for Shopify store development"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// The top-level verbs. Each variant's fields are its arguments/flags.
#[derive(Subcommand)]
pub enum Command {
    /// Manage store credentials (no network calls)
    Store {
        #[command(subcommand)]
        command: StoreCommand,
    },

    /// Run a raw Admin GraphQL query against a store
    Query {
        /// The GraphQL query string, e.g. '{ shop { name } }'
        query: String,
        /// Which configured store to use (defaults to the default store)
        #[arg(short, long)]
        store: Option<String>,
        /// Print raw JSON instead of a summary
        #[arg(long)]
        json: bool,
    },

    /// Export a resource from a store to a JSON file
    Export {
        /// Resource type: products, discounts, metaobjects, customers,
        /// delivery_profiles, giftcards, store_credit
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        resource: Option<String>,
        /// Export all resource types
        #[arg(short, long)]
        all: bool,
        #[arg(short, long)]
        store: Option<String>,
        /// Output file (defaults to <resource>.json)
        #[arg(short, long, conflicts_with = "all")]
        out: Option<PathBuf>,
        /// Output directory for --all (defaults to shoptools_exports)
        // `requires = "all"` wouldn't fire here: bool flags carry an implicit
        // default, which clap counts as present. Conflicting with `resource`
        // is equivalent, since exactly one of `resource`/`--all` is required.
        #[arg(short = 'd', long, conflicts_with = "resource")]
        dir: Option<PathBuf>,
        /// Force the legacy paginated path instead of the Bulk Operations API
        #[arg(long)]
        no_bulk: bool,
    },

    /// Import a resource into a store from a JSON file
    Import {
        /// Resource type: products, discounts, metaobjects, customers,
        /// delivery_profiles, giftcards, store_credit
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        resource: Option<String>,
        /// Import all resource types
        #[arg(short, long)]
        all: bool,
        /// The JSON file produced by `shoptools export`
        #[arg(short, long, required_unless_present = "all", conflicts_with = "all")]
        file: Option<PathBuf>,
        /// Input directory for --all (defaults to shoptools_exports)
        // See Export::dir for why this isn't `requires = "all"`.
        #[arg(short = 'd', long, conflicts_with = "resource")]
        dir: Option<PathBuf>,
        #[arg(short, long)]
        store: Option<String>,
        /// Plan and print changes without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Force the legacy per-record path instead of the Bulk Operations API
        #[arg(long)]
        no_bulk: bool,
    },

    /// Clone resources from one store into another
    Clone {
        /// Source store name
        #[arg(long)]
        from: String,
        /// Target store name
        #[arg(long)]
        to: String,
        /// Comma-separated resource types to copy (defaults to all)
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Plan and print changes without writing anything
        #[arg(long)]
        dry_run: bool,
    },
}

/// Sub-verbs under `shoptools store …`.
#[derive(Subcommand)]
pub enum StoreCommand {
    /// Add or update a store credential
    Add {
        /// A short name you'll refer to this store by, e.g. acme-dev
        name: String,
        /// The myshopify.com domain, e.g. acme-dev.myshopify.com
        #[arg(short, long)]
        shop: String,
        /// The offline Admin API access token
        #[arg(short, long)]
        token: Option<String>,
    },
    /// List configured stores (the default is marked with *)
    List,
    /// Set the default store
    Use {
        /// Name of the store to make default
        name: String,
    },
    /// Remove a store
    Remove {
        /// Name of the store to remove
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("shoptools").chain(args.iter().copied()))
    }

    #[test]
    fn export_all_parses() {
        assert!(parse(&["export", "-a"]).is_ok());
        assert!(parse(&["export", "--all", "-d", "somewhere"]).is_ok());
    }

    #[test]
    fn export_single_resource_parses() {
        assert!(parse(&["export", "products"]).is_ok());
        assert!(parse(&["export", "products", "-o", "x.json"]).is_ok());
    }

    #[test]
    fn export_requires_resource_or_all() {
        assert!(parse(&["export"]).is_err());
    }

    #[test]
    fn export_all_conflicts_with_resource_and_out() {
        assert!(parse(&["export", "-a", "products"]).is_err());
        assert!(parse(&["export", "-a", "-o", "x.json"]).is_err());
    }

    #[test]
    fn export_dir_requires_all() {
        assert!(parse(&["export", "products", "-d", "somewhere"]).is_err());
    }

    #[test]
    fn import_all_parses_without_file() {
        assert!(parse(&["import", "-a"]).is_ok());
        assert!(parse(&["import", "-a", "-d", "somewhere", "--dry-run"]).is_ok());
    }

    #[test]
    fn import_single_resource_requires_file() {
        assert!(parse(&["import", "products"]).is_err());
        assert!(parse(&["import", "products", "-f", "products.json"]).is_ok());
    }

    #[test]
    fn import_all_conflicts_with_resource_and_file() {
        assert!(parse(&["import", "-a", "products"]).is_err());
        assert!(parse(&["import", "-a", "-f", "x.json"]).is_err());
    }
}
