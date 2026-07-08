//! Gift cards. Two hard API limitations shape this resource:
//!   1. The Admin API MASKS gift card codes (`maskedCode`/`lastCharacters` only),
//!      so the real code is unreadable — imported cards get a FRESH code.
//!   2. `giftCardCreate` sets an *initial amount*, not the current balance, so a
//!      partially-redeemed card can't be reproduced exactly. Per the project
//!      decision we recreate at the source card's ORIGINAL initial value.
//!
//! A gift card may belong to a customer (matched across stores by email), so
//! `customers` must be imported first. Template: `product.rs` (flat) plus the
//! reference-resolve step from `discount.rs`.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Resource;
use crate::bulk;
use crate::client::ShopifyClient;

pub struct GiftCard;

#[derive(Debug, Deserialize, Serialize)]
struct GiftCardRecord {
    #[serde(rename = "initialValue")]
    initial_value: Money,
    #[serde(rename = "expiresOn")]
    expires_on: Option<String>,
    note: Option<String>,
    customer: Option<CustomerRef>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Money {
    amount: String,
    #[serde(rename = "currencyCode")]
    currency_code: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct CustomerRef {
    email: Option<String>,
}

const GIFT_CARD_CREATE: &str = r#"
mutation CreateGiftCard($input: GiftCardCreateInput!) {
  giftCardCreate(input: $input) {
    giftCard { id lastCharacters }
    userErrors { field message }
  }
}
"#;

/// Bulk export query (validated ✅ 2026-07). `edges { node { … } }` with no
/// cursors/`pageInfo`; gift cards have no nested connections, so the streamed
/// JSONL lines *are* the records — no reassembly needed. Field selection mirrors
/// the legacy query exactly (and, like it, selects no `id`) so the on-disk shape
/// is unchanged.
const BULK_GIFT_CARDS: &str = r#"
query BulkGiftCards {
  giftCards {
    edges {
      node {
        initialValue { amount currencyCode }
        balance { amount currencyCode }
        expiresOn
        note
        enabled
        customer { email }
      }
    }
  }
}
"#;

impl Resource for GiftCard {
    fn name(&self) -> &'static str {
        "giftcards"
    }

    fn export(&self, client: &ShopifyClient, no_bulk: bool) -> Result<Value> {
        if no_bulk {
            return export_legacy(client);
        }

        // No nested connections → the flattened lines are already the records.
        let nodes = bulk::bulk_query(client, BULK_GIFT_CARDS)?;
        Ok(Value::Array(nodes))
    }

    fn import(
        &self,
        client: &ShopifyClient,
        data: &Value,
        dry_run: bool,
        no_bulk: bool,
    ) -> Result<()> {
        let cards: Vec<GiftCardRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of gift cards")?;

        println!("{} gift card(s) to import", cards.len());

        // dry-run: describe planned actions, touch no network.
        if dry_run {
            for gc in &cards {
                let amount = &gc.initial_value.amount;
                let currency = &gc.initial_value.currency_code;
                match gc.customer.as_ref().and_then(|c| c.email.as_deref()) {
                    Some(e) => println!("  would create: {amount} {currency} for {e}"),
                    None => println!("  would create: {amount} {currency} (unassigned)"),
                }
            }
            return Ok(());
        }

        if no_bulk {
            return import_legacy(client, &cards);
        }

        // Pre-resolve every target-store customer email → id in ONE pass, instead
        // of a `resolve_customer` round-trip per assigned card.
        let email_to_id = build_customer_email_map(client)?;

        // Build one JSONL line per card that resolves; keep a parallel summary
        // list so we can report results (which arrive keyed by __lineNumber).
        let mut lines: Vec<Value> = Vec::new();
        let mut summaries: Vec<(String, String)> = Vec::new();
        for gc in &cards {
            let amount = gc.initial_value.amount.clone();
            let currency = gc.initial_value.currency_code.clone();
            let email = gc.customer.as_ref().and_then(|c| c.email.as_deref());

            let customer_id = match email {
                // An assigned card must resolve its customer in the target store;
                // if it can't, warn and exclude — an orphaned balance is worse
                // than none. Unassigned source cards create as-is.
                Some(e) => match email_to_id.get(&e.to_ascii_lowercase()) {
                    Some(id) => Some(id.as_str()),
                    None => {
                        println!("  skipped card for {e}: target store has no customer with that email");
                        continue;
                    }
                },
                None => None,
            };

            lines.push(json!({ "input": build_gift_card_input(gc, customer_id) }));
            summaries.push((amount, currency));
        }

        if lines.is_empty() {
            return Ok(());
        }

        let mut results = bulk::bulk_mutation(client, GIFT_CARD_CREATE, &lines)?;

        // Results may arrive out of order; `__lineNumber` indexes back into the
        // input file, so sort by it before zipping with our summaries.
        results.sort_by_key(|r| r["__lineNumber"].as_u64().unwrap_or(u64::MAX));

        for ((amount, currency), result) in summaries.iter().zip(results.iter()) {
            let payload = gift_card_create_payload(result);
            if let Some(errors) = payload["userErrors"].as_array()
                && !errors.is_empty()
            {
                println!(
                    "  skipped {amount} {currency} card: {}",
                    payload["userErrors"]
                );
                continue;
            }
            let last4 = payload["giftCard"]["lastCharacters"]
                .as_str()
                .unwrap_or("????");
            println!(
                "  created {amount} {currency} card (\u{2022}\u{2022}\u{2022}\u{2022}{last4}, fresh code)"
            );
        }
        Ok(())
    }
}

/// Legacy cursor-paginated export (used with `--no-bulk`).
fn export_legacy(client: &ShopifyClient) -> Result<Value> {
    let nodes = client.paginate(
        r#"
        query GiftCards($cursor: String) {
          giftCards(first: 50, after: $cursor) {
            nodes {
              initialValue { amount currencyCode }
              balance { amount currencyCode }
              expiresOn
              note
              enabled
              customer { email }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
        "#,
        json!({}),
        "giftCards",
    )?;
    Ok(Value::Array(nodes))
}

/// Legacy per-record import (used with `--no-bulk`): one `giftCardCreate` call
/// per card, resolving the customer by email per record, best-effort skipping.
fn import_legacy(client: &ShopifyClient, cards: &[GiftCardRecord]) -> Result<()> {
    for gc in cards {
        let amount = &gc.initial_value.amount;
        let currency = &gc.initial_value.currency_code;
        let email = gc.customer.as_ref().and_then(|c| c.email.as_deref());

        let customer_id = match email {
            Some(e) => match client.resolve_customer(e) {
                Ok(id) => Some(id),
                Err(err) => {
                    println!("  skipped card for {e}: {err:#}");
                    continue;
                }
            },
            None => None,
        };

        let input = build_gift_card_input(gc, customer_id.as_deref());
        let result = client.graphql(GIFT_CARD_CREATE, json!({ "input": input }))?;
        let payload = &result["giftCardCreate"];
        if let Some(errors) = payload["userErrors"].as_array()
            && !errors.is_empty()
        {
            println!(
                "  skipped {amount} {currency} card: {}",
                payload["userErrors"]
            );
            continue;
        }
        let last4 = payload["giftCard"]["lastCharacters"]
            .as_str()
            .unwrap_or("????");
        println!(
            "  created {amount} {currency} card (\u{2022}\u{2022}\u{2022}\u{2022}{last4}, fresh code)"
        );
    }
    Ok(())
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

/// Translate a `GiftCardRecord` into `GiftCardCreateInput`. Pure — no network —
/// so it's unit-testable. Recreates the card at its ORIGINAL initial value (see
/// the module docs) and, when supplied, assigns it to a target-store customer.
fn build_gift_card_input(gc: &GiftCardRecord, customer_id: Option<&str>) -> Value {
    // `initialValue` (Decimal) is deprecated; the current input field is
    // `initialAmount` (a MoneyInput).
    let mut input = json!({
        "initialAmount": {
            "amount": gc.initial_value.amount,
            "currencyCode": gc.initial_value.currency_code,
        },
    });
    if let Some(e) = &gc.expires_on {
        input["expiresOn"] = json!(e);
    }
    if let Some(n) = &gc.note {
        input["note"] = json!(n);
    }
    if let Some(id) = customer_id {
        input["customerId"] = json!(id);
    }
    input
}

/// Locate the `giftCardCreate` payload inside one bulk-mutation result line.
/// Bulk results wrap each line's mutation output in `data`; fall back to the bare
/// payload if that wrapper is ever absent.
fn gift_card_create_payload(result: &Value) -> &Value {
    if result.get("data").is_some() {
        &result["data"]["giftCardCreate"]
    } else {
        &result["giftCardCreate"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(email: Option<&str>) -> GiftCardRecord {
        serde_json::from_value(json!({
            "initialValue": { "amount": "50.00", "currencyCode": "USD" },
            "expiresOn": "2030-01-01",
            "note": "welcome",
            "customer": email.map(|e| json!({ "email": e })),
        }))
        .unwrap()
    }

    #[test]
    fn builds_input_with_customer() {
        let gc = record(Some("a@b.com"));
        let input = build_gift_card_input(&gc, Some("gid://shopify/Customer/1"));
        assert_eq!(input["initialAmount"]["amount"], "50.00");
        assert_eq!(input["initialAmount"]["currencyCode"], "USD");
        assert_eq!(input["expiresOn"], "2030-01-01");
        assert_eq!(input["note"], "welcome");
        assert_eq!(input["customerId"], "gid://shopify/Customer/1");
    }

    #[test]
    fn builds_input_unassigned_omits_customer_id() {
        let gc = record(None);
        let input = build_gift_card_input(&gc, None);
        assert!(input.get("customerId").is_none());
    }

    #[test]
    fn payload_unwraps_bulk_data_envelope() {
        let wrapped = json!({ "data": { "giftCardCreate": { "userErrors": [] } } });
        assert!(gift_card_create_payload(&wrapped)["userErrors"]
            .as_array()
            .unwrap()
            .is_empty());
        let bare = json!({ "giftCardCreate": { "userErrors": [{ "message": "x" }] } });
        assert_eq!(
            gift_card_create_payload(&bare)["userErrors"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
