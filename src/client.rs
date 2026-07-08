//! Implement `ShopifyClient::new` and `ShopifyClient::graphql`. When they work,
//! `shoptools query '{ shop { name } }'` will make a real Admin API call. Use
//! `config.rs` as your style reference (Result, `?`, `.context(...)`).
//!
//! We use the *blocking* reqwest client: one request at a time, no async/await.
//! (Blocking reqwest spins up its own runtime internally — you never see it.)

use std::time::Duration;

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
            .user_agent("shoptools/0.1")
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

    /// How many times `graphql` retries a request Shopify rejected with a
    /// THROTTLED error before giving up.
    const MAX_THROTTLE_RETRIES: u32 = 5;

    /// POST a GraphQL `query` (with `variables`) and return its `data` object.
    ///
    /// The Admin API rate limit is a cost-based leaky bucket. When we drain it,
    /// Shopify returns an HTTP 200 whose *body* carries a THROTTLED error (not a
    /// 429), so we detect it after decoding and retry the same request with
    /// exponential backoff. Every OTHER error still bails immediately.
    pub fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let url = self.endpoint();
        let body = serde_json::json!({ "query": query, "variables": variables });

        let mut attempt = 0;
        loop {
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
                if is_throttled(&value["errors"]) && attempt < Self::MAX_THROTTLE_RETRIES {
                    // Back off 1s, 2s, 4s, 8s, 16s … then retry the same request.
                    let wait = Duration::from_secs(1u64 << attempt);
                    eprintln!(
                        "  throttled by Shopify; retrying in {}s (attempt {}/{})",
                        wait.as_secs(),
                        attempt + 1,
                        Self::MAX_THROTTLE_RETRIES,
                    );
                    std::thread::sleep(wait);
                    attempt += 1;
                    continue;
                }
                anyhow::bail!("GraphQL errors: {}", value["errors"]);
            }

            // `value` gets thrown away, so clone `data` into owned memory.
            return Ok(value["data"].clone());
        }
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

    /// The underlying blocking HTTP client, for callers that must talk to a
    /// non-Shopify endpoint (e.g. the pre-signed staged-upload target for bulk
    /// operations). Requests made through this bypass the GraphQL/throttle logic
    /// above and carry no `X-Shopify-Access-Token` header unless the caller adds
    /// one. Keep this narrow — `graphql` is the right entry point for the Admin API.
    pub fn http(&self) -> &reqwest::blocking::Client {
        &self.http
    }

    /// Download `url` with a plain unauthenticated GET and return the body as text.
    ///
    /// Bulk-operation result files live at pre-signed URLs that already encode
    /// their own authorization, and Shopify's storage rejects the
    /// `X-Shopify-Access-Token` header — so this deliberately sends no auth.
    pub fn download(&self, url: &str) -> Result<String> {
        let resp = self
            .http
            .get(url)
            .send()
            .context("downloading bulk-operation result file")?;
        let resp = resp
            .error_for_status()
            .context("bulk-operation result download returned an error status")?;
        resp.text().context("reading bulk-operation result body")
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

    /// Resolve a customer EMAIL to this store's customer id (null → error).
    /// Email is the stable cross-store key for customers — the analog of a
    /// product/collection handle.
    pub fn resolve_customer(&self, email: &str) -> Result<String> {
        let data = self.graphql(
            "query($identifier: CustomerIdentifierInput!) { customerByIdentifier(identifier: $identifier) { id } }",
            serde_json::json!({ "identifier": { "emailAddress": email } }),
        )?;
        data["customerByIdentifier"]["id"]
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("target store has no customer with email '{email}'"))
    }

    /// Resolve a location NAME to this store's location id (null → error).
    /// Locations are physical store config (a handful at most), so we just page
    /// through them and match by name rather than adding a lookup query.
    pub fn resolve_location(&self, name: &str) -> Result<String> {
        let locations = self.paginate(
            r#"
            query Locations($cursor: String) {
              locations(first: 50, after: $cursor) {
                nodes { id name }
                pageInfo { hasNextPage endCursor }
              }
            }
            "#,
            serde_json::json!({}),
            "locations",
        )?;
        locations
            .iter()
            .find(|loc| loc["name"].as_str() == Some(name))
            .and_then(|loc| loc["id"].as_str())
            .map(str::to_string)
            .with_context(|| format!("target store has no location named '{name}'"))
    }
}

/// True if any of the top-level GraphQL `errors` is Shopify's cost-based throttle
/// signal (`extensions.code == "THROTTLED"`). A free function (not a method) so
/// it stays pure and unit-testable without a live client.
fn is_throttled(errors: &Value) -> bool {
    errors.as_array().is_some_and(|errs| {
        errs.iter()
            .any(|e| e["extensions"]["code"].as_str() == Some("THROTTLED"))
    })
}

#[cfg(test)]
mod tests {
    use super::is_throttled;
    use serde_json::json;

    #[test]
    fn detects_throttled_error() {
        let errors = json!([
            { "message": "Throttled", "extensions": { "code": "THROTTLED" } }
        ]);
        assert!(is_throttled(&errors));
    }

    #[test]
    fn ignores_non_throttle_errors() {
        let errors = json!([
            { "message": "Field 'foo' doesn't exist", "extensions": { "code": "undefinedField" } }
        ]);
        assert!(!is_throttled(&errors));
    }

    #[test]
    fn handles_missing_extensions_and_non_arrays() {
        assert!(!is_throttled(&json!([{ "message": "boom" }])));
        assert!(!is_throttled(&json!(null)));
        assert!(!is_throttled(&json!("not an array")));
    }
}
