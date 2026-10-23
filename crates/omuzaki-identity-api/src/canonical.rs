//! The canonical JSON form (ADR 0009 rule 3, design D3): object keys sorted by their UTF-8 bytes, no whitespace,
//! numbers as `serde_json` writes them with `float_roundtrip`. Not RFC 8785; this form is ours.

use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::ser::{
    self, Impossible, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

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
/// The value is built and checked in **one** pass of `value`'s `Serialize`, so the value checked is the value hashed,
/// even for a `Serialize` that answers differently each time it is called. Scalars are converted by `serde_json`'s
/// own value serializer, so the result is exactly `serde_json::to_value`'s wherever that has a canonical form. Two
/// things it has none for are refused rather than guessed at:
/// - a NaN or infinite float, which `serde_json` writes as `null`, making `Some(f64::NAN)` hash like `None`;
/// - a map key that is not serialised as a string (an integer, a bool, a tuple...), which `serde_json` would stringify,
///   so a `BTreeMap<u32, _>` and a `BTreeMap<String, _>` could share a hash, and a signature cross between them.
///
/// # Errors
/// [`CanonicalError`] for a NaN or infinite float, a map key that is not a string, or a failing `Serialize`.
pub fn to_json_value<T: Serialize + ?Sized>(value: &T) -> Result<Value, CanonicalError> {
    value.serialize(ValueOf)
}

/// BLAKE3 of the canonical JSON of `value`.
///
/// # Errors
/// [`CanonicalError`] when `value` has no canonical JSON form (see [`to_json_value`]).
pub fn canonical_hash<T: Serialize + ?Sized>(value: &T) -> Result<ContentHash, CanonicalError> {
    Ok(ContentHash::of(&canonical_json(&to_json_value(value)?)))
}

impl ser::Error for CanonicalError {
    fn custom<M: std::fmt::Display>(msg: M) -> Self {
        Self(msg.to_string())
    }
}

/// The serializer behind [`to_json_value`]: it builds the [`Value`] and checks it in the same pass.
struct ValueOf;

fn finite(v: f64) -> Result<(), CanonicalError> {
    if v.is_finite() { Ok(()) } else { Err(CanonicalError(format!("{v} is not a JSON number"))) }
}

/// A scalar, converted by `serde_json`'s value serializer so the number forms match it exactly.
fn scalar(converted: Result<Value, serde_json::Error>) -> Result<Value, CanonicalError> {
    converted.map_err(|e| CanonicalError(e.to_string()))
}

macro_rules! scalars {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(fn $method(self, v: $ty) -> Result<Value, CanonicalError> {
            scalar(serde_json::value::Serializer.$method(v))
        })*
    };
}

impl Serializer for ValueOf {
    type Ok = Value;
    type Error = CanonicalError;
    type SerializeSeq = Items;
    type SerializeTuple = Items;
    type SerializeTupleStruct = Items;
    type SerializeTupleVariant = Variant<Items>;
    type SerializeMap = Entries;
    type SerializeStruct = Entries;
    type SerializeStructVariant = Variant<Entries>;

    scalars!(
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
    );

