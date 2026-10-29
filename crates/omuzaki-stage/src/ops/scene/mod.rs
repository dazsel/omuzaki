//! The `stage.*` scene ops (design D8): one op per intent, one patch per op. `edit` holds what
//! every edit of an existing scene shares; each other submodule is one op or one family of ops (lights, cameras, cast,
//! props).

pub mod camera;
pub mod cast;
pub mod create;
pub(crate) mod edit;
pub mod light;
pub mod list;
pub mod look;
pub mod manage;
pub mod prop;
pub mod set_default;
pub mod stage_preset;

use omuzaki_doc_api::{DocPath, DocRead, Patch, Value};

/// The patch that sets `path` to `value`, creating the missing tables above it in the same step, so the first scene
/// of a project (with no `scenes` or `project` table yet) is still one patch with an exact inverse.
pub(crate) fn put(doc: &dyn DocRead, path: &DocPath, value: Value) -> Patch {
    let mut at = path.clone();
    let mut value = value;
    // Climb while the parent is missing, wrapping the value in each missing table.
    while let Some((parent, key)) = at.split_last() {
        if parent.segments().is_empty() || doc.get(&parent).is_some() {
            break;
        }
        let mut table = serde_json::Map::new();
        table.insert(key.to_owned(), value);
        value = Value::Object(table);
        at = parent;
    }
    Patch::new().with(at.clone(), doc.get(&at).cloned(), Some(value))
}
