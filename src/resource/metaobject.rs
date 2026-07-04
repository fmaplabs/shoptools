//! Metaobjects — the trickiest resource, for three reasons:
//!   1. A metaobject is **schema + data**: the *definition* (which fields exist,
//!      of what type) and the *entries* (actual values). Export captures both,
//!      so it returns an OBJECT `{ definitions, objects }`, not a flat array.
//!   2. Import is **two-phase**: `metaobjectCreate` requires the definition to
//!      already exist, so we create every definition first, then the entries.
//!   3. Fields can be **references** to other resources (a product, collection,
//!      or another metaobject). Those are stored as store-specific ids, so we
//!      capture the referenced object's stable *handle* on export and resolve it
//!      against the target store on import — the same remap as collection-scoped
//!      discounts.
//!
//! Export makes several queries: one for the definitions, then one `metaobjects`
//! query per type (the API requires a `type` argument, so you can't list all at
//! once).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Resource;
use crate::client::ShopifyClient;

pub struct Metaobject;

// ---- Typed DTOs for the import side ------------------------------------------

/// The whole export payload: schema + data together.
#[derive(Debug, Deserialize, Serialize)]
struct MetaobjectExport {
    definitions: Vec<DefinitionRecord>,
    objects: Vec<ObjectRecord>,
}

/// A metaobject *definition* (the schema for one type).
#[derive(Debug, Deserialize, Serialize)]
struct DefinitionRecord {
    name: Option<String>,
    #[serde(rename = "type")]
    type_name: String,
    #[serde(rename = "fieldDefinitions")]
    field_definitions: Vec<FieldDefinitionRecord>,
}

/// One field in a definition. Read shape nests the type as `{ name }`; the
/// create input wants a bare string — we translate in `create_definition`.
#[derive(Debug, Deserialize, Serialize)]
struct FieldDefinitionRecord {
    key: String,
    name: Option<String>,
    #[serde(rename = "type")]
    field_type: FieldType,
}

#[derive(Debug, Deserialize, Serialize)]
struct FieldType {
    name: String,
}

/// A metaobject *entry* (one row of data). `type` isn't returned by the
/// `metaobjects` query — export injects it so import knows which definition
/// this entry belongs to.
#[derive(Debug, Deserialize, Serialize)]
struct ObjectRecord {
    #[serde(rename = "type")]
    type_name: String,
    handle: Option<String>,
    fields: Vec<FieldValue>,
}

/// A metaobject field value. The Admin API stores every value as a string
/// (`jsonValue` gives the typed form); `value` is nullable, hence Option.
#[derive(Debug, Deserialize, Serialize)]
struct FieldValue {
    key: String,
    value: Option<String>,
    /// For a single-reference field, the referenced object as
    /// `{ __typename, handle, type? }`. Null (→ None) for scalar/list fields.
    reference: Option<Value>,
    /// For a list-reference field, `{ nodes: [ { __typename, handle, type? } ] }`.
    /// Null (→ None) for scalar/single-reference fields.
    references: Option<Value>,
}

// ---- GraphQL -----------------------------------------------------------------