    fn serialize_f32(self, v: f32) -> Result<Value, CanonicalError> {
        finite(f64::from(v))?;
        scalar(serde_json::value::Serializer.serialize_f32(v))
    }
    fn serialize_f64(self, v: f64) -> Result<Value, CanonicalError> {
        finite(v)?;
        scalar(serde_json::value::Serializer.serialize_f64(v))
    }
    fn serialize_none(self) -> Result<Value, CanonicalError> {
        Ok(Value::Null)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<Value, CanonicalError> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<Value, CanonicalError> {
        Ok(Value::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Value, CanonicalError> {
        Ok(Value::Null)
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, variant: &'static str) -> Result<Value, CanonicalError> {
        Ok(Value::String(variant.to_owned()))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, v: &T) -> Result<Value, CanonicalError> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<Value, CanonicalError> {
        let mut map = Map::new();
        map.insert(variant.to_owned(), v.serialize(self)?);
        Ok(Value::Object(map))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Items, CanonicalError> {
        Ok(Items(Vec::with_capacity(len.unwrap_or(0))))
    }
    fn serialize_tuple(self, len: usize) -> Result<Items, CanonicalError> {
        Ok(Items(Vec::with_capacity(len)))
    }
    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<Items, CanonicalError> {
        Ok(Items(Vec::with_capacity(len)))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Variant<Items>, CanonicalError> {
        Ok(Variant { name: variant, inner: Items(Vec::with_capacity(len)) })
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Entries, CanonicalError> {
        Ok(Entries::default())
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Entries, CanonicalError> {
        Ok(Entries::default())
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Variant<Entries>, CanonicalError> {
        Ok(Variant { name: variant, inner: Entries::default() })
    }
}

/// The elements of a sequence or tuple, in order.
struct Items(Vec<Value>);

/// The entries of a map or struct, and a map key waiting for its value.
#[derive(Default)]
struct Entries {
    map: Map<String, Value>,
    key: Option<String>,
}

/// An enum variant's body, wrapped as `{"<variant>": body}` at the end, as `serde_json` does.
struct Variant<B> {
    name: &'static str,
    inner: B,
}

impl Variant<Items> {
    fn wrap(self) -> Value {
        let mut map = Map::new();
        map.insert(self.name.to_owned(), Value::Array(self.inner.0));
        Value::Object(map)
    }
}

impl Variant<Entries> {
    fn wrap(self) -> Value {
        let mut map = Map::new();
        map.insert(self.name.to_owned(), Value::Object(self.inner.map));
        Value::Object(map)
    }
}

impl SerializeSeq for Items {
    type Ok = Value;
    type Error = CanonicalError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        self.0.push(v.serialize(ValueOf)?);
        Ok(())
    }
    fn end(self) -> Result<Value, CanonicalError> {
        Ok(Value::Array(self.0))
    }
}

impl SerializeTuple for Items {
    type Ok = Value;
    type Error = CanonicalError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        SerializeSeq::serialize_element(self, v)
    }
    fn end(self) -> Result<Value, CanonicalError> {
        SerializeSeq::end(self)
    }
}

impl SerializeTupleStruct for Items {
    type Ok = Value;
    type Error = CanonicalError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        SerializeSeq::serialize_element(self, v)
    }
    fn end(self) -> Result<Value, CanonicalError> {
        SerializeSeq::end(self)
    }
}

impl SerializeTupleVariant for Variant<Items> {
    type Ok = Value;
    type Error = CanonicalError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        SerializeSeq::serialize_element(&mut self.inner, v)
    }
    fn end(self) -> Result<Value, CanonicalError> {
        Ok(self.wrap())
    }
}

impl SerializeMap for Entries {
    type Ok = Value;
    type Error = CanonicalError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, k: &T) -> Result<(), CanonicalError> {
        self.key = Some(k.serialize(Key)?);
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), CanonicalError> {
        let key = self.key.take().ok_or_else(|| CanonicalError("a map value came before its key".to_owned()))?;
        self.map.insert(key, v.serialize(ValueOf)?);
        Ok(())
    }
    fn end(self) -> Result<Value, CanonicalError> {
        Ok(Value::Object(self.map))
    }
}

impl SerializeStruct for Entries {
    type Ok = Value;
    type Error = CanonicalError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, name: &'static str, v: &T) -> Result<(), CanonicalError> {
        self.map.insert(name.to_owned(), v.serialize(ValueOf)?);
        Ok(())
    }
    fn end(self) -> Result<Value, CanonicalError> {
        Ok(Value::Object(self.map))
    }
}

impl SerializeStructVariant for Variant<Entries> {
    type Ok = Value;
    type Error = CanonicalError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, name: &'static str, v: &T) -> Result<(), CanonicalError> {
        SerializeStruct::serialize_field(&mut self.inner, name, v)
    }
    fn end(self) -> Result<Value, CanonicalError> {
        Ok(self.wrap())
    }
}

/// A map key: a string (a `&str`, a `char`, a unit enum variant's name, or a newtype around one) and nothing else.
struct Key;

fn not_a_string(what: &str) -> CanonicalError {
    CanonicalError(format!("a map key must be a string, not {what}"))
}

macro_rules! refused_keys {
    ($($method:ident($ty:ty) $what:literal),* $(,)?) => {
        $(fn $method(self, _: $ty) -> Result<String, CanonicalError> { Err(not_a_string($what)) })*
    };
}

impl Serializer for Key {
    type Ok = String;
    type Error = CanonicalError;
    type SerializeSeq = Impossible<String, CanonicalError>;
    type SerializeTuple = Impossible<String, CanonicalError>;
    type SerializeTupleStruct = Impossible<String, CanonicalError>;
    type SerializeTupleVariant = Impossible<String, CanonicalError>;
    type SerializeMap = Impossible<String, CanonicalError>;
    type SerializeStruct = Impossible<String, CanonicalError>;
    type SerializeStructVariant = Impossible<String, CanonicalError>;

