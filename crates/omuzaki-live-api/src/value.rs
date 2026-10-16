//! Values on ports (design D3): [`PortType`], [`Val`] and the coercions between them.
//!
//! `PortType` and `Val` are closed on purpose (design D21): the wire checks, the coercion table and the engine's
//! dispatch match them exhaustively, so a new type is a contract change by decision record.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::limits::{MAX_LIST_ITEMS, MAX_TEXT_BYTES};

/// The type of a port.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    /// A pulse: control, no data. Event ports never connect to value ports.
    Event,
    /// `true` or `false`.
    Bool,
    /// A finite `f64`.
    Number,
    /// UTF-8 text.
    Text,
    /// A list of values.
    List,
    /// Any JSON value.
    Json,
}

/// How a value of one type reaches an input of another, on a wire or from a `set` constant (design D3).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Coercion {
    /// The same type: passed as it is.
    Same,
    /// Converted, never failing: `bool` and `number` to `text`, anything to `json`.
    Converts,
    /// `json` to a typed input: checked when read, and a mismatch fails that read.
    CheckedWhenRead,
    /// Never connected: `text` to `number` (use `text.to_number`), events to values, and every other pair.
    Refused,
}

impl PortType {
    /// Every port type, in declaration order.
    pub const ALL: [PortType; 6] =
        [PortType::Event, PortType::Bool, PortType::Number, PortType::Text, PortType::List, PortType::Json];

    /// The type's name as written in documents and schemas (`number`).
    pub const fn as_str(self) -> &'static str {
        match self {
            PortType::Event => "event",
            PortType::Bool => "bool",
            PortType::Number => "number",
            PortType::Text => "text",
            PortType::List => "list",
            PortType::Json => "json",
        }
    }

    /// True for [`PortType::Event`].
    pub const fn is_event(self) -> bool {
        matches!(self, PortType::Event)
    }

    /// The value an input of this type reads when nothing else gives one: `false`, `0`, `""`, `[]`, `null` (design
    /// D3). `None` for events, which carry no value.
    pub fn default_value(self) -> Option<Val> {
        match self {
            PortType::Event => None,
            PortType::Bool => Some(Val::Bool(false)),
            PortType::Number => Some(Val::Number(0.0)),
            PortType::Text => Some(Val::Text(Arc::from(""))),
            PortType::List => Some(Val::List(Arc::from(Vec::new()))),
            PortType::Json => Some(Val::Json(Arc::new(Value::Null))),
        }
    }

    /// How a value of type `from` reaches an input of type `to` (design D3). The wiring ops refuse
    /// [`Coercion::Refused`]; the engine converts the others when the input is read.
    pub const fn coercion(from: PortType, to: PortType) -> Coercion {
        match (from, to) {
            (PortType::Event, PortType::Event)
            | (PortType::Bool, PortType::Bool)
            | (PortType::Number, PortType::Number)
            | (PortType::Text, PortType::Text)
            | (PortType::List, PortType::List)
            | (PortType::Json, PortType::Json) => Coercion::Same,
            (PortType::Event, _) | (_, PortType::Event) => Coercion::Refused,
            (PortType::Bool | PortType::Number, PortType::Text) | (_, PortType::Json) => Coercion::Converts,
            (PortType::Json, _) => Coercion::CheckedWhenRead,
            _ => Coercion::Refused,
        }
    }
}

/// Why a value can't be made or converted.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ValError {
    /// A number that is NaN or infinite. Numbers are always finite.
    #[error("a number must be finite")]
    NotFinite,
    /// The two types never convert (design D3).
    #[non_exhaustive]
    #[error("a {} value can't be used as {}", from.as_str(), to.as_str())]
    Refused {
        /// The value's type.
        from: PortType,
        /// The type it was asked to become.
        to: PortType,
    },
    /// A JSON value that doesn't have the asked type.
    #[non_exhaustive]
    #[error("expected {}, got {found}", expected.as_str())]
    Mismatch {
        /// The type that was asked for.
        expected: PortType,
        /// What the JSON held (`a string`, `an object`).
        found: &'static str,
    },
    /// Text longer than [`MAX_TEXT_BYTES`].
    #[non_exhaustive]
    #[error("text of {bytes} bytes is longer than the limit of {max} bytes", max = MAX_TEXT_BYTES)]
    TextTooLong {
        /// Its length in bytes.
        bytes: usize,
    },
    /// A list with more than [`MAX_LIST_ITEMS`] items.
    #[non_exhaustive]
    #[error("a list of {items} items is longer than the limit of {max}", max = MAX_LIST_ITEMS)]
    ListTooLong {
        /// Its length.
        items: usize,
    },
}

