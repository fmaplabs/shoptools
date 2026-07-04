//! The Shopify Admin GraphQL client. ⭐ YOUR FIRST TASK ⭐
//!
//! Implement `ShopifyClient::new` and `ShopifyClient::graphql`. When they work,
//! `shopli query '{ shop { name } }'` will make a real Admin API call. Use
//! `config.rs` as your style reference (Result, `?`, `.context(...)`).
//!
//! We use the *blocking* reqwest client: one request at a time, no async/await.
//! (Blocking reqwest spins up its own runtime internally — you never see it.)

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config::StoreCredential;

/// The Admin API version to target. Shopify versions the API by calendar
/// quarter; bump this as needed. https://shopify.dev/docs/api/usage/versioning
pub const API_VERSION: &str = "2026-07";

/// A client bound to one store. `token` and `http` are unused until you fill in
/// the methods below — hence the temporary `allow(dead_code)`; delete it once
/// they're wired up and the compiler stops complaining.
pub struct ShopifyClient {
    shop: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl ShopifyClient {
    /// Build a client from resolved credentials.
    pub fn new(cred: StoreCredential) -> Result<ShopifyClient> {
        let http = reqwest::blocking::Client::builder()
            .user_agent("shopli/0.1")
            .build()
            .context("building HTTP client")?; // needs `use anyhow::Context;`

        Ok(ShopifyClient {
            shop: cred.shop,
            token: cred.token,
            http,
        })
    }

    /// The Admin GraphQL endpoint for this shop, e.g.
    /// `https://acme.myshopify.com/admin/api/2025-07/graphql.json`.
    fn endpoint(&self) -> String {
        format!(
            "https://{}/admin/api/{}/graphql.json",
            self.shop, API_VERSION
        )
    }

    /// POST a GraphQL `query` (with `variables`) and return its `data` object.
    pub fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let url = self.endpoint();

        let body = serde_json::json!({ "query": query, "variables": variables });
        let resp = self
            .http
            .post(&url)
            .header("X-Shopify-Access-Token", &self.token)
            .json(&body)
            .send()
            .context("sending request to Shopify")?;

        let value: Value = resp.json().context("decoding Shopify response")?;

        // GraphQL returns HTTP 200 even for query errors, so check the body:
        if !value["errors"].is_null() {
            anyhow::bail!("GraphQL errors: {}", value["errors"]);
        }
        // graphql gets thrown away, so we have to clone the return value into our "owned" memory space so that we can use it.

        Ok(value["data"].clone())
    }

    /// Run a paginated top-level connection query, collecting every node.
    ///
    /// `query` must accept a `$cursor: String` variable, pass it as
    /// `after: $cursor`, and select `pageInfo { hasNextPage endCursor }` next to
    /// `nodes`. `connection` names the top-level connection field in the
    /// response, e.g. "products" or "metaobjectDefinitions".
    pub fn paginate(&self, query: &str, variables: Value, connection: &str) -> Result<Vec<Value>> {
        // A top-level connection is just a nested one whose path is a single
        // field, so reuse the general helper.
        let field = connection.to_string();
        self.paginate_nested(query, variables, move |data| data[field.as_str()].clone())
    }

    /// Run a paginated query for a connection at ANY depth, collecting every
    /// node. `extract` locates the connection object inside the response `data`
    /// (e.g. `|d| d["discountNode"]["discount"]...["collections"].clone()`). The
    /// query must accept `$cursor: String` and select
    /// `pageInfo { hasNextPage endCursor }` next to `nodes`.
    pub fn paginate_nested(
        &self,
        query: &str,
        variables: Value,
        extract: impl Fn(&Value) -> Value,
    ) -> Result<Vec<Value>> {
        let mut all: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            // Start each request where the previous page ended. On the first
            // pass `cursor` is None, which serializes to `null` = "from the start".
            let mut vars = variables.clone();
            vars["cursor"] = serde_json::json!(cursor);

            let data = self.graphql(query, vars)?;
            let conn = extract(&data);

            if let Some(nodes) = conn["nodes"].as_array() {
                all.extend(nodes.iter().cloned());
            }

            // Advance only if there IS a next page and a cursor to advance to;
            // otherwise stop. Requiring both guards against ever looping forever.
            let page = &conn["pageInfo"];
            let has_next = page["hasNextPage"].as_bool().unwrap_or(false);
            match page["endCursor"].as_str() {
                Some(end) if has_next => cursor = Some(end.to_string()),
                _ => break,
            }
        }

        Ok(all)
    }

    /// Resolve a product HANDLE to this store's product id (null → error).
    pub fn resolve_product(&self, handle: &str) -> Result<String> {
        let data = self.graphql(
            "query($identifier: ProductIdentifierInput!) { productByIdentifier(identifier: $identifier) { id } }",
            serde_json::json!({ "identifier": { "handle": handle } }),
        )?;
        data["productByIdentifier"]["id"]
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("target store has no product with handle '{handle}'"))
    }

    /// Resolve a collection HANDLE to this store's collection id (null → error).
    pub fn resolve_collection(&self, handle: &str) -> Result<String> {
        let data = self.graphql(
            "query($handle: String!) { collectionByHandle(handle: $handle) { id } }",
            serde_json::json!({ "handle": handle }),
        )?;
        data["collectionByHandle"]["id"]
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("target store has no collection with handle '{handle}'"))
    }

    /// Resolve a metaobject TYPE+HANDLE to this store's metaobject id (null → error).
    pub fn resolve_metaobject(&self, type_name: &str, handle: &str) -> Result<String> {
        let data = self.graphql(
            "query($handle: MetaobjectHandleInput!) { metaobjectByHandle(handle: $handle) { id } }",
            serde_json::json!({ "handle": { "type": type_name, "handle": handle } }),
        )?;
        data["metaobjectByHandle"]["id"]
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("target store has no metaobject '{type_name}/{handle}'"))
    }
}
