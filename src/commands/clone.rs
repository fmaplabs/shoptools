//! `shoptools clone --from A --to B` — the headline feature: export each resource
//! from store A and import it into store B.
//!
//! The credential/resource setup is done. The loop body — export from source,
//! import into target — is yours. It's literally `export` followed by `import`,
//! which is why those come first.

use anyhow::Result;

use crate::client::ShopifyClient;
use crate::config::Config;
use crate::resource;

pub fn run(from: &str, to: &str, only: &[String], dry_run: bool) -> Result<()> {
    // Load once, then look up both stores. `.clone()` because we need to *own*
    // each credential to hand to `ShopifyClient::new` (which consumes it).
    let config = Config::load()?;
    let from_cred = config.get(Some(from))?.clone();
    let to_cred = config.get(Some(to))?.clone();

    let from_client = ShopifyClient::new(from_cred)?;
    let to_client = ShopifyClient::new(to_cred)?;

    // Which resources? The named subset, or all known ones in dependency order.
    // `collect::<Result<Vec<_>>>()` turns a sequence of Results into one Result:
    // the first error short-circuits the whole thing.
    let resources = if only.is_empty() {
        resource::all()
    } else {
        only.iter()
            .map(|n| resource::by_name(n))
            .collect::<Result<Vec<_>>>()?
    };

    // TODO(you): copy each resource from source to target.
    //
    for res in &resources {
        let data = res.export(&from_client)?;
        res.import(&to_client, &data, dry_run)?;
        println!("cloned {}", res.name());
    }
    Ok(())
    //
    // ⚠ Order matters (see `resource::all`): metaobject/metafield *definitions*
    //   must exist in the target before values that reference them.
    // let _ = (&from_client, &to_client, &resources, dry_run);
    // todo!("export from the source store and import into the target store")
}