impl ValError {
    /// `from` never converts to `to`.
    pub fn refused(from: PortType, to: PortType) -> Self {
        ValError::Refused { from, to }
    }

    /// JSON holding `found` where `expected` was asked for.
    pub fn mismatch(expected: PortType, found: &'static str) -> Self {
        ValError::Mismatch { expected, found }
    }

    /// Text of `bytes` bytes, over [`MAX_TEXT_BYTES`].
    pub fn text_too_long(bytes: usize) -> Self {
        ValError::TextTooLong { bytes }
    }

    /// A list of `items` items, over [`MAX_LIST_ITEMS`].
    pub fn list_too_long(items: usize) -> Self {
        ValError::ListTooLong { items }
    }
}

/// A value on a value port. Cheap to clone: text, lists and JSON are shared.
///
/// A [`Val::Number`] is always finite: build one with [`Val::number`], and [`check`](Self::check) refuses one built
/// directly with NaN or an infinity. Values hold nothing but data, so they compare and serialise deterministically.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    /// `true` or `false`.
    Bool(bool),
    /// A finite number.
    Number(f64),
    /// Text.
    Text(Arc<str>),
    /// A list.
    List(Arc<[Val]>),
    /// Any JSON value.
    Json(Arc<Value>),
}

impl Val {
    /// A number value.
    ///
    /// # Errors
    /// [`ValError::NotFinite`] for NaN and the infinities.
    pub fn number(x: f64) -> Result<Val, ValError> {
        if x.is_finite() { Ok(Val::Number(x)) } else { Err(ValError::NotFinite) }
    }

    /// A text value.
    pub fn text(s: impl Into<Arc<str>>) -> Val {
        Val::Text(s.into())
    }

    /// The value's type.
    pub fn ty(&self) -> PortType {
        match self {
            Val::Bool(_) => PortType::Bool,
            Val::Number(_) => PortType::Number,
            Val::Text(_) => PortType::Text,
            Val::List(_) => PortType::List,
            Val::Json(_) => PortType::Json,
        }
    }

    /// Checks the rules every value on a port keeps: numbers finite, text at most [`MAX_TEXT_BYTES`], lists at most
    /// [`MAX_LIST_ITEMS`] items, all the way down. The engine checks each value a node sets.
    ///
    /// # Errors
    /// The first rule the value breaks.
    pub fn check(&self) -> Result<(), ValError> {
        match self {
            Val::Number(x) => Val::number(*x).map(|_| ()),
            Val::Text(s) if s.len() > MAX_TEXT_BYTES => Err(ValError::TextTooLong { bytes: s.len() }),
            Val::Bool(_) | Val::Json(_) | Val::Text(_) => Ok(()),
            Val::List(items) if items.len() > MAX_LIST_ITEMS => Err(ValError::ListTooLong { items: items.len() }),
            Val::List(items) => items.iter().try_for_each(Val::check),
        }
    }

