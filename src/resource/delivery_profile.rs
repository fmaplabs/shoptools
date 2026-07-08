//! Delivery (shipping) profiles — the most structurally complex resource.
//!
//! A profile is: location groups → zones (countries/provinces) → method
//! definitions (shipping rates). The READ shape wraps each level in a Relay
//! connection and exposes `rateProvider` as a union; the WRITE shape
//! (`DeliveryProfileInput`) is flat `...ToCreate` arrays. Because the nesting is
//! several levels deep we navigate raw `serde_json::Value` here (like
//! `discount.rs`'s `customerGets` translation) rather than typed DTOs.
//!
//! Cross-store remap: locations are referenced by opaque GIDs that differ per
//! store, so export captures each location's NAME and import resolves it against
//! the target via `ShopifyClient::resolve_location`. Locations themselves are
//! physical store config and are assumed to already exist in the target.
//!
//! v1 scope (documented deferrals):
//!   * the default "General Profile" is skipped — Shopify auto-creates it and
//!     rejects a duplicate.
//!   * only flat `DeliveryRateDefinition` rates are cloned; carrier-calculated /
//!     participant rates are skipped.
//!   * method conditions (weight/price tiers) and product association are not
//!     cloned.
//!   * nested connections are read at `first: 50` without deeper paging.
//!
//! IMPORTANT: the deeply-nested read field paths below should be validated
//! against the live 2026-07 schema on first run (a wrong field yields a clear
//! GraphQL error naming it).

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::Resource;
use crate::client::ShopifyClient;

pub struct DeliveryProfile;

/// Borrow a `Value`'s array as a slice, or an empty slice if it isn't one.
/// Keeps the deep navigation below free of `unwrap_or(&vec![])` lifetime pain.
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

const DELIVERY_PROFILE_CREATE: &str = r#"
mutation CreateDeliveryProfile($profile: DeliveryProfileInput!) {
  deliveryProfileCreate(profile: $profile) {
    profile { id name }
    userErrors { field message }
  }
}
"#;

impl Resource for DeliveryProfile {
    fn name(&self) -> &'static str {
        "delivery_profiles"
    }

    fn export(&self, client: &ShopifyClient, _no_bulk: bool) -> Result<Value> {
        let profiles = client.paginate(
            r#"
            query DeliveryProfiles($cursor: String) {
              deliveryProfiles(first: 25, after: $cursor) {
                nodes {
                  id
                  name
                  default
                  profileLocationGroups {
                    locationGroup {
                      locations(first: 50) { nodes { name } }
                    }
                    locationGroupZones(first: 50) {
                      nodes {
                        zone {
                          name
                          countries {
                            code { countryCode restOfWorld }
                            provinces { code }
                          }
                        }
                        methodDefinitions(first: 50) {
                          nodes {
                            name
                            active
                            rateProvider {
                              __typename
                              ... on DeliveryRateDefinition {
                                price { amount currencyCode }
                              }
                            }
                          }
                        }
                      }
                    }
                  }
                }
                pageInfo { hasNextPage endCursor }
              }
            }
            "#,
            json!({}),
            "deliveryProfiles",
        )?;

        // Skip the default profile — it always exists in the target and can't be
        // re-created.
        let kept: Vec<Value> = profiles
            .into_iter()
            .filter(|p| p["default"].as_bool() != Some(true))
            .collect();
        Ok(Value::Array(kept))
    }

    fn import(
        &self,
        client: &ShopifyClient,
        data: &Value,
        dry_run: bool,
        _no_bulk: bool,
    ) -> Result<()> {
        let profiles = arr(data);
        println!("{} delivery profile(s) to import", profiles.len());

        for p in profiles {
            let name = p["name"].as_str().unwrap_or("(unnamed)");
            if dry_run {
                let groups = arr(&p["profileLocationGroups"]).len();
                println!("  would create profile: {name} ({groups} location group(s))");
                continue;
            }
            // Best-effort per profile: an unresolved location or a rejected input
            // skips this profile, not the whole run.
            match create_profile(client, p) {
                Ok(()) => println!("  created profile {name}"),
                Err(e) => println!("  profile '{name}' skipped: {e:#}"),
            }
        }
        Ok(())
    }
}

/// Translate one exported profile (read shape) into `DeliveryProfileInput` and
/// create it. Location names are resolved to target ids along the way.
fn create_profile(client: &ShopifyClient, p: &Value) -> Result<()> {
    let mut location_groups: Vec<Value> = Vec::new();

    for plg in arr(&p["profileLocationGroups"]) {
        // Resolve every location in this group by name → target id.
        let mut locations_to_add: Vec<String> = Vec::new();
        for loc in arr(&plg["locationGroup"]["locations"]["nodes"]) {
            let lname = loc["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("location is missing its name"))?;
            locations_to_add.push(client.resolve_location(lname)?);
        }

        let zones: Vec<Value> = arr(&plg["locationGroupZones"]["nodes"])
            .iter()
            .map(|zn| build_zone(&zn["zone"], &zn["methodDefinitions"]))
            .collect();

        location_groups.push(json!({
            "locationsToAdd": locations_to_add,
            "zonesToCreate": zones,
        }));
    }

    let input = json!({
        "name": p["name"],
        "locationGroupsToCreate": location_groups,
    });

    let result = client.graphql(DELIVERY_PROFILE_CREATE, json!({ "profile": input }))?;
    let payload = &result["deliveryProfileCreate"];
    if let Some(errors) = payload["userErrors"].as_array()
        && !errors.is_empty()
    {
        bail!("{}", payload["userErrors"]);
    }
    Ok(())
}

/// Build one `DeliveryLocationGroupZoneInput` from the read shapes of a zone and
/// its method definitions.
///   * countries: `{ code { countryCode restOfWorld }, provinces { code } }`
///     → `{ code, restOfWorld?, provinces:[{code}] }`
///   * only `DeliveryRateDefinition` (flat-rate) methods are carried; other rate
///     providers (carrier/participant) are skipped.
fn build_zone(zone: &Value, method_defs: &Value) -> Value {
    let countries: Vec<Value> = arr(&zone["countries"])
        .iter()
        .map(|c| {
            let mut out = json!({});
            if let Some(cc) = c["code"]["countryCode"].as_str() {
                out["code"] = json!(cc);
            }
            if c["code"]["restOfWorld"].as_bool() == Some(true) {
                out["restOfWorld"] = json!(true);
            }
            let provinces: Vec<Value> = arr(&c["provinces"])
                .iter()
                .filter_map(|pr| pr["code"].as_str().map(|code| json!({ "code": code })))
                .collect();
            if !provinces.is_empty() {
                out["provinces"] = json!(provinces);
            }
            out
        })
        .collect();

    let mut methods: Vec<Value> = Vec::new();
    for md in arr(&method_defs["nodes"]) {
        if md["rateProvider"]["__typename"].as_str() != Some("DeliveryRateDefinition") {
            continue; // skip carrier-calculated / participant rates
        }
        let price = &md["rateProvider"]["price"];
        methods.push(json!({
            "name": md["name"],
            "rateDefinition": {
                "price": { "amount": price["amount"], "currencyCode": price["currencyCode"] }
            },
        }));
    }

    json!({
        "name": zone["name"],
        "countries": countries,
        "methodDefinitionsToCreate": methods,
    })
}
