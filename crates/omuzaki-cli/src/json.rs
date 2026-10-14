//! Canonical JSON for `--json`: object keys sorted at every depth, so the output is byte-for-byte the same whichever
//! `serde_json` features the build enables.
//!
//! Why this exists: `serde_json::Map` is sorted by default but insertion-ordered when any crate in the dependency
//! graph turns on `preserve_order`. Op outputs and patch values are `Value`s from the runner, so without this their
//! key order would depend on the build, and scripts diffing `omuzaki` output would see phantom changes.
//!
//! Why not `serde_json::to_value` and sort that: it widens `f32` to `f64` (`0.1_f32` becomes `0.10000000149011612`).
//! Instead the value is serialised once, to text, and the text is re-emitted with sorted keys; numbers are copied
//! verbatim as [`RawValue`]s, so they print exactly as `serde_json` printed them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use omuzaki_ops_api::OpError;
use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;

/// Two spaces per level, like `serde_json::to_string_pretty`.
const INDENT: &str = "  ";

/// Pretty-printed JSON with every object's keys sorted, ending in a newline.
///
/// # Errors
/// `Internal` if the value can't be serialised (only a broken `Serialize` impl can cause that).
pub fn pretty<T: Serialize + ?Sized>(value: &T) -> Result<String, OpError> {
    let mut text =
        canonical(value, true).map_err(|e| OpError::internal(format!("encoding the JSON output failed: {e}")))?;
    text.push('\n');
    Ok(text)
}

/// One value as sorted JSON text: indented over several lines when `pretty`, otherwise on one line. For the places
/// that show a document value inside text output (patch diffs, an op's answer, schema defaults), which must not vary
/// with the build either.
pub fn render_value(value: &Value, pretty: bool) -> String {
    // A `Value` always serialises; the fallback only keeps this infallible.
    canonical(value, pretty).unwrap_or_else(|_| value.to_string())
}

fn canonical<T: Serialize + ?Sized>(value: &T, pretty: bool) -> Result<String, serde_json::Error> {
    let compact = serde_json::to_string(value)?;
    let mut out = String::with_capacity(compact.len() * 2);
    write_sorted(&mut out, &compact, 0, pretty)?;
    Ok(out)
}

/// Writes `json` (compact, valid) re-laid-out from `depth`, recursing into objects and arrays; leaves are copied.
fn write_sorted(out: &mut String, json: &str, depth: usize, pretty: bool) -> Result<(), serde_json::Error> {
    match json.as_bytes().first() {
        Some(b'{') => {
            let members: BTreeMap<String, &RawValue> = serde_json::from_str(json)?;
            let colon = if pretty { ": " } else { ":" };
            write_members(out, depth, pretty, ('{', '}'), members.iter(), |out, (key, value)| {
                let _ = write!(out, "{}{colon}", serde_json::to_string(key)?);
                write_sorted(out, value.get(), depth + 1, pretty)
            })
        }
        Some(b'[') => {
            let items: Vec<&RawValue> = serde_json::from_str(json)?;
            write_members(out, depth, pretty, ('[', ']'), items.iter(), |out, item| {
                write_sorted(out, item.get(), depth + 1, pretty)
            })
        }
        _ => {
            out.push_str(json);
            Ok(())
        }
    }
}

