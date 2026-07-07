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

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Resource;
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

impl Resource for GiftCard {
    fn name(&self) -> &'static str {
        "giftcards"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
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

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        let cards: Vec<GiftCardRecord> = serde_json::from_value(data.clone())
            .context("import data was not a JSON array of gift cards")?;

        println!("{} gift card(s) to import", cards.len());

        for gc in &cards {
            let amount = &gc.initial_value.amount;
            let currency = &gc.initial_value.currency_code;
            let email = gc.customer.as_ref().and_then(|c| c.email.as_deref());

            if dry_run {
                match email {
                    Some(e) => println!("  would create: {amount} {currency} for {e}"),
                    None => println!("  would create: {amount} {currency} (unassigned)"),
                }
                continue;
            }

            // `initialValue` (Decimal) is deprecated; the current input field is
            // `initialAmount` (a MoneyInput).
            let mut input = json!({
                "initialAmount": { "amount": amount, "currencyCode": currency },
            });
            if let Some(e) = &gc.expires_on {
                input["expiresOn"] = json!(e);
            }
            if let Some(n) = &gc.note {
                input["note"] = json!(n);
            }

            // An assigned card must resolve its customer in the target store (by
            // email); if it can't, skip — an orphaned balance is worse than none.
            // Unassigned source cards create as-is.
            if let Some(e) = email {
                match client.resolve_customer(e) {
                    Ok(id) => input["customerId"] = json!(id),
                    Err(err) => {
                        println!("  skipped card for {e}: {err:#}");
                        continue;
                    }
                }
            }

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
}
