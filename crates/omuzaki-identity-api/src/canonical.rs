//! The canonical JSON form (ADR 0009 rule 3, design D3): object keys sorted by their UTF-8 bytes, no whitespace,
//! numbers as `serde_json` writes them with `float_roundtrip`. Not RFC 8785; this form is ours.

use omuzaki_store_api::ContentHash;
use serde::Serialize;
use serde_json::Value;

/// A value that has no canonical JSON form (a map with non-string keys, a failing `Serialize`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("no canonical JSON form: {0}")]
pub struct CanonicalError(pub String);

/// `value` as canonical JSON bytes. Keys are sorted here rather than trusted to `serde_json`'s map type, which keeps
/// insertion order when any crate in the build turns on `preserve_order`.
pub fn canonical_json(value: &Value) -> Vec<u8> {
    let mut out = String::new();
    write(value, &mut out);
    out.into_bytes()
}

fn write(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write(v, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(v, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// BLAKE3 of the canonical JSON of `value`.
///
/// A non-finite float has no JSON form and serialises as `null`; bodies must not hold one.
///
/// # Errors
/// [`CanonicalError`] when `value` cannot be turned into JSON.
pub fn canonical_hash<T: Serialize>(value: &T) -> Result<ContentHash, CanonicalError> {
    let json = serde_json::to_value(value).map_err(|e| CanonicalError(e.to_string()))?;
    Ok(ContentHash::of(&canonical_json(&json)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_at_every_level_and_nothing_is_padded() {
        let value = json!({"b": [1, {"z": 1.5, "a": null}], "a": "x y"});
        assert_eq!(String::from_utf8(canonical_json(&value)).unwrap(), r#"{"a":"x y","b":[1,{"a":null,"z":1.5}]}"#);
    }

    #[test]
    fn a_map_with_non_string_keys_has_no_form() {
        let map = std::collections::BTreeMap::from([((1, 2), 3)]);
        assert!(canonical_hash(&map).is_err());
    }
}
