//! The licence documents as the project stores them: no `null`s.
//!
//! The project files are TOML, which has no null, and an absent option reads back as the same `None`, so a record or a
//! source row is written without the nulls its optional fields serialise to. `licence.set` and `licence.set_source`
//! both store through here, so the two cannot disagree about what a stored document looks like.

use omuzaki_ops_api::OpError;
use serde::Serialize;
use serde_json::Value;

/// `value` as the project stores it. `what` names it for the error ("the record").
///
/// # Errors
/// `internal` if the value cannot be serialised (a bug: these types hold nothing that fails).
pub(super) fn node_of<T: Serialize>(value: &T, what: &str) -> Result<Value, OpError> {
    serde_json::to_value(value)
        .map(strip_nulls)
        .map_err(|e| OpError::internal(format!("serialising {what} failed: {e}")))
}

/// `value` without its `null`s. Array elements keep their places.
fn strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, strip_nulls(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn nulls_go_from_every_table_but_arrays_keep_their_places() {
        let node = node_of(&json!({ "a": null, "b": { "c": null, "d": 1 }, "e": [null, { "f": null }] }), "x").unwrap();
        assert_eq!(node, json!({ "b": { "d": 1 }, "e": [null, {}] }));
    }
}
