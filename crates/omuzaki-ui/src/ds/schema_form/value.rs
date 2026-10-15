//! The value a form edits: typed, and convertible to the JSON an op takes.

use omuzaki_doc_api::Value;

use super::model::UNBOUNDED;

/// What a form edits. It mirrors the schema: an object holds its fields in the schema's order, by key.
///
/// Build one with [`Form::default_value`](super::Form::default_value) or
/// [`Form::value_from_json`](super::Form::value_from_json), let the form edit it, and send `Value::from(&value)` as
/// the arguments of a request.
#[derive(Clone, Debug, PartialEq)]
pub enum FormValue {
    /// A boolean field.
    Bool(bool),
    /// A number or integer field. An integer field only ever holds whole numbers.
    Number(f64),
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
    /// A nested object: its fields by key, in the schema's order.
    Object(Vec<(String, FormValue)>),
    /// JSON the form shows but does not edit: a field whose shape has no control, or a value that does not fit its
    /// field (out of range, not one of the options). It passes through to [`Value`] unchanged.
    Raw(Value),
    /// A read-only field the value did not mention. Left out of the JSON.
    Absent,
}

/// A number as JSON: a whole number as an integer (what an integer field must send), anything else as a float.
fn number(n: f64) -> Value {
    #[allow(clippy::cast_possible_truncation, reason = "checked to be whole and within UNBOUNDED just above")]
    if n.fract() == 0.0 && n.abs() <= UNBOUNDED {
        return Value::from(n as i64);
    }
    serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
}

impl From<&FormValue> for Value {
    fn from(value: &FormValue) -> Self {
        match value {
            FormValue::Bool(on) => Self::Bool(*on),
            FormValue::Number(n) => number(*n),
            FormValue::Text(text) => Self::String(text.clone()),
            FormValue::Choice { value, .. } | FormValue::Raw(value) => value.clone(),
            FormValue::Object(fields) => Self::Object(
                fields
                    .iter()
                    .filter(|(_, v)| *v != FormValue::Absent)
                    .map(|(k, v)| (k.clone(), Self::from(v)))
                    .collect(),
            ),
            FormValue::Absent => Self::Null,
        }
    }
}
