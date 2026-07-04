//! shopli — a personal CLI for Shopify store development.
//!
//! This is the **crate root** of the library. The `mod` lines below tell the
//! compiler which modules exist and where to find them:
//!   - `mod config;`  → looks for `src/config.rs`
//!   - `mod commands;`→ looks for `src/commands/mod.rs` (a folder module)
//!
//! The modules form a dependency stack, lowest first:
//!   config  (credentials, no network)
//!     └── client   (HTTP + GraphQL, needs a credential)
//!           └── resource (per-type export/import, needs a client)
//!                 └── commands (the user-facing verbs, tie it all together)
//!   cli     (argument parsing; depended on by `run` below)

mod cli;
mod client;
mod commands;
mod config;
mod resource;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};

/// Parse the command line and dispatch to the matching command handler.
///
/// The `match` is *exhaustive*: if you add a variant to `Command`, this won't
/// compile until you handle it here. That compiler nudge is a feature.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Store { command } => commands::store::run(command),
        Command::Query { query, store, json } => {
            commands::query::run(&query, store.as_deref(), json)
        }
        Command::Export { resource, store, out } => {
            commands::export::run(&resource, store.as_deref(), out)
        }
        Command::Import { resource, file, store, dry_run } => {
            commands::import::run(&resource, &file, store.as_deref(), dry_run)
        }
        Command::Clone { from, to, only, dry_run } => {
            commands::clone::run(&from, &to, &only, dry_run)
        }
    }
}
