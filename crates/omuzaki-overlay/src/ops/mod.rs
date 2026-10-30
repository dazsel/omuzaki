//! The ops: `overlay.*` edits the widgets (document edits, undoable) and drives them live (alerts, timers, goals);
//! `program.*` switches the program's scene with a transition and reports the compositor.

mod overlay;
mod program;

use omuzaki_doc_api::{DocPath, DocRead, Patch};
use omuzaki_ops_api::{OpCtx, OpError};
use serde_json::{Map, Value};

use crate::service::ProgramService;

/// The patch that writes `value` at `path`, making any missing parent tables on the way.
fn put(doc: &dyn DocRead, path: &DocPath, value: Value) -> Patch {
    let mut at = path.clone();
    let mut value = value;
    while let Some((parent, key)) = at.split_last() {
        if parent.segments().is_empty() || doc.get(&parent).is_some() {
            break;
        }
        let mut table = Map::new();
        table.insert(key.to_owned(), value);
        value = Value::Object(table);
        at = parent;
    }
    Patch::new().with(at.clone(), doc.get(&at).cloned(), Some(value))
}

/// RFC 7396 merge: `patch`'s fields replace `target`'s, objects merge field by field, and `null` removes a field.
fn merge(target: &mut Value, patch: &Value) {
    let Value::Object(fields) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(into) = target {
        for (key, value) in fields {
            if value.is_null() {
                into.remove(key);
            } else {
                merge(into.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    }
}

/// The compositor, for a live op.
fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a ProgramService, OpError> {
    ctx.service::<ProgramService>()
        .map_err(|_| OpError::unsupported("the program compositor isn't running here: open the studio to go live"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::merge;

    #[test]
    fn merging_replaces_merges_and_removes() {
        let mut target = json!({ "a": 1, "b": { "c": 2, "d": 3 }, "e": [1] });
        merge(&mut target, &json!({ "a": 5, "b": { "c": null, "x": 1 }, "e": [2, 3] }));
        assert_eq!(target, json!({ "a": 5, "b": { "d": 3, "x": 1 }, "e": [2, 3] }));
    }
}
