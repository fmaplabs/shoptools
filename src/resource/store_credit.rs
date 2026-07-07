//! Store credit. There's no top-level store-credit connection — a balance hangs
//! off a **customer's** `storeCreditAccounts`. So export walks customers and keeps
//! only those carrying a non-zero balance; import resolves each customer by email
//! in the target store and credits the account.
//!
//! Because store credit is per-customer, `customers` must be imported first.
//! Only *current balances* are reproduced — the debit/transaction history and
//! any expiry are not. Template: `metaobject.rs`'s iterate-and-create + best-effort.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::Resource;
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

impl Resource for StoreCredit {
    fn name(&self) -> &'static str {
        "store_credit"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
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

        // Keep only customers who have a non-zero store credit balance.
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
        Ok(Value::Array(out))
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        let records: Vec<StoreCreditRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of store credit records")?;

        println!("{} customer(s) with store credit to import", records.len());

        for r in &records {
            let summary = r
                .balances
                .iter()
                .map(|b| format!("{} {}", b.amount, b.currency_code))
                .collect::<Vec<_>>()
                .join(", ");

            if dry_run {
                println!("  would credit {}: {summary}", r.email);
                continue;
            }

            // Resolve the customer in the target store by email. `storeCreditAccountCredit`
            // accepts a Customer id directly and auto-creates the currency account.
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
                println!(
                    "  credited {} {} {}",
                    r.email, bal.amount, bal.currency_code
                );
            }
        }
        Ok(())
    }
}
