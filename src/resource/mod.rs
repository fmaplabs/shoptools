//! The `Resource` abstraction — the interesting bit of Rust design in this
//! project. Each Shopify resource type (products, discounts, metaobjects…)
//! implements the `Resource` *trait*, so `export`, `import`, and `clone` can
//! operate on any of them without knowing the specifics. Adding a new resource
//! type is a new file that implements this trait — no edits to the commands.

use anyhow::{Result, bail};
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
        "customers" => Ok(Box::new(customer::Customer)),
        "delivery_profiles" => Ok(Box::new(delivery_profile::DeliveryProfile)),
        "giftcards" => Ok(Box::new(giftcard::GiftCard)),
        "store_credit" => Ok(Box::new(store_credit::StoreCredit)),
        other => bail!(
            "unknown resource '{other}' (known: products, discounts, metaobjects, \
             customers, delivery_profiles, giftcards, store_credit)"
        ),
    }
}

/// Every known resource, in a safe order for cloning (dependencies first).
pub fn all() -> Vec<Box<dyn Resource>> {
    // NOTE: order matters for `clone`.
    //   * Metaobject *definitions* must exist before values that reference them.
    //   * Customers must exist before gift cards / store credit (which reference a
    //     customer by email) and before orders (deferred).
    //   * Products come before delivery profiles so a later product-association
    //     feature has variants to resolve.
    // Pre-existing gap: discounts resolve *collections* by handle, but collections
    // aren't cloneable yet — they must already exist in the target.
    vec![
        Box::new(metaobject::Metaobject),
        Box::new(customer::Customer),
        Box::new(product::Product),
        Box::new(delivery_profile::DeliveryProfile),
        Box::new(discount::Discount),
        Box::new(giftcard::GiftCard),
        Box::new(store_credit::StoreCredit),
    ]
}

pub mod customer;
pub mod delivery_profile;
pub mod discount;
pub mod giftcard;
pub mod metaobject;
pub mod product;
pub mod store_credit;
