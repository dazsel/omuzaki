//! TOML text → document values.
//!
//! The tree is JSON-shaped (`serde_json::Value`), so every TOML construct needs a JSON counterpart:
//!
//! | TOML | tree |
//! |---|---|
//! | table, inline table | object |
//! | array, array of tables | array |
//! | string, integer, float, boolean | the same |
//! | date and time values | a string holding the value's text |
//!
//! Date and time values have no JSON type. Surfacing their text keeps files that contain them openable, and
//! [`crate::toml_write`] keeps such a value verbatim for as long as the string doesn't change. `nan` and `inf`
//! are the one thing refused: JSON numbers can't hold them.

use serde_json::{Map, Number, Value};
use toml_edit::{Document, Item, TableLike};

/// Why a TOML file couldn't become document values.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// Not valid TOML.
    Syntax(toml_edit::TomlError),
    /// Valid TOML that the tree can't hold.
    Unsupported {
        /// Where in the file, as a dotted key path.
        at: String,
        /// What is wrong.
        reason: &'static str,
    },
}

/// Parses a whole TOML file into the object it holds.
pub(crate) fn parse_table(text: &str) -> Result<Map<String, Value>, ReadError> {
    let doc = Document::parse(strip_bom(text)).map_err(ReadError::Syntax)?;
    table_to_map(doc.as_table())
}

/// Drops a leading UTF-8 byte-order mark, which Windows editors like to add and TOML doesn't allow.
pub(crate) fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn table_to_map(table: &dyn TableLike) -> Result<Map<String, Value>, ReadError> {
    let mut map = Map::new();
    for (key, item) in table.iter() {
        if item.is_none() {
            continue;
        }
        let value = item_to_value(item).map_err(|e| e.within(key))?;
        map.insert(key.to_owned(), value);
    }
    Ok(map)
}

fn item_to_value(item: &Item) -> Result<Value, ReadError> {
    match item {
        Item::None => Ok(Value::Null),
        Item::Value(v) => value_to_json(v),
        Item::Table(t) => Ok(Value::Object(table_to_map(t)?)),
        Item::ArrayOfTables(tables) => tables
            .iter()
            .enumerate()
            .map(|(i, t)| table_to_map(t).map(Value::Object).map_err(|e| e.within(&i.to_string())))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
    }
}

fn value_to_json(value: &toml_edit::Value) -> Result<Value, ReadError> {
    use toml_edit::Value as T;
    Ok(match value {
        T::String(s) => Value::String(s.value().clone()),
        T::Integer(i) => Value::from(*i.value()),
        T::Float(f) => Number::from_f64(*f.value()).map(Value::Number).ok_or(ReadError::Unsupported {
            at: String::new(),
            reason: "nan and inf can't be stored in the document; write a finite number",
        })?,
        T::Boolean(b) => Value::Bool(*b.value()),
        T::Datetime(d) => Value::String(d.value().to_string()),
        T::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, v)| value_to_json(v).map_err(|e| e.within(&i.to_string())))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)?,
        T::InlineTable(t) => Value::Object(table_to_map(t)?),
    })
}

impl ReadError {
    /// Prefixes the error's location with `segment` (errors are built from the inside out).
    fn within(self, segment: &str) -> Self {
        match self {
            Self::Unsupported { at, reason } => {
                let at = if at.is_empty() { segment.to_owned() } else { format!("{segment}.{at}") };
                Self::Unsupported { at, reason }
            }
            syntax @ Self::Syntax(_) => syntax,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_every_toml_construct() {
        let text = r#"
name = "mira"
count = 3
ratio = 0.5
whole = 2.0
on = true
born = 2026-10-11T12:00:00Z
tags = ["a", "b"]
pair = { x = 1, y = 2 }

[eyes]
size = 4

[[layers]]
id = "base"

[[layers]]
id = "shine"
"#;
        let got = Value::Object(parse_table(text).unwrap());
        assert_eq!(
            got,
            json!({
                "name": "mira", "count": 3, "ratio": 0.5, "whole": 2.0, "on": true,
                "born": "2026-10-11T12:00:00Z", "tags": ["a", "b"], "pair": {"x": 1, "y": 2},
                "eyes": {"size": 4}, "layers": [{"id": "base"}, {"id": "shine"}],
            })
        );
        assert!(got["whole"].is_f64() && got["count"].is_i64());
    }

    #[test]
    fn syntax_errors_carry_a_position() {
        let ReadError::Syntax(e) = parse_table("a = \n").unwrap_err() else { panic!("expected a syntax error") };
        assert!(e.span().is_some());
    }

    #[test]
    fn nan_is_refused_with_its_location() {
        let ReadError::Unsupported { at, .. } = parse_table("[a]\nb = [1, nan]\n").unwrap_err() else {
            panic!("expected unsupported")
        };
        assert_eq!(at, "a.b.1");
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        assert_eq!(parse_table("\u{feff}a = 1\n").unwrap().get("a"), Some(&json!(1)));
    }
}
