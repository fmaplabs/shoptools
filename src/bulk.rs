//! Shopify **Bulk Operations** plumbing — the server-side alternative to the
//! client-side cursor pagination in `client.rs`.
//!
//! Two public entry points, both *blocking* (no async — the whole crate uses
//! blocking reqwest):
//!   * [`bulk_query`]  — submit a `bulkOperationRunQuery`, poll it to completion,
//!     download the JSONL result and parse it into a flat `Vec<Value>`.
//!   * [`bulk_mutation`] — upload a JSONL variables file, submit a
//!     `bulkOperationRunMutation`, poll, download and return the per-line results.
//!
//! Bulk queries emit *nested* connection nodes as separate flattened JSONL lines
//! carrying a `__parentId`. [`reassemble`] rebuilds today's nested export shape
//! (`variants: { nodes: [...] }` etc.) from those flat lines so on-disk files are
//! unchanged. It is a pure function — unit-tested at the bottom without a client.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::client::ShopifyClient;

/// Submit a bulk query, then poll it to completion (validated ✅ 2026-07).
const RUN_BULK_EXPORT: &str = r#"
mutation RunBulkExport($query: String!) {
  bulkOperationRunQuery(query: $query, groupObjects: false) {
    bulkOperation { id status }
    userErrors { field message }
  }
}
"#;

/// Poll a bulk operation by id. Used for both queries and mutations — the
/// `BulkOperation` object is the same for either (validated ✅ 2026-07).
const BULK_OPERATION_STATUS: &str = r#"
query BulkOperationStatus($id: ID!) {
  node(id: $id) {
    ... on BulkOperation {
      id
      status
      errorCode
      objectCount
      rootObjectCount
      url
      partialDataUrl
    }
  }
}
"#;

/// Reserve a pre-signed upload target for the bulk-mutation variables file
/// (validated ✅ 2026-07).
const STAGED_UPLOAD_CREATE: &str = r#"
mutation StagedUploadForBulkImport($input: [StagedUploadInput!]!) {
  stagedUploadsCreate(input: $input) {
    stagedTargets { url resourceUrl parameters { name value } }
    userErrors { field message }
  }
}
"#;

/// Run a mutation once per line of the uploaded JSONL file (validated ✅ 2026-07).
const RUN_BULK_IMPORT: &str = r#"
mutation RunBulkImport($mutation: String!, $stagedUploadPath: String!) {
  bulkOperationRunMutation(mutation: $mutation, stagedUploadPath: $stagedUploadPath) {
    bulkOperation { id status }
    userErrors { field message }
  }
}
"#;

/// Overall wall-clock budget for a bulk operation. On exceeding it we bail with
/// the operation id so the user can inspect or cancel it in Shopify.
const POLL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Shopify rejects staged-upload variables files larger than 100 MB.
const MAX_JSONL_BYTES: usize = 100 * 1024 * 1024;

/// Maps a nested connection's child `__typename` to the parent field the
/// reassembled children live under. For products, a `ProductVariant` child goes
/// to the parent's `variants` field (→ `variants: { nodes: [...] }`). Pass one
/// spec per nested connection; when a resource has more than one, each child
/// line must select `__typename` so [`reassemble`] can route it.
pub struct ChildSpec {
    /// The child node's GraphQL `__typename`, e.g. `"ProductVariant"`.
    pub typename: &'static str,
    /// The destination field on the parent, e.g. `"variants"`.
    pub field: &'static str,
}

/// Submit a bulk **query**, poll to completion, download and parse the JSONL.
///
/// `query` is a plain GraphQL query body (no `first`/`after`/`pageInfo`) that
/// selects at least one connection as `edges { node { … } }`. Returns the flat
/// JSONL lines — call [`reassemble`] to rebuild nested shapes. A `COMPLETED`
/// operation with a null `url` (empty result set) yields an empty vec.
pub fn bulk_query(client: &ShopifyClient, query: &str) -> Result<Vec<Value>> {
    let data = client.graphql(RUN_BULK_EXPORT, json!({ "query": query }))?;
    let payload = &data["bulkOperationRunQuery"];
    bail_on_user_errors(payload, "bulkOperationRunQuery")?;

    let id = payload["bulkOperation"]["id"]
        .as_str()
        .context("bulkOperationRunQuery returned no bulkOperation id")?
        .to_string();

    let op = poll_until_done(client, &id)?;
    download_result(client, &op, &id, "bulk query")
}