    /// This value as an input of type `to` reads it (design D3): the same type as it is; `bool` and `number` to
    /// text (numbers in their shortest round-trip form, integers without `.0`); anything to `json`; `json` to a typed
    /// input checked now.
    ///
    /// # Errors
    /// [`ValError::Refused`] for a pair that never converts (`text` to `number` included: that can fail, so it is
    /// `text.to_number`'s job), [`ValError::Mismatch`] for JSON of the wrong shape, and the errors of
    /// [`Val::from_json`] for JSON over the D8 limits.
    pub fn coerce(&self, to: PortType) -> Result<Val, ValError> {
        match (PortType::coercion(self.ty(), to), self) {
            (Coercion::Same, _) => Ok(self.clone()),
            (Coercion::Converts, _) if to == PortType::Json => Ok(Val::Json(Arc::new(self.to_json()))),
            (Coercion::Converts, Val::Bool(b)) => Ok(Val::text(if *b { "true" } else { "false" })),
            (Coercion::Converts, Val::Number(x)) => Ok(Val::text(number_text(*x))),
            (Coercion::CheckedWhenRead, Val::Json(json)) => Val::from_json(json, to),
            _ => Err(ValError::Refused { from: self.ty(), to }),
        }
    }

    /// The value as JSON. A whole number of magnitude below 2^53 is written as an integer, so an op argument
    /// declared `integer` accepts it.
    pub fn to_json(&self) -> Value {
        match self {
            Val::Bool(b) => Value::Bool(*b),
            Val::Number(x) => number_json(*x),
            Val::Text(s) => Value::String(s.to_string()),
            Val::List(items) => Value::Array(items.iter().map(Val::to_json).collect()),
            Val::Json(json) => (**json).clone(),
        }
    }

    /// Reads `value` as a value of type `ty`. A list's items take their natural types (booleans, numbers, text,
    /// lists, and JSON for objects and `null`).
    ///
    /// # Errors
    /// [`ValError::Mismatch`] if `value` doesn't have that shape, [`ValError::Refused`] for [`PortType::Event`],
    /// [`ValError::NotFinite`] for a number JSON can't hold as a finite `f64`, and [`ValError::TextTooLong`] or
    /// [`ValError::ListTooLong`] for text or a list (at any depth) over the D8 limits, which typed values keep.
    pub fn from_json(value: &Value, ty: PortType) -> Result<Val, ValError> {
        let mismatch = || ValError::Mismatch { expected: ty, found: json_kind(value) };
        match ty {
            PortType::Event => Err(ValError::Refused { from: PortType::Json, to: ty }),
            PortType::Json => Ok(Val::Json(Arc::new(value.clone()))),
            PortType::Bool => value.as_bool().map(Val::Bool).ok_or_else(mismatch),
            PortType::Number => value.as_f64().ok_or_else(mismatch).and_then(Val::number),
            PortType::Text => match value.as_str() {
                Some(s) if s.len() > MAX_TEXT_BYTES => Err(ValError::TextTooLong { bytes: s.len() }),
                Some(s) => Ok(Val::text(s)),
                None => Err(mismatch()),
            },
            PortType::List => match value {
                // Checked before the items are built; nested lists and text check themselves in `natural`.
                Value::Array(items) if items.len() > MAX_LIST_ITEMS => {
                    Err(ValError::ListTooLong { items: items.len() })
                }
                Value::Array(items) => {
                    Ok(Val::List(items.iter().map(Val::natural).collect::<Result<Vec<_>, _>>()?.into()))
                }
                _ => Err(mismatch()),
            },
        }
    }

    /// A JSON value as the value of its own natural type.
    fn natural(value: &Value) -> Result<Val, ValError> {
        let ty = match value {
            Value::Bool(_) => PortType::Bool,
            Value::Number(_) => PortType::Number,
            Value::String(_) => PortType::Text,
            Value::Array(_) => PortType::List,
            Value::Null | Value::Object(_) => PortType::Json,
        };
        Val::from_json(value, ty)
    }
}

/// A number as text: the shortest form that reads back as the same `f64`, without `.0` on whole numbers.
fn number_text(x: f64) -> String {
    // Rust's `Display` for f64 is the shortest round-trip form and prints whole numbers without a fraction.
    format!("{x}")
}

/// The largest magnitude below which every whole `f64` is an exact integer.
const EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0; // 2^53

/// A finite number as JSON: whole numbers below 2^53 as integers, the rest as floats.
fn number_json(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < EXACT_INTEGER_LIMIT {
        // Exact: the value is whole and inside the range where every integer is representable.
        #[allow(clippy::cast_possible_truncation)]
        return Value::from(x as i64);
    }
    serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
}

