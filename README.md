# shoptools

A personal CLI for Shopify store development, written in Rust — and a hands-on
project for learning Rust. Query a store's Admin API, manage credentials for
many stores/environments, export/import resources, and **clone** resources from
one store into another.

Design doc:
[`docs/superpowers/specs/2026-07-04-shoptools-cli-design.md`](docs/superpowers/specs/2026-07-04-shoptools-cli-design.md)

## Build & run

```sh
cargo build            # compile
cargo test             # run tests (config module has real tests)
cargo run -- --help    # everything after `--` is passed to shoptools
cargo run -- store list
```

`cargo run -- <args>` builds if needed, then runs the binary with `<args>`.

## What works right now

The credential layer is complete — no network needed:

```sh
# Point at a scratch config so you don't touch ~/.config while experimenting:
export SHOPTOOLS_CONFIG=/tmp/shoptools-dev.toml

cargo run -- store add acme-dev --shop acme-dev.myshopify.com --token shpat_xxx
cargo run -- store list          # the default is marked with *
cargo run -- store use acme-dev
cargo run -- store remove acme-dev
```

Everything else (`query`, `export`, `import`, `clone`) compiles but panics with
`not yet implemented` — those are yours to build.

## Your implementation path

Work top to bottom; each step unlocks the next. Search the code for `TODO(you)`.

1. **`src/client.rs`** — `ShopifyClient::new` + `graphql`. Your first live API call.
2. **`src/commands/query.rs`** — finish the result-printing TODO. Now
   `cargo run -- query '{ shop { name } }'` works end-to-end. 🎉
3. **`src/resource/product.rs`** `export` → **`src/commands/export.rs`** file write.
   Now `cargo run -- export products` works.
4. **`src/commands/import.rs`** + `product.rs` `import`.
5. **`src/commands/clone.rs`** loop — the headline feature.
6. Fill in `discount.rs` and `metaobject.rs` the same way.

### Getting a token

In a store's admin: **Settings → Apps and sales channels → Develop apps →
Create an app → Configure Admin API scopes → Install → reveal the Admin API
access token** (`shpat_…`). That token is what `--token` / the `SHOPIFY_*_TOKEN`
env vars want. Use an **offline** token (the default for custom apps); it doesn't
expire with a session.

### Environment variables

Credential env vars are explicit about which side of a data flow they belong
to, so a `.env` (loaded automatically via `dotenvy`) can hold both stores at
once. When both variables of a pair are set, they take priority over the
config file:

| Variable | Role |
| --- | --- |
| `SHOPIFY_SOURCE_SHOP` / `SHOPIFY_SOURCE_TOKEN` | the store data is read **from** (`query`, `export`) |
| `SHOPIFY_TARGET_SHOP` / `SHOPIFY_TARGET_TOKEN` | the store data is written **into** (`import`) |
| `SHOPTOOLS_CONFIG` | path override for the config file itself (not a store credential) |

`clone --from A --to B` names both stores explicitly, so it always reads them
from the config file.

## Supported resources

`export` / `import` / `clone` work over these resource names (in a safe clone
order, dependencies first):

| Name | Notes |
| --- | --- |
| `metaobjects` | definitions + entries; references remapped by handle |
| `customers` | matched across stores by **email**; the foundation for the three below |
| `products` | handle, options, variants, prices — created via `productSet` |
| `delivery_profiles` | flat-rate zones; locations remapped by name; default profile skipped |
| `discounts` | percentage, all-customers; collection scope remapped by handle |
| `giftcards` | codes can't be read, so imported cards get **fresh codes** |
| `store_credit` | per-customer balances (current balance only) |

Cross-store references are never carried by id — they're re-resolved against the
target store by a **stable identifier** (email / handle / location name). The
Admin API's cost-based rate limit is handled with automatic backoff-and-retry in
`ShopifyClient::graphql`, so paginated requests ride out throttling.

## Bulk Operations

`export` and `import` use Shopify's [Bulk Operations API](https://shopify.dev/docs/api/usage/bulk-operations/queries)
by default for `products`, `customers`, `discounts`, `giftcards`, and
`store_credit`: exports run a single `bulkOperationRunQuery` and download the
resulting JSONL (no pagination, no nested-connection caps, effectively immune
to rate limiting), and imports upload a JSONL variables file via
`stagedUploadsCreate` and run one `bulkOperationRunMutation` (per-record errors
are still reported as skips, so re-runs stay idempotent). The on-disk JSON
format is unchanged — bulk JSONL is reassembled into the same shape the
paginated path produces, so old export files import fine and vice versa.

`metaobjects` (per-type queries, ordered definition→entry imports) and
`delivery_profiles` (nesting exceeds the bulk API's two-level connection limit)
keep the paginated/per-record path.

Notes:
- Pass `--no-bulk` to `export`/`import` to force the legacy paginated/per-record
  path — handy for tiny datasets, since a bulk operation has a fixed
  submit-poll-download overhead of a few seconds.
- Shopify runs one bulk query and one bulk mutation per shop at a time; if
  another operation is already running, shoptools reports the error and exits.
- Bulk plumbing (submit, poll with backoff, staged upload, JSONL reassembly)
  lives in `src/bulk.rs`.

## Reading order (to learn the codebase)

`src/lib.rs` (the map) → `src/cli.rs` (the CLI shape) → `src/config.rs` (the
fully-worked reference: structs, serde, errors) → `src/commands/store.rs` (a
real command) → then the stubs above.

## A note on warnings

Until you implement the stubs you'll see warnings like *"unreachable
expression"* (code after a `todo!()`) — that's expected. They disappear as you
replace each `todo!()`. Run `cargo build` often; the compiler is your guide.

## Later (deliberately deferred)

- Blocking → async (`tokio`) for cloning resources concurrently.
- CSV export/import alongside JSON.
- **Orders** — `orderCreate` exists, but orders can't be reproduced faithfully
  (new ids/numbers, approximate transactions/fulfillments, single discount code,
  and a 5-orders/min cap on dev stores). Deferred until the volume/fidelity
  tradeoff is decided; the pieces it needs (customer-by-email + throttle retry)
  are already in place.
- More resources (collections, pages, redirects, …). Note: `discounts` already
  reference collections by handle, so collections must exist in the target until
  a `collections` resource lands.
