//! Reading application records from the document, and the source row an item's part gets (design D5, D8).

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{DocPath, DocRead, Value};
use omuzaki_library_api::{APPLIED_KEY, ApplicationRecord, ItemId, ItemRights, Version};
use serde_json::json;

/// The schema of a part source row (`omuzaki-io-ops`'s `PartSource`, which `licence.derive` reads).
const SOURCE_ROW_SCHEMA: u32 = 1;

/// Every record of `character`, by application key, in key order. A node that isn't a record of this build is skipped
/// with a warning: it is still in the document, and `library.lock` names it.
pub(crate) fn of_character(doc: &dyn DocRead, character: &CharId) -> Vec<(String, ApplicationRecord)> {
    let Some(Value::Object(applied)) = doc.get(&paths::character(character).join(APPLIED_KEY)) else {
        return Vec::new();
    };
    applied
        .iter()
        .filter_map(|(app, node)| match ApplicationRecord::from_node(node) {
            Ok(record) => Some((app.clone(), record)),
            Err(error) => {
                tracing::warn!(%character, %app, %error, "an application record this build can't read");
                None
            }
        })
        .collect()
}

/// The characters of the document, in id order.
pub(crate) fn characters(doc: &dyn DocRead) -> Vec<CharId> {
    let Some(Value::Object(all)) = doc.get(&DocPath::root().join("characters")) else { return Vec::new() };
    all.keys().filter_map(|id| CharId::new(id.as_str()).ok()).collect()
}

/// The record of `app` on `character`.
pub(crate) fn one(doc: &dyn DocRead, character: &CharId, app: &str) -> Option<ApplicationRecord> {
    of_character(doc, character).into_iter().find(|(key, _)| key == app).map(|(_, record)| record)
}

/// `value` without its null members, as the project files hold a row.
fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, without_nulls(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(without_nulls).collect()),
        other => other,
    }
}

/// The source row of a part `item@version` created (design D8): its provenance, its terms and, for an owned item,
/// who asserts the ownership (the item, for first-party items).
pub(crate) fn source_row(rights: &ItemRights, item: &ItemId, version: &Version) -> Value {
    let provenance = serde_json::to_value(rights.to_provenance(item, version)).unwrap_or(Value::Null);
    let mut row = json!({ "schema": SOURCE_ROW_SCHEMA, "provenance": provenance });
    if let Some(terms) = &rights.terms {
        row["terms"] = serde_json::to_value(terms).unwrap_or(Value::Null);
    }
    if rights.owned {
        let by = if item.is_first_party() {
            format!("Omuzaki first-party library item {item}@{version}")
        } else {
            format!("library item {item}@{version}")
        };
        row["owned"] = json!({ "asserted_by": by });
    }
    without_nulls(row)
}