/// What a JSON value is, for error text.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

impl From<bool> for Val {
    fn from(b: bool) -> Self {
        Val::Bool(b)
    }
}

/// Wraps without checking: a NaN or an infinity is refused when the value is set on a port ([`Val::check`]).
impl From<f64> for Val {
    fn from(x: f64) -> Self {
        Val::Number(x)
    }
}

impl From<i32> for Val {
    fn from(x: i32) -> Self {
        Val::Number(f64::from(x))
    }
}

impl From<u32> for Val {
    fn from(x: u32) -> Self {
        Val::Number(f64::from(x))
    }
}

impl From<&str> for Val {
    fn from(s: &str) -> Self {
        Val::text(s)
    }
}

impl From<String> for Val {
    fn from(s: String) -> Self {
        Val::text(s)
    }
}

impl From<Arc<str>> for Val {
    fn from(s: Arc<str>) -> Self {
        Val::Text(s)
    }
}

impl From<Vec<Val>> for Val {
    fn from(items: Vec<Val>) -> Self {
        Val::List(items.into())
    }
}

impl From<Value> for Val {
    fn from(json: Value) -> Self {
        Val::Json(Arc::new(json))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn whole_numbers_become_json_integers() {
        assert_eq!(Val::Number(3.0).to_json(), json!(3));
        assert_eq!(Val::Number(-0.0).to_json(), json!(0));
        assert_eq!(Val::Number(2.5).to_json(), json!(2.5));
        assert_eq!(Val::Number(1e300).to_json(), json!(1e300));
    }

    #[test]
    fn check_finds_nested_problems() {
        assert_eq!(Val::from(vec![Val::Number(f64::NAN)]).check(), Err(ValError::NotFinite));
        let long = Val::text("x".repeat(MAX_TEXT_BYTES + 1));
        assert_eq!(long.check(), Err(ValError::TextTooLong { bytes: MAX_TEXT_BYTES + 1 }));
        let many = Val::from(vec![Val::Bool(true); MAX_LIST_ITEMS + 1]);
        assert_eq!(many.check(), Err(ValError::ListTooLong { items: MAX_LIST_ITEMS + 1 }));
        assert!(Val::from(vec![Val::Bool(true); MAX_LIST_ITEMS]).check().is_ok());
    }

    #[test]
    fn json_read_as_a_typed_value_keeps_the_limits() {
        let long = json!("x".repeat(MAX_TEXT_BYTES + 1));
        assert_eq!(Val::from_json(&long, PortType::Text), Err(ValError::TextTooLong { bytes: MAX_TEXT_BYTES + 1 }));
        let many = json!(vec![true; MAX_LIST_ITEMS + 1]);
        assert_eq!(Val::from_json(&many, PortType::List), Err(ValError::ListTooLong { items: MAX_LIST_ITEMS + 1 }));
        let nested = json!([["x".repeat(MAX_TEXT_BYTES + 1)]]);
        assert_eq!(Val::from_json(&nested, PortType::List), Err(ValError::TextTooLong { bytes: MAX_TEXT_BYTES + 1 }));
        let as_json = Val::Json(Arc::new(long));
        assert_eq!(as_json.coerce(PortType::Text), Err(ValError::TextTooLong { bytes: MAX_TEXT_BYTES + 1 }));
        assert!(Val::from_json(&json!("x".repeat(MAX_TEXT_BYTES)), PortType::Text).is_ok());
        assert!(Val::from_json(&json!(vec![true; MAX_LIST_ITEMS]), PortType::List).is_ok());
    }

    #[test]
    fn a_list_from_json_takes_natural_types() {
        let list = Val::from_json(&json!([true, 2, "x", [1], {"a": 1}, null]), PortType::List).unwrap();
        let Val::List(items) = list else { panic!("a list") };
        let types: Vec<PortType> = items.iter().map(Val::ty).collect();
        let expected =
            [PortType::Bool, PortType::Number, PortType::Text, PortType::List, PortType::Json, PortType::Json];
        assert_eq!(types, expected);
    }
}
