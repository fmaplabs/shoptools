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
use crate::bulk;
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

/// Bulk export query (validated ✅ 2026-07). `edges { node { … } }` with no
/// cursors/`pageInfo`; the Bulk Operations API streams *every* customer
/// server-side. Customers have **no nested connections** — `addresses` is a
/// plain list field, so it comes back inline on each customer's JSONL line and
/// needs no reassembly. We deliberately don't select `id`: the legacy export
/// doesn't, and with no children to link there's nothing to strip it against, so
/// omitting it keeps the on-disk shape byte-for-byte identical.
const BULK_CUSTOMERS: &str = r#"
query BulkCustomers {
  customers {
    edges {
      node {
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
    }
  }
}
"#;

impl Resource for Customer {
    fn name(&self) -> &'static str {
        "customers"
    }

    fn export(&self, client: &ShopifyClient, no_bulk: bool) -> Result<Value> {
        if no_bulk {
            return export_legacy(client);
        }

        // No nested connections → no `ChildSpec`. `reassemble` with an empty spec
        // list just returns the root lines in order (roots keep whatever they
        // selected; we selected no transport-only keys, so nothing is stripped).
        let lines = bulk::bulk_query(client, BULK_CUSTOMERS)?;
        let customers = bulk::reassemble(lines, &[]);
        Ok(Value::Array(customers))
    }

    fn import(
        &self,
        client: &ShopifyClient,
        data: &Value,
        dry_run: bool,
        no_bulk: bool,
    ) -> Result<()> {
        let customers: Vec<CustomerRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of customers")?;

        println!("{} customer(s) to import", customers.len());

        // dry-run: report the same per-record skips and planned creates as a real
        // run would, but touch no network.
        if dry_run {
            for c in &customers {
                let label = customer_label(c);
                if !is_importable(c) {
                    println!("  skipped {label}: customer needs a name, phone, or email");
                    continue;
                }
                println!("  would create: {label}");
            }
            return Ok(());
        }

        if no_bulk {
            return import_legacy(client, &customers);
        }

        // Bulk import: one `customerCreate` invocation per JSONL line. Records
        // that can't be created at all (no name/phone/email) are filtered out
        // up front with the same skip message — they can't go in the file.
        let mut importable: Vec<&CustomerRecord> = Vec::new();
        let mut lines: Vec<Value> = Vec::new();
        for c in &customers {
            let label = customer_label(c);
            if !is_importable(c) {
                println!("  skipped {label}: customer needs a name, phone, or email");
                continue;
            }
            lines.push(json!({ "input": build_customer_input(c) }));
            importable.push(c);
        }

        if lines.is_empty() {
            return Ok(());
        }

        let mut results = bulk::bulk_mutation(client, CUSTOMER_CREATE, &lines)?;

        // Results may arrive out of order; `__lineNumber` indexes back into the
        // input file, so sort by it before zipping with the importable records.
        results.sort_by_key(|r| r["__lineNumber"].as_u64().unwrap_or(u64::MAX));

        for (c, result) in importable.iter().zip(results.iter()) {
            let label = customer_label(c);
            let payload = customer_create_payload(result);

            // Best-effort per customer: a duplicate email (an idempotent re-run)
            // or any other per-record issue surfaces as a per-line userError skip,
            // not a fatal error.
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

/// Legacy cursor-paginated export (used with `--no-bulk`).
fn export_legacy(client: &ShopifyClient) -> Result<Value> {
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

/// Legacy per-record import (used with `--no-bulk`): one `customerCreate` GraphQL
/// call per customer, best-effort skipping on userErrors.
fn import_legacy(client: &ShopifyClient, customers: &[CustomerRecord]) -> Result<()> {
    for c in customers {
        let label = customer_label(c);

        // The API requires at least a name, phone, or email.
        if !is_importable(c) {
            println!("  skipped {label}: customer needs a name, phone, or email");
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

/// The human-readable identifier used in log lines: email, else phone, else a
/// placeholder.
fn customer_label(c: &CustomerRecord) -> &str {
    c.email
        .as_deref()
        .or(c.phone.as_deref())
        .unwrap_or("(no identifier)")
}

/// `customerCreate` needs at least a name, phone, or email; a record with none
/// can't be created and is skipped before we build a mutation for it.
fn is_importable(c: &CustomerRecord) -> bool {
    c.email.is_some() || c.phone.is_some() || c.first_name.is_some() || c.last_name.is_some()
}

/// Locate the `customerCreate` payload inside one bulk-mutation result line. Bulk
/// results wrap each line's mutation output in `data`; fall back to the bare
/// payload if that wrapper is ever absent.
fn customer_create_payload(result: &Value) -> &Value {
    if result.get("data").is_some() {
        &result["data"]["customerCreate"]
    } else {
        &result["customerCreate"]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_address_country_code_and_omits_consent() {
        let record: CustomerRecord = serde_json::from_value(json!({
            "firstName": "Ada",
            "lastName": "Lovelace",
            "email": "ada@example.com",
            "tags": ["vip"],
            "taxExempt": true,
            "emailMarketingConsent": { "marketingState": "SUBSCRIBED" },
            "addresses": [{
                "address1": "1 Analytical Way",
                "city": "London",
                "provinceCode": "ENG",
                "countryCodeV2": "GB",
                "zip": "EC1A"
            }]
        }))
        .unwrap();

        let input = build_customer_input(&record);

        assert_eq!(input["firstName"], "Ada");
        assert_eq!(input["email"], "ada@example.com");
        assert_eq!(input["tags"][0], "vip");
        assert_eq!(input["taxExempt"], true);
        // The read `countryCodeV2` becomes the input's `countryCode`.
        assert_eq!(input["addresses"][0]["countryCode"], "GB");
        assert!(input["addresses"][0].get("countryCodeV2").is_none());
        assert_eq!(input["addresses"][0]["provinceCode"], "ENG");
        // Marketing consent is captured on export but never replayed on create.
        assert!(input.get("emailMarketingConsent").is_none());
    }

    #[test]
    fn importable_requires_name_phone_or_email() {
        let empty: CustomerRecord = serde_json::from_value(json!({})).unwrap();
        assert!(!is_importable(&empty));

        let named: CustomerRecord = serde_json::from_value(json!({ "firstName": "Grace" })).unwrap();
        assert!(is_importable(&named));

        let emailed: CustomerRecord =
            serde_json::from_value(json!({ "email": "g@example.com" })).unwrap();
        assert!(is_importable(&emailed));
    }

    #[test]
    fn customer_create_payload_unwraps_bulk_data_envelope() {
        let bulk = json!({
            "data": { "customerCreate": { "userErrors": [{ "message": "boom" }] } },
            "__lineNumber": 3
        });
        assert_eq!(
            customer_create_payload(&bulk)["userErrors"][0]["message"],
            "boom"
        );

        let bare = json!({ "customerCreate": { "userErrors": [] } });
        assert!(
            customer_create_payload(&bare)["userErrors"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
