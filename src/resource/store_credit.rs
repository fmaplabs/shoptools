//! Store credit. There's no top-level store-credit connection — a balance hangs
//! off a **customer's** `storeCreditAccounts`. So export walks customers and keeps
//! only those carrying a non-zero balance; import resolves each customer by email
//! in the target store and credits the account.
//!
//! Because store credit is per-customer, `customers` must be imported first.
//! Only *current balances* are reproduced — the debit/transaction history and
//! any expiry are not. Template: `metaobject.rs`'s iterate-and-create + best-effort.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::Resource;
use crate::bulk::{self, ChildSpec};
use crate::client::ShopifyClient;

pub struct StoreCredit;

/// One customer's store credit, as produced by `export`.
#[derive(Debug, Deserialize)]
struct StoreCreditRecord {
    email: String,
    balances: Vec<Money>,
}

#[derive(Debug, Deserialize)]
struct Money {
    amount: String,
    #[serde(rename = "currencyCode")]
    currency_code: String,
}

const STORE_CREDIT_CREDIT: &str = r#"
mutation CreditStoreCredit($id: ID!, $creditInput: StoreCreditAccountCreditInput!) {
  storeCreditAccountCredit(id: $id, creditInput: $creditInput) {
    userErrors { field message }
  }
}
"#;

/// Bulk export query (validated ✅ 2026-07). `customers → storeCreditAccounts`,
/// both as `edges { node }` with no cursors/`pageInfo`. The customer node selects
/// `id` purely so the bulk API can link `storeCreditAccount` child lines via
/// `__parentId`; `reshape_store_credit` strips it back out. Account children come
/// back as separate JSONL lines tagged with `__parentId`/`__typename`, which
/// `bulk::reassemble` nests under `storeCreditAccounts.nodes`.
const BULK_STORE_CREDIT: &str = r#"
query BulkStoreCredit {
  customers {
    edges {
      node {
        id
        email
        storeCreditAccounts {
          edges {
            node {
              __typename
              balance { amount currencyCode }
            }
          }
        }
      }
    }
  }
}
"#;

impl Resource for StoreCredit {
    fn name(&self) -> &'static str {
        "store_credit"
    }

    fn export(&self, client: &ShopifyClient, no_bulk: bool) -> Result<Value> {
        if no_bulk {
            return export_legacy(client);
        }

        let lines = bulk::bulk_query(client, BULK_STORE_CREDIT)?;
        let specs = [ChildSpec {
            typename: "StoreCreditAccount",
            field: "storeCreditAccounts",
        }];
        let customers = bulk::reassemble(lines, &specs);
        Ok(Value::Array(reshape_store_credit(customers)))
    }

    fn import(
        &self,
        client: &ShopifyClient,
        data: &Value,
        dry_run: bool,
        no_bulk: bool,
    ) -> Result<()> {
        let records: Vec<StoreCreditRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of store credit records")?;

        println!("{} customer(s) with store credit to import", records.len());

        // dry-run: describe planned actions, touch no network.
        if dry_run {
            for r in &records {
                println!("  would credit {}: {}", r.email, balance_summary(r));
            }
            return Ok(());
        }

        if no_bulk {
            return import_legacy(client, &records);
        }

        // Pre-resolve every target-store customer email → id in ONE pass, instead
        // of a `resolve_customer` round-trip per record.
        let email_to_id = build_customer_email_map(client)?;

        // Build one JSONL line per (customer, balance). `storeCreditAccountCredit`
        // accepts a Customer id directly and auto-creates the currency account.
        // Keep a parallel summary list so we can report per-line results.
        let mut lines: Vec<Value> = Vec::new();
        let mut summaries: Vec<(String, String, String)> = Vec::new();
        for r in &records {
            let customer_id = match email_to_id.get(&r.email.to_ascii_lowercase()) {
                Some(id) => id,
                None => {
                    println!(
                        "  skipped {}: target store has no customer with that email",
                        r.email
                    );
                    continue;
                }
            };

            for bal in &r.balances {
                lines.push(json!({
                    "id": customer_id,
                    "creditInput": {
                        "creditAmount": { "amount": bal.amount, "currencyCode": bal.currency_code },
                    },
                }));
                summaries.push((r.email.clone(), bal.amount.clone(), bal.currency_code.clone()));
            }
        }

        if lines.is_empty() {
            return Ok(());
        }

        let mut results = bulk::bulk_mutation(client, STORE_CREDIT_CREDIT, &lines)?;

        // Results may arrive out of order; `__lineNumber` indexes back into the
        // input file, so sort by it before zipping with our summaries.
        results.sort_by_key(|r| r["__lineNumber"].as_u64().unwrap_or(u64::MAX));

        for ((email, amount, currency), result) in summaries.iter().zip(results.iter()) {
            let payload = store_credit_payload(result);
            if let Some(errors) = payload["userErrors"].as_array()
                && !errors.is_empty()
            {
                println!("  {amount} {currency} skipped: {}", payload["userErrors"]);
                continue;
            }
            println!("  credited {email} {amount} {currency}");
        }
        Ok(())
    }
}

