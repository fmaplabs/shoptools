//! The Shopify Admin GraphQL client. ⭐ YOUR FIRST TASK ⭐
//!
//! Implement `ShopifyClient::new` and `ShopifyClient::graphql`. When they work,
//! `shopli query '{ shop { name } }'` will make a real Admin API call. Use
//! `config.rs` as your style reference (Result, `?`, `.context(...)`).
//!
//! We use the *blocking* reqwest client: one request at a time, no async/await.
//! (Blocking reqwest spins up its own runtime internally — you never see it.)

use anyhow::Result;
use serde_json::Value;

use crate::config::StoreCredential;

/// The Admin API version to target. Shopify versions the API by calendar
/// quarter; bump this as needed. https://shopify.dev/docs/api/usage/versioning
pub const API_VERSION: &str = "2025-07";

/// A client bound to one store. `token` and `http` are unused until you fill in
/// the methods below — hence the temporary `allow(dead_code)`; delete it once
/// they're wired up and the compiler stops complaining.
#[allow(dead_code)]
pub struct ShopifyClient {
    shop: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl ShopifyClient {
    /// Build a client from resolved credentials.
    pub fn new(cred: StoreCredential) -> Result<ShopifyClient> {
        // TODO(you): build a blocking HTTP client and stash the credentials.
        //
        //   let http = reqwest::blocking::Client::builder()
        //       .user_agent("shopli/0.1")
        //       .build()
        //       .context("building HTTP client")?;   // needs `use anyhow::Context;`
        //   Ok(ShopifyClient { shop: cred.shop, token: cred.token, http })
        let _ = cred;
        todo!("construct the ShopifyClient")
    }

    /// The Admin GraphQL endpoint for this shop, e.g.
    /// `https://acme.myshopify.com/admin/api/2025-07/graphql.json`.
    fn endpoint(&self) -> String {
        format!("https://{}/admin/api/{}/graphql.json", self.shop, API_VERSION)
    }

    /// POST a GraphQL `query` (with `variables`) and return its `data` object.
    pub fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let url = self.endpoint();

        // TODO(you): perform the request and unwrap the response.
        //
        //   let body = serde_json::json!({ "query": query, "variables": variables });
        //   let resp = self.http
        //       .post(&url)
        //       .header("X-Shopify-Access-Token", &self.token)
        //       .json(&body)
        //       .send()
        //       .context("sending request to Shopify")?;
        //
        //   let value: Value = resp.json().context("decoding Shopify response")?;
        //
        //   // GraphQL returns HTTP 200 even for query errors, so check the body:
        //   if !value["errors"].is_null() {
        //       anyhow::bail!("GraphQL errors: {}", value["errors"]);
        //   }
        //   Ok(value["data"].clone())
        let _ = (url, query, variables);
        todo!("perform the GraphQL request")
    }
}
