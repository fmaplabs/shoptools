//! `shoptools query <graphql>` — run a raw Admin GraphQL query.
//!
//! This handler is already wired to `config::resolve` and the client. Once you
//! implement `client.rs`, it will run up to the point where it prints — and
//! finishing that print (the final TODO) is your second small task.

use anyhow::Result;

use crate::client::ShopifyClient;
use crate::config;

pub fn run(query: &str, store: Option<&str>, json: bool) -> Result<()> {
    // 1. Which store? Resolve credentials (env vars or config file).
    let cred = config::resolve(store)?;
    // 2. Build a client for it.
    let client = ShopifyClient::new(cred)?;
    // 3. Send the query with no variables.
    let data = client.graphql(query, serde_json::json!({}))?;

    if json {
        println!("{data}");
    } else {
        // # is used for pretty printing the JSON output.
        // standard rust convention, not specific to serde
        println!("{data:#}");
    }
    Ok(())
}
