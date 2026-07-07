//! Customers — the FOUNDATION resource. Gift cards, store credit, and (later)
//! orders all reference a customer, and across stores that reference is resolved
//! by **email** (see `ShopifyClient::resolve_customer`). So customers must be
//! imported before any of those. The shape is otherwise the simplest kind: a
//! flat typed DTO, one `customerCreate` per record — model it on `product.rs`.
//!
//! Two read-vs-write gotchas to know:
//!   1. An address reads `countryCodeV2` but `CustomerInput` writes `countryCode`.
//!   2. `Customer.addresses` is a plain LIST (`[MailingAddress!]!`), not a
//!      connection, so there's no `nodes`/`pageInfo` around it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Resource;
use crate::client::ShopifyClient;

pub struct Customer;

/// One customer, deserialized from the exported JSON. Almost everything is
/// `Option` because a customer may have, say, only a phone and no email.
#[derive(Debug, Deserialize, Serialize)]
struct CustomerRecord {
    #[serde(rename = "firstName")]
    first_name: Option<String>,
    #[serde(rename = "lastName")]
    last_name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    note: Option<String>,
    tags: Option<Vec<String>>,
    #[serde(rename = "taxExempt")]
    tax_exempt: Option<bool>,
    /// Captured for fidelity in the export, but NOT replayed on create — see the
    /// module doc and `build_customer_input`. Held as raw JSON since we never
    /// read into it.
    #[serde(rename = "emailMarketingConsent")]
    email_marketing_consent: Option<Value>,
    #[serde(rename = "smsMarketingConsent")]
    sms_marketing_consent: Option<Value>,
    addresses: Option<Vec<AddressRecord>>,
}

/// One mailing address. Read shape uses `countryCodeV2`; the create input wants
/// `countryCode` — translated in `build_customer_input`.
#[derive(Debug, Deserialize, Serialize)]
struct AddressRecord {
    #[serde(rename = "firstName")]
    first_name: Option<String>,
    #[serde(rename = "lastName")]
    last_name: Option<String>,
    company: Option<String>,
    address1: Option<String>,
    address2: Option<String>,
    city: Option<String>,
    #[serde(rename = "provinceCode")]
    province_code: Option<String>,
    #[serde(rename = "countryCodeV2")]
    country_code: Option<String>,
    zip: Option<String>,
    phone: Option<String>,
}

const CUSTOMER_CREATE: &str = r#"
mutation CreateCustomer($input: CustomerInput!) {
  customerCreate(input: $input) {
    customer { id email }
    userErrors { field message }
  }
}
"#;

impl Resource for Customer {
    fn name(&self) -> &'static str {
        "customers"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
        let nodes = client.paginate(
            r#"
            query Customers($cursor: String) {
              customers(first: 50, after: $cursor) {
                nodes {
                  firstName
                  lastName
                  email
                  phone
                  note
                  tags
                  taxExempt
                  emailMarketingConsent { marketingState marketingOptInLevel }
                  smsMarketingConsent { marketingState marketingOptInLevel }
                  addresses(first: 10) {
                    firstName lastName company
                    address1 address2 city provinceCode countryCodeV2 zip phone
                  }
                }
                pageInfo { hasNextPage endCursor }
              }
            }
            "#,
            json!({}),
            "customers",
        )?;
        Ok(Value::Array(nodes))
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        let customers: Vec<CustomerRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of customers")?;

        println!("{} customer(s) to import", customers.len());

        for c in &customers {
            let label = c
                .email
                .as_deref()
                .or(c.phone.as_deref())
                .unwrap_or("(no identifier)");

            // The API requires at least a name, phone, or email.
            if c.email.is_none()
                && c.phone.is_none()
                && c.first_name.is_none()
                && c.last_name.is_none()
            {
                println!("  skipped {label}: customer needs a name, phone, or email");
                continue;
            }

            if dry_run {
                println!("  would create: {label}");
                continue;
            }

            let input = build_customer_input(c);
            let result = client.graphql(CUSTOMER_CREATE, json!({ "input": input }))?;
            let payload = &result["customerCreate"];

            // Best-effort per customer: a duplicate email (an idempotent re-run)
            // or any other per-record issue skips just this one, not the whole run.
            if let Some(errors) = payload["userErrors"].as_array()
                && !errors.is_empty()
            {
                println!("  skipped {label}: {}", payload["userErrors"]);
                continue;
            }
            println!("  created {label}");
        }
        Ok(())
    }
}

/// Translate a `CustomerRecord` into `CustomerInput`. The address `countryCodeV2`
/// read field becomes the input's `countryCode`; marketing consent is captured in
/// the export but deliberately omitted here (its input is strict enough that a
/// rejection would skip the whole customer — set consent separately if needed).
fn build_customer_input(c: &CustomerRecord) -> Value {
    let addresses: Vec<Value> = c
        .addresses
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|a| {
            json!({
                "firstName": a.first_name,
                "lastName": a.last_name,
                "company": a.company,
                "address1": a.address1,
                "address2": a.address2,
                "city": a.city,
                "provinceCode": a.province_code,
                "countryCode": a.country_code,
                "zip": a.zip,
                "phone": a.phone,
            })
        })
        .collect();

    json!({
        "firstName": c.first_name,
        "lastName": c.last_name,
        "email": c.email,
        "phone": c.phone,
        "note": c.note,
        "tags": c.tags,
        "taxExempt": c.tax_exempt,
        "addresses": addresses,
    })
}
