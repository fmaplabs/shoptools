//! The `Resource` abstraction — the interesting bit of Rust design in this
//! project. Each Shopify resource type (products, discounts, metaobjects…)
//! implements the `Resource` *trait*, so `export`, `import`, and `clone` can
//! operate on any of them without knowing the specifics. Adding a new resource
//! type is a new file that implements this trait — no edits to the commands.

use anyhow::{bail, Result};
use serde_json::Value;

use crate::client::ShopifyClient;

/// A cloneable Shopify resource type. `&self` on every method (rather than
/// `self`) keeps the trait *object-safe*, which is what lets us store a mixed
/// list as `Vec<Box<dyn Resource>>` in `all()` below.
pub trait Resource {
    /// The name used on the command line, e.g. "products".
    fn name(&self) -> &'static str;

    /// Read all of this resource from `client`, returning a JSON array.
    /// Implementations paginate through the Admin API.
    fn export(&self, client: &ShopifyClient) -> Result<Value>;

    /// Write `data` (a JSON array, as produced by `export`) into `client`'s
    /// store. When `dry_run` is true, print what *would* happen and change nothing.
    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()>;
}

/// Look up a single resource implementation by its command-line name.
/// Returns a boxed trait object so the caller doesn't care which concrete type
/// it is — only that it's a `Resource`.
pub fn by_name(name: &str) -> Result<Box<dyn Resource>> {
    match name {
        "products" => Ok(Box::new(product::Product)),
        "discounts" => Ok(Box::new(discount::Discount)),
        "metaobjects" => Ok(Box::new(metaobject::Metaobject)),
        other => bail!("unknown resource '{other}' (known: products, discounts, metaobjects)"),
    }
}

/// Every known resource, in a safe order for cloning (dependencies first).
pub fn all() -> Vec<Box<dyn Resource>> {
    // NOTE: order matters for `clone`. Metaobject/metafield *definitions* must
    // exist before values that reference them; products may reference metafields.
    // Revisit this ordering as you implement the resources.
    vec![
        Box::new(metaobject::Metaobject),
        Box::new(product::Product),
        Box::new(discount::Discount),
    ]
}

pub mod discount;
pub mod metaobject;
pub mod product;