/// Submit a bulk **mutation**: upload `lines` as a JSONL variables file, run
/// `mutation` once per line, poll, and return the per-line result `Value`s.
///
/// Each entry in `lines` is the `variables` object for one invocation of
/// `mutation` (e.g. `{ "input": { … } }`). Errors if the serialized JSONL
/// exceeds 100 MB. Per-line `userErrors` are *not* fatal here — they come back
/// inside the returned result values for the caller to report per record.
pub fn bulk_mutation(
    client: &ShopifyClient,
    mutation: &str,
    lines: &[Value],
) -> Result<Vec<Value>> {
    let jsonl = to_jsonl(lines)?;
    if jsonl.len() > MAX_JSONL_BYTES {
        bail!(
            "bulk import payload is {} bytes, over the 100 MB staged-upload limit",
            jsonl.len()
        );
    }

    let staged_path = stage_upload(client, jsonl)?;

    let data = client.graphql(
        RUN_BULK_IMPORT,
        json!({ "mutation": mutation, "stagedUploadPath": staged_path }),
    )?;
    let payload = &data["bulkOperationRunMutation"];
    bail_on_user_errors(payload, "bulkOperationRunMutation")?;

    let id = payload["bulkOperation"]["id"]
        .as_str()
        .context("bulkOperationRunMutation returned no bulkOperation id")?
        .to_string();

    let op = poll_until_done(client, &id)?;
    download_result(client, &op, &id, "bulk mutation")
}

/// Upload `jsonl` to a fresh staged target and return its `stagedUploadPath`
/// (the `key` parameter), for handing to `bulkOperationRunMutation`.
fn stage_upload(client: &ShopifyClient, jsonl: String) -> Result<String> {
    let input = json!([{
        "resource": "BULK_MUTATION_VARIABLES",
        "filename": "bulk_op_vars.jsonl",
        "mimeType": "text/jsonl",
        "httpMethod": "POST",
    }]);
    let data = client.graphql(STAGED_UPLOAD_CREATE, json!({ "input": input }))?;
    let payload = &data["stagedUploadsCreate"];
    bail_on_user_errors(payload, "stagedUploadsCreate")?;

    let target = &payload["stagedTargets"][0];
    let url = target["url"]
        .as_str()
        .context("stagedUploadsCreate returned no target url")?;
    let params = target["parameters"]
        .as_array()
        .context("stagedUploadsCreate returned no parameters")?;
    let staged_path =
        staged_upload_path(params).context("staged-upload parameters missing a 'key' entry")?;

    // Multipart POST: every parameter as a text field first, the file part last.
    let mut form = reqwest::blocking::multipart::Form::new();
    for p in params {
        let name = p["name"]
            .as_str()
            .context("staged-upload parameter missing a name")?
            .to_string();
        let value = p["value"].as_str().unwrap_or("").to_string();
        form = form.text(name, value);
    }
    let part = reqwest::blocking::multipart::Part::bytes(jsonl.into_bytes())
        .file_name("bulk_op_vars.jsonl")
        .mime_str("text/jsonl")
        .context("building multipart file part")?;
    form = form.part("file", part);

    let resp = client
        .http()
        .post(url)
        .multipart(form)
        .send()
        .context("uploading bulk-mutation variables file")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        bail!("staged upload POST failed ({status}): {body}");
    }

    Ok(staged_path)
}

/// Poll `id` until it reaches a terminal status, sleeping with capped backoff
/// (1s, 2s, 4s, then 10s) and printing `objectCount` progress to stderr.
fn poll_until_done(client: &ShopifyClient, id: &str) -> Result<Value> {
    let start = Instant::now();
    let mut backoff: u64 = 1;

    loop {
        let data = client.graphql(BULK_OPERATION_STATUS, json!({ "id": id }))?;
        let node = data["node"].clone();
        let status = node["status"].as_str().unwrap_or("").to_string();

        match status.as_str() {
            "COMPLETED" | "FAILED" | "CANCELED" | "CANCELING" | "EXPIRED" => return Ok(node),
            _ => {}
        }

        eprintln!(
            "  bulk {id}: {status} ({} objects)",
            count_str(&node["objectCount"])
        );

        if start.elapsed() > POLL_TIMEOUT {
            bail!(
                "bulk operation {id} still {status} after {} minutes; inspect or cancel it in Shopify",
                POLL_TIMEOUT.as_secs() / 60
            );
        }

        std::thread::sleep(Duration::from_secs(backoff));
        backoff = if backoff < 4 { backoff * 2 } else { 10 };
    }
}

