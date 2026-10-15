//! The canonical JSON form (ADR 0009 rule 3, design D3): object keys sorted by their UTF-8 bytes, no whitespace,
//! numbers as `serde_json` writes them with `float_roundtrip`. Not RFC 8785; this form is ours.

use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple, SerializeTupleStruct,
    SerializeTupleVariant,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

/// A value that has no canonical JSON form (a map with non-string keys, a failing `Serialize`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
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

/// `value` as a JSON [`Value`], refusing what has no canonical form.
///
/// `serde_json` writes a NaN or infinite float as `null`, which would make `Some(f64::NAN)` hash like `None`, so
/// those are refused here first.
///
/// # Errors
/// [`CanonicalError`] for a NaN or infinite float, a map with non-string keys or a failing `Serialize`.
pub fn to_json_value<T: Serialize>(value: &T) -> Result<Value, CanonicalError> {
    value.serialize(FiniteCheck)?;
    serde_json::to_value(value).map_err(|e| CanonicalError(e.to_string()))
}

/// BLAKE3 of the canonical JSON of `value`.
///
/// # Errors
/// [`CanonicalError`] when `value` has no canonical JSON form (see [`to_json_value`]).
pub fn canonical_hash<T: Serialize>(value: &T) -> Result<ContentHash, CanonicalError> {
    Ok(ContentHash::of(&canonical_json(&to_json_value(value)?)))
}

impl ser::Error for CanonicalError {
    fn custom<M: std::fmt::Display>(msg: M) -> Self {
        Self(msg.to_string())
    }
}

/// A serializer that writes nothing and fails on a non-finite float anywhere in the value.
struct FiniteCheck;

fn finite(v: f64) -> Result<(), CanonicalError> {
    if v.is_finite() { Ok(()) } else { Err(CanonicalError(format!("{v} is not a JSON number"))) }
}

macro_rules! accept {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(fn $method(self, _: $ty) -> Result<(), CanonicalError> { Ok(()) })*
    };
}

impl Serializer for FiniteCheck {
    type Ok = ();
    type Error = CanonicalError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    accept!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_char(char),
        serialize_str(&str),
        serialize_bytes(&[u8]),
        serialize_unit_struct(&'static str),
    );

    fn serialize_f32(self, v: f32) -> Result<(), CanonicalError> {
        finite(f64::from(v))
    }
    fn serialize_f64(self, v: f64) -> Result<(), CanonicalError> {
        finite(v)
    }
    fn serialize_none(self) -> Result<(), CanonicalError> {
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<(), CanonicalError> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), CanonicalError> {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<(), CanonicalError> {
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, v: &T) -> Result<(), CanonicalError> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        v: &T,
    ) -> Result<(), CanonicalError> {
        v.serialize(self)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self, CanonicalError> {
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, CanonicalError> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, CanonicalError> {
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, CanonicalError> {
        Ok(self)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self, CanonicalError> {
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, CanonicalError> {
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, CanonicalError> {
        Ok(self)
    }
}

impl SerializeSeq for FiniteCheck {
    type Ok = ();
    type Error = CanonicalError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        v.serialize(FiniteCheck)
    }
    fn end(self) -> Result<(), CanonicalError> {
        Ok(())
    }
}

impl SerializeTuple for FiniteCheck {
    type Ok = ();
    type Error = CanonicalError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        v.serialize(FiniteCheck)
    }
    fn end(self) -> Result<(), CanonicalError> {
        Ok(())
    }
}

impl SerializeTupleStruct for FiniteCheck {
    type Ok = ();
    type Error = CanonicalError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        v.serialize(FiniteCheck)
    }
    fn end(self) -> Result<(), CanonicalError> {
        Ok(())
    }
}

impl SerializeTupleVariant for FiniteCheck {
    type Ok = ();
    type Error = CanonicalError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        v.serialize(FiniteCheck)
    }
    fn end(self) -> Result<(), CanonicalError> {
        Ok(())
    }
}

impl SerializeMap for FiniteCheck {
    type Ok = ();
    type Error = CanonicalError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, k: &T) -> Result<(), CanonicalError> {
        k.serialize(FiniteCheck)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        v.serialize(FiniteCheck)
    }
    fn end(self) -> Result<(), CanonicalError> {
        Ok(())
    }
}

impl SerializeStruct for FiniteCheck {
    type Ok = ();
    type Error = CanonicalError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, v: &T) -> Result<(), CanonicalError> {
        v.serialize(FiniteCheck)
    }
    fn end(self) -> Result<(), CanonicalError> {
        Ok(())
    }
}

impl SerializeStructVariant for FiniteCheck {
    type Ok = ();
    type Error = CanonicalError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, v: &T) -> Result<(), CanonicalError> {
        v.serialize(FiniteCheck)
    }
    fn end(self) -> Result<(), CanonicalError> {
        Ok(())
    }
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
    fn non_finite_floats_are_refused_not_turned_into_null() {
        #[derive(Serialize)]
        struct Body {
            x: Option<f64>,
        }
        let none = canonical_hash(&Body { x: None }).unwrap();
        assert_ne!(canonical_hash(&Body { x: Some(1.0) }).unwrap(), none);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = canonical_hash(&Body { x: Some(bad) }).unwrap_err();
            assert!(err.0.contains("not a JSON number"), "{err}");
            assert!(canonical_hash(&vec![vec![Some(bad)]]).is_err(), "nested in a sequence");
            assert!(canonical_hash(&std::collections::BTreeMap::from([("k", (1, bad))])).is_err(), "in a map tuple");
        }
        assert!(canonical_hash(&f32::NAN).is_err());
        assert!(canonical_hash(&(f64::MAX, f64::MIN_POSITIVE, 0.0_f64)).is_ok());
    }

    #[test]
    fn a_map_with_non_string_keys_has_no_form() {
        let map = std::collections::BTreeMap::from([((1, 2), 3)]);
        assert!(canonical_hash(&map).is_err());
    }
}
