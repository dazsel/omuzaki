//! Canonical JSON for the determinism check (design D6): objects with sorted keys at every level, numbers in their
//! round-trip form, so two runs of the same script compare byte for byte.

use serde_json::{Map, Value};

/// `value` with every object's keys sorted, whatever map order `serde_json` was built with.
pub(crate) fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(k, v)| (k, canonical(v))).collect::<Map<_, _>>())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}
