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
        /// Resource type: products, discounts, or metaobjects
        resource: String,
        #[arg(short, long)]
        store: Option<String>,
        /// Output file (defaults to <resource>.json)
        #[arg(short, long)]
        out: Option<PathBuf>,
    },

    /// Import a resource into a store from a JSON file
    Import {
        /// Resource type: products, discounts, or metaobjects
        resource: String,
        /// The JSON file produced by `shoptools export`
        #[arg(short, long)]
        file: PathBuf,
        #[arg(short, long)]
        store: Option<String>,
        /// Plan and print changes without writing anything
        #[arg(long)]
        dry_run: bool,
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
        /// The offline Admin API access token (or set shoptools_TOKEN)
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
