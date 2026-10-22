//! The value a form edits: typed, and convertible to the JSON an op takes.

use omuzaki_core::colour::Colour;
use omuzaki_doc_api::Value;

use crate::ds::widgets::{Curve, CurvePoint};

/// What a form edits. It mirrors the schema: an object holds its fields in the schema's order, by key.
///
/// Build one with [`Form::default_value`](super::Form::default_value) or
/// [`Form::value_from_json`](super::Form::value_from_json), let the form edit it, and send `Value::from(&value)` as
/// the arguments of a request.
#[derive(Clone, Debug, PartialEq)]
pub enum FormValue {
    /// A boolean field.
    Bool(bool),
    /// A `number` field.
    Number(f64),
    /// An `integer` field. It is kept as an integer, never through a float, so every `i64` and `u64` survives the
    /// round trip to JSON. A field that can hold integers a float cannot is typed as text and parsed exactly.
    Integer(i128),
    /// A string field. It may break the field's `pattern` while it is being typed; see
    /// [`Form::invalid_count`](super::Form::invalid_count).
    Text(String),
    /// An enum or few-valued integer field: the chosen option and its JSON.
    Choice {
        /// The position among the field's options.
        index: usize,
        /// The option's value as the schema writes it.
        value: Value,
    },
    /// A colour field.
    Colour(Colour),
    /// A `number[]` field drawn as a curve: x, y pairs over the unit square, tangents automatic.
    Curve(Curve),
    /// A nested object: its fields by key, in the schema's order.
    Object(Vec<(String, FormValue)>),
    /// A field with no control of its own, as the JSON text the person typed; it may not parse while it is being typed
    /// (see [`Form::invalid_count`](super::Form::invalid_count)).
    Json(String),
    /// JSON the form shows but does not edit: a field whose shape has no control, or a value that does not fit its
    /// field (out of range, not one of the options). It passes through to [`Value`] unchanged.
    Raw(Value),
    /// A read-only field the value did not mention. Left out of the JSON.
    Absent,
}

/// A number as JSON: a whole number within the range floats hold exactly as an integer, anything else as a float.
pub(super) fn number(n: f64) -> Value {
    #[allow(clippy::cast_possible_truncation, reason = "checked to be whole and within 2^53 just above")]
    if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_992.0 {
        return Value::from(n as i64);
    }
    serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
}

/// A curve from the flat `[x0, y0, x1, y1, ...]` array a schema carries, or `None` when it is not one (an odd count,
/// a point off the unit square, too few or too many points, x not increasing).
pub(super) fn curve_from_json(numbers: &[Value]) -> Option<Curve> {
    if !numbers.len().is_multiple_of(2) {
        return None;
    }
    let points = numbers
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            // A coordinate that single precision would print differently is not taken, so reading never changes it.
            #[allow(clippy::cast_possible_truncation, reason = "curve points are single precision; checked just after")]
            let coordinate = |v: &Value| {
                let f = v.as_f64()?;
                let x = f as f32;
                (shortest(x) == number(f)).then_some(x)
            };
            Some(CurvePoint::new(coordinate(&pair[0])?, coordinate(&pair[1])?))
        })
        .collect::<Option<Vec<_>>>()?;
    let curve = Curve::new(0.0..=1.0, 0.0..=1.0, points.clone()).ok()?;
    // `Curve::new` moves the end points onto the domain and clamps y: a value it would change is not this field's.
    (curve.points() == points.as_slice()).then_some(curve)
}

/// A single-precision coordinate as the double that prints the same (0.3, not 0.30000001192092896), so the JSON reads
/// as it was written.
fn shortest(x: f32) -> Value {
    let f = x.to_string().parse().unwrap_or_else(|_| f64::from(x));
    number(f)
}

/// An integer as JSON, as an `i64` or a `u64` (the two kinds of integer JSON carries here).
pub(super) fn integer(n: i128) -> Value {
    if let Ok(n) = i64::try_from(n) {
        Value::from(n)
    } else if let Ok(n) = u64::try_from(n) {
        Value::from(n)
    } else {
        Value::Null
    }
}

impl From<&FormValue> for Value {
    fn from(value: &FormValue) -> Self {
        match value {
            FormValue::Bool(on) => Self::Bool(*on),
            FormValue::Number(n) => number(*n),
            FormValue::Integer(n) => integer(*n),
            FormValue::Text(text) => Self::String(text.clone()),
            FormValue::Choice { value, .. } | FormValue::Raw(value) => value.clone(),
            FormValue::Colour(colour) => Self::String(colour.to_string()),
            FormValue::Curve(curve) => curve.points().iter().flat_map(|p| [shortest(p.x), shortest(p.y)]).collect(),
            FormValue::Object(fields) => Self::Object(
                fields
                    .iter()
                    .filter(|(_, v)| *v != FormValue::Absent)
                    .map(|(k, v)| (k.clone(), Self::from(v)))
                    .collect(),
            ),
            FormValue::Json(text) => serde_json::from_str(text).unwrap_or(Self::Null),
            FormValue::Absent => Self::Null,
        }
    }
}
