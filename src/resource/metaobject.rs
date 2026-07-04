//! Metaobjects — the trickiest resource, for two reasons:
//!   1. A metaobject is **schema + data**: the *definition* (which fields exist,
//!      of what type) and the *entries* (actual values). Export captures both,
//!      so it returns an OBJECT `{ definitions, objects }`, not a flat array.
//!   2. Import is **two-phase**: `metaobjectCreate` requires the definition to
//!      already exist, so we create every definition first, then the entries.
//!
//! Export makes several queries: one for the definitions, then one `metaobjects`
//! query per type (the API requires a `type` argument, so you can't list all at
//! once).

use anyhow::{Context, Result};
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
      handle
      fields { key value }
    }
    pageInfo { hasNextPage endCursor }
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

            let entries =
                client.paginate(OBJECTS_QUERY, json!({ "type": type_name }), "metaobjects")?;
            for mut obj in entries {
                // The query doesn't echo the type back, so stamp it on each
                // entry — import needs it to pick the definition.
                obj["type"] = json!(type_name);
                objects.push(obj);
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
            create_object(client, obj)?;
        }

        Ok(())
    }
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
    // Drop null-valued fields — the create input can't take a null value.
    let fields: Vec<Value> = obj
        .fields
        .iter()
        .filter_map(|f| f.value.as_ref().map(|v| json!({ "key": f.key, "value": v })))
        .collect();

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
            println!(
                "  entry '{}' skipped: {}",
                obj.handle.as_deref().unwrap_or("?"),
                payload["userErrors"]
            );
            return Ok(());
        }
    }
    println!(
        "  created {} {}",
        obj.type_name,
        obj.handle.as_deref().unwrap_or("")
    );
    Ok(())
}