/// Legacy cursor-paginated export (used with `--no-bulk`): pages `customers`,
/// reading nested `storeCreditAccounts`, and keeps non-zero balances.
fn export_legacy(client: &ShopifyClient) -> Result<Value> {
    let customers = client.paginate(
        r#"
        query StoreCredit($cursor: String) {
          customers(first: 50, after: $cursor) {
            nodes {
              email
              storeCreditAccounts(first: 10) {
                nodes { balance { amount currencyCode } }
              }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
        "#,
        json!({}),
        "customers",
    )?;
    Ok(Value::Array(reshape_store_credit(customers)))
}

/// Legacy per-record import (used with `--no-bulk`): resolve the customer by
/// email per record, then one `storeCreditAccountCredit` call per balance.
fn import_legacy(client: &ShopifyClient, records: &[StoreCreditRecord]) -> Result<()> {
    for r in records {
        let customer_id = match client.resolve_customer(&r.email) {
            Ok(id) => id,
            Err(e) => {
                println!("  skipped {}: {e:#}", r.email);
                continue;
            }
        };

        for bal in &r.balances {
            let credit_input = json!({
                "creditAmount": { "amount": bal.amount, "currencyCode": bal.currency_code },
            });
            let result = client.graphql(
                STORE_CREDIT_CREDIT,
                json!({ "id": customer_id, "creditInput": credit_input }),
            )?;
            let payload = &result["storeCreditAccountCredit"];
            if let Some(errors) = payload["userErrors"].as_array()
                && !errors.is_empty()
            {
                println!(
                    "  {} {} skipped: {}",
                    bal.amount, bal.currency_code, payload["userErrors"]
                );
                continue;
            }
            println!("  credited {} {} {}", r.email, bal.amount, bal.currency_code);
        }
    }
    Ok(())
}

/// Reshape reassembled/paginated customer records into the exact on-disk export
/// shape: `{ email, balances }` keeping only non-zero balances, omitting
/// customers with no store credit. Pure — no network — so it's unit-testable.
/// Accepts both the bulk-reassembled shape (customer carries a transport `id`,
/// stripped here) and the legacy paginated shape.
fn reshape_store_credit(customers: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for c in &customers {
        let Some(email) = c["email"].as_str() else {
            continue;
        };
        let balances: Vec<Value> = c["storeCreditAccounts"]["nodes"]
            .as_array()
            .map(|accts| {
                accts
                    .iter()
                    .filter_map(|a| {
                        let bal = &a["balance"];
                        let amount = bal["amount"].as_str()?;
                        if amount.parse::<f64>().unwrap_or(0.0) == 0.0 {
                            return None;
                        }
                        Some(json!({ "amount": amount, "currencyCode": bal["currencyCode"] }))
                    })
                    .collect()
            })
            .unwrap_or_default();

        if balances.is_empty() {
            continue;
        }
        out.push(json!({ "email": email, "balances": balances }));
    }
    out
}

/// Build a target-store `email (lowercased) → customer id` map in one paginated
/// pass. A plain paginated query (not a bulk operation) is right here: this is a
/// lookup against the *target* store during import, not an export.
fn build_customer_email_map(client: &ShopifyClient) -> Result<HashMap<String, String>> {
    let customers = client.paginate(
        r#"
        query CustomersEmailId($cursor: String) {
          customers(first: 250, after: $cursor) {
            nodes { id email }
            pageInfo { hasNextPage endCursor }
          }
        }
        "#,
        json!({}),
        "customers",
    )?;

    let mut map = HashMap::new();
    for c in &customers {
        if let (Some(email), Some(id)) = (c["email"].as_str(), c["id"].as_str()) {
            map.insert(email.to_ascii_lowercase(), id.to_string());
        }
    }
    Ok(map)
}

/// One-line human summary of a record's balances, e.g. "50.00 USD, 10.00 CAD".
fn balance_summary(r: &StoreCreditRecord) -> String {
    r.balances
        .iter()
        .map(|b| format!("{} {}", b.amount, b.currency_code))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Locate the `storeCreditAccountCredit` payload inside one bulk-mutation result
/// line. Bulk results wrap each line's mutation output in `data`; fall back to
/// the bare payload if that wrapper is ever absent.
fn store_credit_payload(result: &Value) -> &Value {
    if result.get("data").is_some() {
        &result["data"]["storeCreditAccountCredit"]
    } else {
        &result["storeCreditAccountCredit"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reshapes_bulk_customers_dropping_zero_and_empty() {
        // Mirrors the reassembled bulk shape: customers carry a transport `id`
        // and a `storeCreditAccounts.nodes` list; one has a non-zero balance, one
        // has only a zero balance, one has no accounts at all.
        let customers = vec![
            json!({
                "id": "gid://shopify/Customer/1",
                "email": "has@credit.com",
                "storeCreditAccounts": { "nodes": [
                    { "balance": { "amount": "50.00", "currencyCode": "USD" } },
                    { "balance": { "amount": "0.0", "currencyCode": "CAD" } }
                ] }
            }),
            json!({
                "id": "gid://shopify/Customer/2",
                "email": "zero@credit.com",
                "storeCreditAccounts": { "nodes": [
                    { "balance": { "amount": "0.00", "currencyCode": "USD" } }
                ] }
            }),
            json!({ "id": "gid://shopify/Customer/3", "email": "no@credit.com" }),
        ];

        let out = reshape_store_credit(customers);

        // Only the customer with a non-zero balance survives.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["email"], "has@credit.com");
        // Transport-only `id` is dropped from the output shape.
        assert!(out[0].get("id").is_none());
        // Only the non-zero balance is kept.
        let balances = out[0]["balances"].as_array().unwrap();
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0]["amount"], "50.00");
        assert_eq!(balances[0]["currencyCode"], "USD");
    }

    #[test]
    fn payload_unwraps_bulk_data_envelope() {
        let wrapped = json!({ "data": { "storeCreditAccountCredit": { "userErrors": [] } } });
        assert!(store_credit_payload(&wrapped)["userErrors"]
            .as_array()
            .unwrap()
            .is_empty());
        let bare = json!({ "storeCreditAccountCredit": { "userErrors": [{ "message": "x" }] } });
        assert_eq!(
            store_credit_payload(&bare)["userErrors"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