const DEFINITIONS_QUERY: &str = r#"
query Definitions($cursor: String) {
  metaobjectDefinitions(first: 50, after: $cursor) {
    nodes {
      name
      type
      fieldDefinitions { key name type { name } }
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

const OBJECTS_QUERY: &str = r#"
query Objects($type: String!, $cursor: String) {
  metaobjects(type: $type, first: 50, after: $cursor) {
    nodes {
      id
      handle
      fields {
        key
        value
        reference {
          __typename
          ... on Product { handle }
          ... on Collection { handle }
          ... on Metaobject { type handle }
        }
        references(first: 250) {
          nodes {
            __typename
            ... on Product { handle }
            ... on Collection { handle }
            ... on Metaobject { type handle }
          }
          pageInfo { hasNextPage endCursor }
        }
      }
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

/// Follow-up query to page ONE field's reference list when the inline page in
/// `export` overflows. Keyed by metaobject id + field key.
const FIELD_REFS: &str = r#"
query FieldRefs($id: ID!, $key: String!, $cursor: String) {
  metaobject(id: $id) {
    field(key: $key) {
      references(first: 250, after: $cursor) {
        nodes {
          __typename
          ... on Product { handle }
          ... on Collection { handle }
          ... on Metaobject { type handle }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
"#;

const DEFINITION_CREATE: &str = r#"
mutation CreateDefinition($definition: MetaobjectDefinitionCreateInput!) {
  metaobjectDefinitionCreate(definition: $definition) {
    metaobjectDefinition { type }
    userErrors { field message code }
  }
}
"#;

const OBJECT_CREATE: &str = r#"
mutation CreateObject($metaobject: MetaobjectCreateInput!) {
  metaobjectCreate(metaobject: $metaobject) {
    metaobject { handle }
    userErrors { field message code }
  }
}
"#;

impl Resource for Metaobject {
    fn name(&self) -> &'static str {
        "metaobjects"
    }

    fn export(&self, client: &ShopifyClient) -> Result<Value> {
        // Phase 1: the definitions (the schema), across all pages.
        let definitions = client.paginate(DEFINITIONS_QUERY, json!({}), "metaobjectDefinitions")?;

        // Phase 2: for each definition's type, fetch its entries. `metaobjects`
        // needs a `type` argument, so we paginate once per type and accumulate.
        let mut objects: Vec<Value> = Vec::new();
        for def in &definitions {
            let type_name = def["type"]
                .as_str()
                .context("metaobject definition is missing its type")?;

            let mut entries =
                client.paginate(OBJECTS_QUERY, json!({ "type": type_name }), "metaobjects")?;

            for obj in &mut entries {
                fixup_truncated_references(client, obj)?;
                // The query doesn't echo the type back, so stamp it on each
                // entry — import needs it to pick the definition.
                obj["type"] = json!(type_name);
                objects.push(obj.clone());
            }
        }

        Ok(json!({ "definitions": Value::Array(definitions), "objects": objects }))
    }

    fn import(&self, client: &ShopifyClient, data: &Value, dry_run: bool) -> Result<()> {
        let export: MetaobjectExport = serde_json::from_value(data.clone())
            .context("import data was not a metaobject export ({definitions, objects})")?;

        println!(
            "{} definition(s), {} object(s) to import",
            export.definitions.len(),
            export.objects.len()
        );

        // PHASE 1 — definitions must exist before any entry that uses them.
        for def in &export.definitions {
            if dry_run {
                println!(
                    "  would create definition: {} ({} field(s))",
                    def.type_name,
                    def.field_definitions.len()
                );
                continue;
            }
            create_definition(client, def)?;
        }

        // PHASE 2 — the entries themselves.
        for obj in &export.objects {
            if dry_run {
                println!(
                    "  would create {}: {}",
                    obj.type_name,
                    obj.handle.as_deref().unwrap_or("(auto handle)")
                );
                continue;
            }
            // Best-effort: a missing reference target skips one entry, not the run.
            if let Err(e) = create_object(client, obj) {
                println!("  entry '{}' skipped: {e:#}", obj.handle.as_deref().unwrap_or("?"));
            }
        }

        Ok(())
    }
}

/// If any of `obj`'s reference-list fields was truncated at the inline page
/// limit, refetch that field's full reference list by metaobject id + key and
/// splice it back in. No-op for the common case (lists under the page limit).
fn fixup_truncated_references(client: &ShopifyClient, obj: &mut Value) -> Result<()> {
    let id = obj["id"].as_str().map(str::to_string);
    let field_count = obj["fields"].as_array().map_or(0, |a| a.len());

    for i in 0..field_count {
        let truncated =
            obj["fields"][i]["references"]["pageInfo"]["hasNextPage"].as_bool() == Some(true);
        if !truncated {
            continue;
        }
        let (Some(id), Some(key)) = (&id, obj["fields"][i]["key"].as_str().map(str::to_string))
        else {
            continue;
        };
        let all = client.paginate_nested(FIELD_REFS, json!({ "id": id, "key": key }), |d| {
            d["metaobject"]["field"]["references"].clone()
        })?;
        obj["fields"][i]["references"] = json!({ "nodes": all });
    }
    Ok(())
}

fn create_definition(client: &ShopifyClient, def: &DefinitionRecord) -> Result<()> {
    // Translate read shape -> create input: `type { name }` becomes `type: <str>`.
    let field_defs: Vec<Value> = def
        .field_definitions
        .iter()
        .map(|fd| {
            json!({
                "key": fd.key,
                "name": fd.name,
                "type": fd.field_type.name,
            })
        })
        .collect();

    let input = json!({
        "name": def.name,
        "type": def.type_name,
        "fieldDefinitions": field_defs,
    });

    let result = client.graphql(DEFINITION_CREATE, json!({ "definition": input }))?;
    let payload = &result["metaobjectDefinitionCreate"];

    // A definition that already exists returns a "type is taken" userError. For a
    // clone that's fine — we warn and keep going rather than bail, so re-running
    // the clone (or a target that already has the schema) still imports entries.
    if let Some(errors) = payload["userErrors"].as_array() {
        if !errors.is_empty() {
            println!("  definition '{}' skipped: {}", def.type_name, payload["userErrors"]);
            return Ok(());
        }
    }
    println!("  created definition {}", def.type_name);
    Ok(())
}

fn create_object(client: &ShopifyClient, obj: &ObjectRecord) -> Result<()> {
    // Resolve each field's value, remapping references to the target store.
    // A field with no value is skipped.
    let mut fields: Vec<Value> = Vec::new();
    for f in &obj.fields {
        if let Some(value) = resolve_field_value(client, f)? {
            fields.push(json!({ "key": f.key, "value": value }));
        }
    }

    let mut input = json!({
        "type": obj.type_name,
        "fields": fields,
    });
    // Reuse the source handle when present, so entries match across stores.
    if let Some(handle) = &obj.handle {
        input["handle"] = json!(handle);
    }

    let result = client.graphql(OBJECT_CREATE, json!({ "metaobject": input }))?;
    let payload = &result["metaobjectCreate"];

    if let Some(errors) = payload["userErrors"].as_array() {
        if !errors.is_empty() {
            bail!("{}", payload["userErrors"]);
        }
    }
    println!(
        "  created {} {}",
        obj.type_name,
        obj.handle.as_deref().unwrap_or("")
    );
    Ok(())
}

/// Produce the create-input value for a field, remapping references from the
/// source store's ids to the target store's ids by stable handle. Returns None
/// for a field with no value (skip it).
fn resolve_field_value(client: &ShopifyClient, f: &FieldValue) -> Result<Option<String>> {
    // Single reference: the value becomes the target object's id.
    if let Some(reference) = &f.reference {
        return Ok(Some(resolve_reference(client, reference)?));
    }
    // List reference: the value is a JSON-encoded array of target ids.
    if let Some(references) = &f.references {
        if let Some(nodes) = references["nodes"].as_array() {
            let mut ids = Vec::new();
            for node in nodes {
                ids.push(resolve_reference(client, node)?);
            }
            return Ok(Some(serde_json::to_string(&ids)?));
        }
    }
    // Scalar: pass the raw value through unchanged.
    Ok(f.value.clone())
}

/// Remap one reference object (`{ __typename, handle, type? }`) to the target
/// store's global id, by resolving its stable handle.
fn resolve_reference(client: &ShopifyClient, r: &Value) -> Result<String> {
    let typename = r["__typename"]
        .as_str()
        .context("reference is missing __typename")?;
    match typename {
        "Product" => {
            let handle = r["handle"].as_str().context("product reference missing handle")?;
            client.resolve_product(handle)
        }
        "Collection" => {
            let handle = r["handle"]
                .as_str()
                .context("collection reference missing handle")?;
            client.resolve_collection(handle)
        }
        "Metaobject" => {
            let type_name = r["type"]
                .as_str()
                .context("metaobject reference missing type")?;
            let handle = r["handle"]
                .as_str()
                .context("metaobject reference missing handle")?;
            client.resolve_metaobject(type_name, handle)
        }
        other => bail!(
            "reference to '{other}' can't be remapped across stores yet \
             (only Product / Collection / Metaobject references are supported)"
        ),
    }
}