/// Interpret a terminal bulk operation `op` and return its downloaded, parsed
/// result. `label`/`id` are for error context.
fn download_result(
    client: &ShopifyClient,
    op: &Value,
    id: &str,
    label: &str,
) -> Result<Vec<Value>> {
    match op["status"].as_str() {
        Some("COMPLETED") => match op["url"].as_str() {
            // A null/empty url means the result set was empty.
            None | Some("") => Ok(Vec::new()),
            Some(url) => {
                let text = client.download(url)?;
                parse_jsonl(&text)
            }
        },
        Some("FAILED") => bail!("{label} {id} failed with errorCode {}", op["errorCode"]),
        other => bail!("{label} {id} ended in unexpected status {other:?}"),
    }
}

/// Rebuild the nested export shape from flat JSONL lines.
///
/// Lines *without* `__parentId` are roots, kept in input order and indexed by
/// their `id`. Lines *with* `__parentId` are appended to
/// `parent[spec.field]["nodes"]`, where `spec` is chosen by the child's
/// `__typename` (or the sole spec when there's exactly one). Transport-only keys
/// are stripped so output matches today's files: `__parentId` and `__typename`
/// from every node, and `id` from child nodes (roots keep their `id`).
///
/// Supports a single level of child nesting (the common case: products →
/// variants, customers → storeCreditAccounts). Deeper nesting is a wave-2
/// concern handled per-resource.
pub fn reassemble(lines: Vec<Value>, children: &[ChildSpec]) -> Vec<Value> {
    let mut roots: Vec<Value> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for mut line in lines {
        let parent_id = line
            .get("__parentId")
            .and_then(Value::as_str)
            .map(str::to_string);

        match parent_id {
            Some(parent_id) => {
                let typename = line.get("__typename").and_then(Value::as_str);
                let spec = match children {
                    [only] => Some(only),
                    _ => typename.and_then(|t| children.iter().find(|s| s.typename == t)),
                };
                let Some(spec) = spec else {
                    // Unroutable child (unknown __typename / no specs): drop it.
                    continue;
                };
                strip_transport(&mut line, true);
                if let Some(&i) = index.get(&parent_id) {
                    let field = &mut roots[i][spec.field];
                    if !field.is_object() {
                        *field = json!({ "nodes": [] });
                    }
                    if let Some(nodes) = field["nodes"].as_array_mut() {
                        nodes.push(line);
                    }
                }
            }
            None => {
                let id = line.get("id").and_then(Value::as_str).map(str::to_string);
                strip_transport(&mut line, false);
                let i = roots.len();
                roots.push(line);
                if let Some(id) = id {
                    index.insert(id, i);
                }
            }
        }
    }

    roots
}

/// Remove transport-only keys added by the bulk export. `is_child` nodes also
/// shed their `id` (child shapes like variants don't carry one on disk); roots
/// keep `id`.
fn strip_transport(line: &mut Value, is_child: bool) {
    if let Some(obj) = line.as_object_mut() {
        obj.remove("__parentId");
        obj.remove("__typename");
        if is_child {
            obj.remove("id");
        }
    }
}

/// Serialize `lines` to a newline-delimited JSON buffer (one compact object per
/// line), the format Shopify's staged-upload endpoint expects.
fn to_jsonl(lines: &[Value]) -> Result<String> {
    let mut buf = String::new();
    for (i, line) in lines.iter().enumerate() {
        let encoded =
            serde_json::to_string(line).with_context(|| format!("serializing JSONL line {i}"))?;
        buf.push_str(&encoded);
        buf.push('\n');
    }
    Ok(buf)
}

/// Parse a newline-delimited JSON document into one `Value` per non-blank line.
fn parse_jsonl(text: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(line).with_context(|| format!("parsing JSONL line {}", i + 1))?;
        out.push(value);
    }
    Ok(out)
}

/// Extract the `key` parameter from a staged-upload target's `parameters` list;
/// it is the `stagedUploadPath` passed to `bulkOperationRunMutation`.
fn staged_upload_path(params: &[Value]) -> Option<String> {
    params
        .iter()
        .find(|p| p["name"].as_str() == Some("key"))
        .and_then(|p| p["value"].as_str())
        .map(str::to_string)
}

/// `objectCount` is an `UnsignedInt64`, which JSON-encodes as a string; fall back
/// to a numeric encoding just in case.
fn count_str(v: &Value) -> String {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
        .unwrap_or_else(|| "0".to_string())
}