/// The shared layout of objects and arrays: one member per line when `pretty`, and `{}`/`[]` when empty.
fn write_members<I: ExactSizeIterator>(
    out: &mut String,
    depth: usize,
    pretty: bool,
    (open, close): (char, char),
    members: I,
    mut write_member: impl FnMut(&mut String, I::Item) -> Result<(), serde_json::Error>,
) -> Result<(), serde_json::Error> {
    let newline = |out: &mut String, depth: usize| {
        if pretty {
            out.push('\n');
            out.push_str(&INDENT.repeat(depth));
        }
    };
    out.push(open);
    let empty = members.len() == 0;
    for (i, member) in members.enumerate() {
        if i > 0 {
            out.push(',');
        }
        newline(out, depth + 1);
        write_member(out, member)?;
    }
    if !empty {
        newline(out, depth);
    }
    out.push(close);
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Field order here is deliberately not alphabetical, and does not depend on any `serde_json` feature: a
    /// derived `Serialize` always emits fields in declaration order.
    #[derive(Serialize)]
    struct Outer {
        zeta: u8,
        alpha: Inner,
        mid: Vec<Inner>,
    }

    #[derive(Serialize)]
    struct Inner {
        yankee: &'static str,
        bravo: f32,
    }

    #[test]
    fn keys_are_sorted_at_every_depth_including_inside_arrays() {
        let value =
            Outer { zeta: 1, alpha: Inner { yankee: "y", bravo: 0.1 }, mid: vec![Inner { yankee: "a", bravo: 2.0 }] };
        let text = pretty(&value).unwrap();
        let expected = "{\n  \"alpha\": {\n    \"bravo\": 0.1,\n    \"yankee\": \"y\"\n  },\n  \"mid\": [\n    {\n      \
                        \"bravo\": 2.0,\n      \"yankee\": \"a\"\n    }\n  ],\n  \"zeta\": 1\n}\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn numbers_print_exactly_as_serde_json_printed_them() {
        // Whatever form serde_json chose (1e+300, 18446744073709551615, ...) is copied, not re-formatted.
        for number in [json!(u64::MAX), json!(-3), json!(1e300), json!(0.1_f64 + 0.2)] {
            let expected = number.to_string();
            assert_eq!(pretty(&json!({ "n": number })).unwrap(), format!("{{\n  \"n\": {expected}\n}}\n"));
        }
        // An f32 keeps its shortest form (a round trip through `Value` would print 0.10000000149011612).
        assert_eq!(pretty(&0.1_f32).unwrap(), "0.1\n");
    }

    #[test]
    fn layout_matches_serde_jsons_own_pretty_printer() {
        // `json!` keeps its keys sorted unless preserve_order is on, so serde_json's pretty output is already the
        // canonical form here and must be reproduced byte for byte.
        let value = json!({
            "a": [], "b": {}, "c": [1, [2, {"x": null}], {}], "d": "he said \"hi\"\n\u{e9}\u{1f600}",
            "e": {"k\"ey": true, "nested": {"deep": [false]}}
        });
        let expected = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
        assert_eq!(pretty(&value).unwrap(), expected);
    }

    #[test]
    fn scalars_and_empty_containers_work_at_the_top_level() {
        assert_eq!(pretty(&json!(null)).unwrap(), "null\n");
        assert_eq!(pretty("text").unwrap(), "\"text\"\n");
        assert_eq!(pretty(&Vec::<u8>::new()).unwrap(), "[]\n");
        assert_eq!(pretty(&serde_json::Map::new()).unwrap(), "{}\n");
    }

    #[test]
    fn render_value_sorts_keys_on_one_line_or_many() {
        let value = json!({"z": [1, {"b": 1, "a": [] }], "a": "x"});
        assert_eq!(render_value(&value, false), r#"{"a":"x","z":[1,{"a":[],"b":1}]}"#);
        let many = "{\n  \"a\": \"x\",\n  \"z\": [\n    1,\n    {\n      \"a\": [],\n      \"b\": 1\n    }\n  ]\n}";
        assert_eq!(render_value(&value, true), many);
        assert_eq!(render_value(&json!({}), false), "{}");
    }

    #[test]
    fn output_is_valid_json_with_the_same_content() {
        let value = json!({"z": [1, 2, {"b": 1, "a": 2}], "a": "x"});
        let reparsed: serde_json::Value = serde_json::from_str(&pretty(&value).unwrap()).unwrap();
        assert_eq!(reparsed, value);
    }
}