    refused_keys!(
        serialize_bool(bool) "a bool",
        serialize_i8(i8) "a number",
        serialize_i16(i16) "a number",
        serialize_i32(i32) "a number",
        serialize_i64(i64) "a number",
        serialize_i128(i128) "a number",
        serialize_u8(u8) "a number",
        serialize_u16(u16) "a number",
        serialize_u32(u32) "a number",
        serialize_u64(u64) "a number",
        serialize_u128(u128) "a number",
        serialize_f32(f32) "a number",
        serialize_f64(f64) "a number",
        serialize_bytes(&[u8]) "bytes",
        serialize_unit_struct(&'static str) "a unit struct",
    );

    fn serialize_char(self, v: char) -> Result<String, CanonicalError> {
        Ok(v.to_string())
    }
    fn serialize_str(self, v: &str) -> Result<String, CanonicalError> {
        Ok(v.to_owned())
    }
    fn serialize_none(self) -> Result<String, CanonicalError> {
        Err(not_a_string("null"))
    }
    fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> Result<String, CanonicalError> {
        Err(not_a_string("an option"))
    }
    fn serialize_unit(self) -> Result<String, CanonicalError> {
        Err(not_a_string("null"))
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, variant: &'static str) -> Result<String, CanonicalError> {
        Ok(variant.to_owned())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, v: &T) -> Result<String, CanonicalError> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<String, CanonicalError> {
        Err(not_a_string("an enum variant with a value"))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, CanonicalError> {
        Err(not_a_string("a sequence"))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, CanonicalError> {
        Err(not_a_string("a tuple"))
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeTupleStruct, CanonicalError> {
        Err(not_a_string("a tuple"))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, CanonicalError> {
        Err(not_a_string("an enum variant with a value"))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, CanonicalError> {
        Err(not_a_string("a map"))
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct, CanonicalError> {
        Err(not_a_string("a struct"))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, CanonicalError> {
        Err(not_a_string("an enum variant with a value"))
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
        // An integer key would stringify to the same bytes as a string key: refused, so the two never share a hash.
        let numbered = std::collections::BTreeMap::from([(7_u32, "x")]);
        let err = canonical_hash(&numbered).unwrap_err();
        assert!(err.0.contains("must be a string"), "{err}");
        assert!(canonical_hash(&std::collections::BTreeMap::from([(true, 1)])).is_err());
        assert!(canonical_hash(&std::collections::BTreeMap::from([("7", "x")])).is_ok());
        assert!(canonical_hash(&std::collections::BTreeMap::from([('k', 1)])).is_ok(), "a char is a string");
    }

    /// A `Serialize` that answers finitely the first time it is asked and with NaN after: the one pass sees the NaN
    /// if it is ever asked twice, and the value it builds is the value it checked.
    #[test]
    fn a_serializer_that_changes_its_answer_is_checked_on_the_value_that_is_hashed() {
        struct Fickle(std::cell::Cell<u32>);
        impl Serialize for Fickle {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let calls = self.0.get();
                self.0.set(calls + 1);
                s.serialize_f64(if calls == 0 { 1.0 } else { f64::NAN })
            }
        }
        let fickle = Fickle(std::cell::Cell::new(0));
        assert_eq!(to_json_value(&fickle).unwrap(), serde_json::json!(1.0));
        assert_eq!(fickle.0.get(), 1, "serialised once");
        // Asked again it is NaN, and that is refused.
        assert!(to_json_value(&fickle).is_err());
    }

    /// Wherever a canonical form exists, the one-pass value is exactly `serde_json`'s, so no hash changes.
    #[test]
    fn the_value_matches_serde_jsons_for_every_shape() {
        #[derive(Serialize)]
        enum Shape {
            Unit,
            New(u8),
            Tuple(i32, f32),
            Struct { a: Option<String>, b: Vec<u8> },
        }
        #[derive(Serialize)]
        struct Unit;
        #[derive(Serialize)]
        struct Id(String);
        #[derive(Serialize)]
        struct Body<'a> {
            shapes: Vec<Shape>,
            unit: Unit,
            id: Id,
            ids: std::collections::BTreeMap<String, Id>,
            big: (u128, i128, i64, u64),
            floats: (f32, f64, f32),
            bytes: &'a serde_bytes_like::Bytes,
            c: char,
            none: Option<u8>,
        }
        mod serde_bytes_like {
            pub struct Bytes(pub [u8; 3]);
            impl serde::Serialize for Bytes {
                fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    s.serialize_bytes(&self.0)
                }
            }
        }
        let body = Body {
            shapes: vec![
                Shape::Unit,
                Shape::New(3),
                Shape::Tuple(-1, 0.1),
                Shape::Struct { a: Some("x".into()), b: vec![1, 2] },
            ],
            unit: Unit,
            id: Id("a".into()),
            ids: std::collections::BTreeMap::from([("k".to_owned(), Id("v".into()))]),
            big: (u128::from(u64::MAX), -5, i64::MIN, u64::MAX),
            floats: (1.1, -0.0, 3.4e38),
            bytes: &serde_bytes_like::Bytes([0, 128, 255]),
            c: 'é',
            none: None,
        };
        assert_eq!(to_json_value(&body).unwrap(), serde_json::to_value(&body).unwrap());
        assert_eq!(canonical_hash(&body).unwrap(), ContentHash::of(&canonical_json(&serde_json::to_value(&body).unwrap())));
        assert!(to_json_value(&u128::MAX).is_err(), "out of range, as serde_json says");
    }
}