/// Bail with a readable message if `payload.userErrors` is a non-empty array.
/// Surfaces Shopify's "a bulk operation is already running" the same way.
fn bail_on_user_errors(payload: &Value, op: &str) -> Result<()> {
    if let Some(errors) = payload["userErrors"].as_array()
        && !errors.is_empty()
    {
        bail!("{op} returned userErrors: {}", payload["userErrors"]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassemble_single_child_type() {
        // Products → variants: one root, two variant children (one carries the
        // extra id/__typename the bulk export adds; both must be stripped).
        let lines = vec![
            json!({ "id": "gid://Product/1", "handle": "tee", "title": "Tee" }),
            json!({
                "id": "gid://Variant/11", "__typename": "ProductVariant",
                "__parentId": "gid://Product/1", "sku": "TEE-S", "price": "19.99"
            }),
            json!({
                "id": "gid://Variant/12", "__typename": "ProductVariant",
                "__parentId": "gid://Product/1", "sku": "TEE-M", "price": "19.99"
            }),
        ];
        let specs = [ChildSpec {
            typename: "ProductVariant",
            field: "variants",
        }];

        let out = reassemble(lines, &specs);

        assert_eq!(out.len(), 1);
        let product = &out[0];
        // Root keeps its id; transport keys never appear on it.
        assert_eq!(product["id"], "gid://Product/1");
        assert_eq!(product["handle"], "tee");
        // Children nested under variants.nodes, in order.
        let variants = product["variants"]["nodes"].as_array().unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0]["sku"], "TEE-S");
        assert_eq!(variants[1]["sku"], "TEE-M");
        // Transport-only keys stripped from children (incl. their own id).
        for v in variants {
            assert!(v.get("id").is_none());
            assert!(v.get("__typename").is_none());
            assert!(v.get("__parentId").is_none());
        }
    }

    #[test]
    fn reassemble_multi_child_type() {
        // A resource with two nested connections: children route by __typename.
        let lines = vec![
            json!({ "id": "gid://Discount/1", "title": "Sale" }),
            json!({ "__typename": "DiscountRedeemCode", "__parentId": "gid://Discount/1", "code": "SAVE10" }),
            json!({ "__typename": "Collection", "__parentId": "gid://Discount/1", "handle": "shoes" }),
            json!({ "__typename": "Collection", "__parentId": "gid://Discount/1", "handle": "hats" }),
        ];
        let specs = [
            ChildSpec {
                typename: "DiscountRedeemCode",
                field: "codes",
            },
            ChildSpec {
                typename: "Collection",
                field: "collections",
            },
        ];

        let out = reassemble(lines, &specs);

        assert_eq!(out.len(), 1);
        let d = &out[0];
        let codes = d["codes"]["nodes"].as_array().unwrap();
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0]["code"], "SAVE10");
        let collections = d["collections"]["nodes"].as_array().unwrap();
        assert_eq!(collections.len(), 2);
        assert_eq!(collections[0]["handle"], "shoes");
        assert_eq!(collections[1]["handle"], "hats");
    }

    #[test]
    fn parse_jsonl_skips_blank_lines() {
        let text = "{\"a\":1}\n\n{\"b\":2}\n";
        let parsed = parse_jsonl(text).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["a"], 1);
        assert_eq!(parsed[1]["b"], 2);
    }

    #[test]
    fn parse_jsonl_reports_bad_line() {
        let err = parse_jsonl("{\"ok\":1}\nnot json\n").unwrap_err();
        assert!(err.to_string().contains("line 2"));
    }

    #[test]
    fn to_jsonl_is_newline_delimited() {
        let lines = vec![json!({ "input": { "handle": "a" } }), json!({ "x": 1 })];
        let buf = to_jsonl(&lines).unwrap();
        assert_eq!(buf, "{\"input\":{\"handle\":\"a\"}}\n{\"x\":1}\n");
    }

    #[test]
    fn extracts_staged_upload_key() {
        let params = vec![
            json!({ "name": "Content-Type", "value": "text/jsonl" }),
            json!({ "name": "key", "value": "tmp/12345/bulk_op_vars.jsonl" }),
            json!({ "name": "policy", "value": "abc" }),
        ];
        assert_eq!(
            staged_upload_path(&params).as_deref(),
            Some("tmp/12345/bulk_op_vars.jsonl")
        );
    }

    #[test]
    fn missing_staged_upload_key_is_none() {
        let params = vec![json!({ "name": "policy", "value": "abc" })];
        assert!(staged_upload_path(&params).is_none());
    }
}
