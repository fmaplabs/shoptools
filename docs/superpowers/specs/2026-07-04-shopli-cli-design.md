# shoptools — design

A personal Rust CLI for Shopify store development. Doubles as a hands-on Rust
learning project. This document is the agreed design; the code is scaffolded as
working reference modules plus guided stubs to implement.

## Goals

1. Learn Rust (Cargo, modules, crates, traits, error handling) by building
   something real.
2. A genuinely useful tool: query a store's Admin API, manage credentials for
   many stores/environments, export/import resources, and **clone** resources
   from one store into another environment.

## Non-goals (YAGNI)

- No OAuth server / app-install flow. `shoptools` never mints tokens; the user
  obtains an offline Admin API token however they like and hands it over.
- No async runtime initially. Blocking HTTP first; async is a later upgrade.
- No REST. Admin **GraphQL** only (metaobjects/metafields are first-class there,
  and REST is being retired for many resources).

## Authentication

- **Offline Admin API access tokens** (`shpat_…` or Partner-app equivalent).
  Offline = long-lived, not tied to a user session (online tokens expire ~24h).
- Provisioned by the user either via store admin → *Settings → Apps → Develop
  apps* (token handed over directly, no OAuth) or a Partner Dashboard custom
  distribution app (one-time OAuth install mints an offline token). `shoptools`
  treats the token as an opaque string; it does not care which path produced it.
- **Credential resolution chain** (first found wins), mirroring `aws`/`gh`/`docker`:
  1. `--token` flag
  2. role-scoped environment variables, explicit about which side of the data
     flow they belong to: `SHOPIFY_SOURCE_SHOP` + `SHOPIFY_SOURCE_TOKEN` for
     commands that read from a store (`query`, `export`), and
     `SHOPIFY_TARGET_SHOP` + `SHOPIFY_TARGET_TOKEN` for commands that write
     into one (`import`); both variables of a pair must be set
  3. stored config file entry (selected by `--store <name>` or the default store)

## Architecture

Library + thin binary. `src/lib.rs` is the crate root and holds all logic in
modules; `src/main.rs` is a ~15-line shell that parses args and calls
`shoptools::run()`. The library split makes the logic testable.

Module tree mirrors the dependency stack (lower modules know nothing of higher):

```
src/
├── main.rs            # parse args → shoptools::run() → print errors
├── lib.rs             # crate root: module declarations + run()
├── cli.rs             # clap: Cli struct + Command enum (parsing only)
├── config.rs          # StoreCredential; config load/save; resolution chain
├── client.rs          # ShopifyClient: reqwest wrapper, auth header, GraphQL POST
├── commands/
│   ├── mod.rs         # groups the command handlers
│   ├── query.rs       # `shoptools query`
│   ├── store.rs       # `shoptools store add/list/use/remove`
│   ├── export.rs      # `shoptools export`
│   ├── import.rs      # `shoptools import`
│   └── clone.rs       # `shoptools clone`  (export from A → import into B)
└── resource/
    ├── mod.rs         # `Resource` trait + registry of known resources
    ├── product.rs     # Resource impl for products
    ├── discount.rs    # Resource impl for discounts
    └── metaobject.rs  # Resource impl for metaobjects/metafields
```

The `Resource` trait is the key abstraction: `clone`/`export`/`import` iterate
over `Resource` implementors polymorphically, so adding a resource type is a new
file, not an edit to `clone`.

## Crates (added via `cargo add`)

| Crate      | Features                         | Role |
|------------|----------------------------------|------|
| `clap`     | `derive`                         | Arg parsing from typed structs/enums |
| `serde`    | `derive`                         | Serialization framework |
| `serde_json` | —                              | JSON backend (Admin API) |
| `toml`     | —                                | TOML backend (config file) |
| `reqwest`  | `blocking`, `json`, `rustls-tls` | HTTP client (rustls avoids system OpenSSL) |
| `anyhow`   | —                                | App-level errors, ergonomic `?` |
| `dirs`     | —                                | Cross-platform config dir |

## Command surface

```
shoptools store add <name> --shop <domain> [--token <tok>]
shoptools store list
shoptools store use <name>
shoptools store remove <name>
shoptools query <graphql> [--store <name>] [--json]
shoptools export <resource> [--store <name>] [--out <file>]
shoptools import <resource> --file <f> [--store <name>] [--dry-run]
shoptools clone --from <store> --to <store> [--only products,discounts] [--dry-run]
```

Mutating commands (`import`, `clone`) default to safe behavior; `--dry-run`
plans and prints instead of writing. A `dry_run: bool` is threaded to the
resource layer and honored in one place.

## Config file

- Location: `dirs::config_dir()/shoptools/config.toml` (e.g. `~/.config/shoptools/config.toml`).
- Shape:
  ```toml
  default = "acme-dev"

  [stores.acme-dev]
  shop = "acme-dev.myshopify.com"
  token = "shpat_…"

  [stores.acme-prod]
  shop = "acme-prod.myshopify.com"
  token = "shpat_…"
  ```

## Build arc (what ships working vs. as a guided stub)

Chosen ratio: **Balanced.**

- ✅ **Working + tested:** `config.rs` (the reference module), all `store`
  subcommands (no network — proves the tool is alive), and the compiling
  skeleton (`main.rs`, `lib.rs`, `cli.rs`).
- 🔨 **Guided stub (you implement):** `client.rs` + `commands/query.rs` first
  (your first live Admin API call), then `export`/`resource::product`, then
  `import`/`clone`/remaining resources. Stubs use `todo!()` + guiding comments,
  and where useful a failing test to make pass.

## Known challenges (captured, not yet solved)

- **Clone ordering / dependencies:** metafield & metaobject *definitions* must
  exist before *values*; products may reference metafields. `clone` must process
  resources in a dependency-aware order. Stubbed with notes.
- **Pagination:** Admin GraphQL returns pages (cursors). Export must paginate;
  the stub notes where the loop goes.
- **Rate limits:** Admin GraphQL uses a cost-based bucket. Out of scope for the
  first pass; a later concern (and a motivation for the async upgrade).

## Error handling & testing

- `anyhow::Result` at the application boundary; `?` propagates errors with
  context via `.context("…")`. Library-specific error types (`thiserror`) can
  come later if needed.
- `config.rs` ships with unit tests (resolution precedence, round-trip
  serialization). Stubs ship with a failing test that documents the target
  behavior.

## Future upgrades (explicitly deferred)

- Blocking → async (`tokio`) for concurrent resource cloning.
- CSV export/import (`csv` crate) alongside JSON.
- More resources (customers, collections, pages, redirects, …).
