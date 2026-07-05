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
- More resources (customers, collections, pages, …).
