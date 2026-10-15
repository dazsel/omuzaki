//! One settings file as text: reading keys out of it and merging edits into it without disturbing anything else.
//!
//! The file is edited with `toml_edit`, so comments, order, spacing and keys no build registers survive a write (D5).
//! A key `ui.scale` is `scale` under `[ui]`. A key's stored version lives in the `[versions]` table, which lists only
//! keys above version 1; that table name is therefore reserved and no setting may use `versions` as its area.

use serde_json::Value;
use toml_edit::{DocumentMut, Item, Table, TableLike};

/// The table that lists the stored version of every key above version 1.
const VERSIONS: &str = "versions";

/// The format marker written into a file this store creates.
const FORMAT: i64 = 1;

/// One change to apply to a file: the key's new value and version, or its removal.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Edit {
    /// The dotted setting key.
    pub key: String,
    /// The new value and the key's current version; `None` removes the key.
    pub value: Option<(Value, u32)>,
}

/// Why an edit could not be merged into a file's text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApplyError {
    /// The text is not TOML.
    Unparsable(String),
    /// A key's path runs through something that is not a table, or its value has no TOML form.
    Conflict(String),
}

/// Parses a settings file.
pub(crate) fn parse(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>().map_err(|e| e.to_string())
}

/// The value stored at the dotted `key`, as JSON.
pub(crate) fn lookup(doc: &DocumentMut, key: &str) -> Option<Value> {
    let mut item = doc.as_item();
    for segment in key.split('.') {
        item = item.get(segment)?;
    }
    Some(item_to_json(item))
}

/// The version a key was stored at; absent means 1 (D5).
pub(crate) fn stored_version(doc: &DocumentMut, key: &str) -> u32 {
    doc.get(VERSIONS)
        .and_then(|t| t.get(key))
        .and_then(Item::as_integer)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v >= 1)
        .unwrap_or(1)
}

fn item_to_json(item: &Item) -> Value {
    match item {
        Item::None => Value::Null,
        Item::Value(v) => value_to_json(v),
        Item::Table(t) => table_to_json(t),
        Item::ArrayOfTables(a) => Value::Array(a.iter().map(|t| table_to_json(t)).collect()),
    }
}

fn table_to_json(table: &dyn TableLike) -> Value {
    Value::Object(table.iter().map(|(k, v)| (k.to_owned(), item_to_json(v))).collect())
}

fn value_to_json(value: &toml_edit::Value) -> Value {
    use toml_edit::Value as T;
    match value {
        T::String(s) => Value::String(s.value().clone()),
        T::Integer(i) => Value::from(*i.value()),
        T::Float(f) => serde_json::Number::from_f64(*f.value()).map_or(Value::Null, Value::Number),
        T::Boolean(b) => Value::Bool(*b.value()),
        T::Datetime(d) => Value::String(d.value().to_string()),
        T::Array(a) => Value::Array(a.iter().map(value_to_json).collect()),
        T::InlineTable(t) => table_to_json(t),
    }
}

/// A JSON value as TOML, or why TOML cannot hold it.
pub(crate) fn json_to_toml(value: &Value) -> Result<toml_edit::Value, String> {
    Ok(match value {
        Value::Null => return Err("TOML has no null".to_owned()),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.into(),
            (None, _) if n.is_u64() => return Err(format!("{n} does not fit a TOML integer")),
            (None, Some(f)) => f.into(),
            (None, None) => return Err(format!("{n} is not a TOML number")),
        },
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => items.iter().map(json_to_toml).collect::<Result<toml_edit::Array, _>>()?.into(),
        Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();
            for (k, v) in map {
                table.insert(k, json_to_toml(v)?);
            }
            table.into()
        }
    })
}

/// Applies `edits` to `text` (a missing file is empty) and returns the new text.
///
/// Only the edited keys, their `[versions]` entries and, for a new file, the `format` marker change; every other byte
/// of an existing file stays as it was.
pub(crate) fn apply(text: Option<&str>, edits: &[Edit]) -> Result<String, ApplyError> {
    let fresh = text.is_none_or(|t| t.trim().is_empty());
    let mut doc = match text {
        Some(t) => parse(t).map_err(ApplyError::Unparsable)?,
        None => DocumentMut::new(),
    };
    for edit in edits {
        let segments: Vec<&str> = edit.key.split('.').collect();
        if let Some((value, version)) = &edit.value {
            let new = json_to_toml(value).map_err(|e| ApplyError::Conflict(format!("`{}`: {e}", edit.key)))?;
            put(doc.as_table_mut(), &segments, new).map_err(ApplyError::Conflict)?;
            set_version(&mut doc, &edit.key, *version);
        } else {
            remove(doc.as_table_mut(), &segments);
            set_version(&mut doc, &edit.key, 1);
        }
    }
    if fresh {
        doc.insert("format", toml_edit::value(FORMAT));
    }
    Ok(doc.to_string())
}

fn put(table: &mut dyn TableLike, segments: &[&str], new: toml_edit::Value) -> Result<(), String> {
    match segments {
        [] => Err("an empty key".to_owned()),
        [leaf] => {
            // Keep the old value's decor so a trailing comment on the line survives.
            if let Some(old) = table.get_mut(leaf).and_then(Item::as_value_mut) {
                let decor = old.decor().clone();
                *old = new;
                *old.decor_mut() = decor;
            } else {
                table.insert(leaf, Item::Value(new));
            }
            Ok(())
        }
        [head, rest @ ..] => {
            if !table.contains_key(head) {
                let mut created = Table::new();
                created.set_implicit(true);
                table.insert(head, Item::Table(created));
            }
            let next = table
                .get_mut(head)
                .and_then(Item::as_table_like_mut)
                .ok_or_else(|| format!("`{head}` is a value, not a table"))?;
            put(next, rest, new)
        }
    }
}

fn remove(table: &mut dyn TableLike, segments: &[&str]) {
    match segments {
        [] => {}
        [leaf] => {
            table.remove(leaf);
        }
        [head, rest @ ..] => {
            if let Some(next) = table.get_mut(head).and_then(Item::as_table_like_mut) {
                remove(next, rest);
            }
        }
    }
}

fn set_version(doc: &mut DocumentMut, key: &str, version: u32) {
    if version > 1 {
        let versions = doc.entry(VERSIONS).or_insert_with(|| Item::Table(Table::new()));
        if let Some(table) = versions.as_table_like_mut() {
            table.insert(key, toml_edit::value(i64::from(version)));
        }
    } else if let Some(table) = doc.get_mut(VERSIONS).and_then(Item::as_table_like_mut) {
        table.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn value_text() -> impl Strategy<Value = String> {
        prop_oneof![
            (0i64..1000).prop_map(|n| n.to_string()),
            "[a-z]{1,6}".prop_map(|s| format!("\"{s}\"")),
            Just("true".to_owned()),
            Just("[1, 2, 3]".to_owned()),
            Just("{ a = 1, b = \"x\" }".to_owned()),
        ]
    }

    /// One `key = value` line with optional comment lines above it and a trailing comment.
    fn entry() -> impl Strategy<Value = String> {
        (proptest::option::of("[a-z ]{0,12}"), value_text(), proptest::option::of("[a-z ]{0,12}")).prop_map(
            |(lead, value, trail)| {
                let lead = lead.map_or(String::new(), |c| format!("# {c}\n"));
                let trail = trail.map_or(String::new(), |c| format!(" # {c}"));
                format!("{lead}KEY = {value}{trail}\n")
            },
        )
    }

    /// A valid settings file: sections of entries (some for the edited keys, some unknown), blank lines and comments.
    fn file_text() -> impl Strategy<Value = String> {
        let sections: [(&'static str, &'static [&'static str]); 4] = [
            ("ui", &["scale", "theme", "x1", "x2"]),
            ("quality", &["tier", "y1"]),
            ("future_feature", &["f1", "f2", "f3"]),
            ("misc.deep", &["m"]),
        ];
        let parts: Vec<_> = sections
            .iter()
            .map(|(name, keys)| {
                let keys: Vec<&'static str> = keys.to_vec();
                let name: &'static str = name;
                proptest::option::of(proptest::collection::btree_map(proptest::sample::select(keys), entry(), 0..4))
                    .prop_map(move |body| (name, body))
            })
            .collect();
        (parts, proptest::option::of("[a-z ]{0,12}"), proptest::bool::ANY).prop_map(|(parts, head, versions)| {
            let mut text = head.map_or(String::new(), |c| format!("# {c}\n"));
            text.push_str("format = 1\n");
            for (name, body) in parts {
                let Some(body) = body else { continue };
                write!(text, "\n[{name}]\n").unwrap();
                for (key, entry) in body {
                    text.push_str(&entry.replace("KEY", key));
                }
            }
            if versions {
                text.push_str("\n[versions]\n\"ui.scale\" = 2\n");
            }
            text
        })
    }

    fn edits() -> impl Strategy<Value = Vec<Edit>> {
        let one = (
            proptest::sample::select(vec!["ui.scale", "ui.theme", "quality.tier", "brand.new", "brand.deep.er"]),
            proptest::option::of((value_text(), 1u32..4)),
        )
            .prop_map(|(key, value)| Edit {
                key: key.to_owned(),
                value: value.map(|(text, version)| {
                    let parsed = parse(&format!("v = {text}")).map(|d| lookup(&d, "v"));
                    (parsed.ok().flatten().unwrap_or(Value::Null), version)
                }),
            });
        // The store keeps one pending change per key, so an edit set has each key once.
        proptest::collection::vec(one, 0..5).prop_map(|edits| {
            let unique: BTreeMap<String, Edit> = edits.into_iter().map(|e| (e.key.clone(), e)).collect();
            unique.into_values().collect()
        })
    }

    /// The lines that belong to a key an edit may change (or the `[versions]` table). A comment line directly above
    /// such a key describes it and goes when the key goes, so it counts as the key's own.
    fn is_editable_line(line: &str) -> bool {
        let key = line.split('=').next().unwrap_or("").trim().trim_matches('"');
        let bare = key.rsplit('.').next().unwrap_or(key);
        ["scale", "theme", "tier"].contains(&bare) || line.starts_with("\"ui.")
    }

    fn unrelated_lines(old: &str) -> Vec<&str> {
        let lines: Vec<&str> = old.lines().collect();
        let mut owned = vec![false; lines.len()];
        for i in (0..lines.len()).rev() {
            let next_owned = owned.get(i + 1).copied().unwrap_or(false);
            owned[i] = is_editable_line(lines[i]) || (lines[i].starts_with('#') && next_owned);
        }
        lines.into_iter().zip(owned).filter(|(_, owned)| !owned).map(|(l, _)| l).collect()
    }

    fn assert_subsequence(old: &str, new: &str) -> Result<(), TestCaseError> {
        let mut rest = new.lines();
        for line in unrelated_lines(old) {
            prop_assert!(rest.any(|n| n == line), "line `{line}` was lost or moved\n--- old\n{old}\n--- new\n{new}");
        }
        Ok(())
    }

    proptest! {
        #[test]
        fn unrelated_keys_and_comments_stay_byte_identical(text in file_text(), edits in edits()) {
            let new = apply(Some(&text), &edits).unwrap();
            assert_subsequence(&text, &new)?;
            // The last edit of each key wins and reads back exactly.
            let mut last: BTreeMap<&str, &Option<(Value, u32)>> = BTreeMap::new();
            for e in &edits {
                last.insert(&e.key, &e.value);
            }
            let doc = parse(&new).unwrap();
            for (key, value) in last {
                if let Some((v, version)) = value {
                    let (got, stored) = (lookup(&doc, key), stored_version(&doc, key));
                    prop_assert_eq!(got.as_ref(), Some(v));
                    prop_assert_eq!(stored, *version);
                } else {
                    prop_assert_eq!(lookup(&doc, key), None);
                }
            }
        }

        #[test]
        fn applying_the_same_edits_twice_changes_nothing_more(text in file_text(), edits in edits()) {
            let once = apply(Some(&text), &edits).unwrap();
            prop_assert_eq!(apply(Some(&once), &edits).unwrap(), once);
        }

        #[test]
        fn values_survive_a_round_trip_through_a_file(n in any::<i64>(), s in "\\PC{0,20}", f in -1e6f64..1e6) {
            let value = json!({ "n": n, "s": s, "f": f, "list": [n, s], "flag": true });
            let text = apply(None, &[Edit { key: "a.b".into(), value: Some((value.clone(), 1)) }]).unwrap();
            prop_assert_eq!(lookup(&parse(&text).unwrap(), "a.b"), Some(value));
        }
    }

    #[test]
    fn the_unedited_keys_of_a_known_file_do_not_move() {
        let text = "# top\nformat = 1\n\n[ui]\n# why\nscale = 125 # mine\n\n[other]\nk = 1\n";
        let edit = Edit { key: "ui.scale".into(), value: Some((json!(130), 1)) };
        assert_eq!(apply(Some(text), &[edit]).unwrap(), text.replace("125", "130"));
    }

    #[test]
    fn a_new_file_gets_its_format_marker_and_a_value_without_toml_form_is_refused() {
        let new = apply(None, &[Edit { key: "ui.scale".into(), value: Some((json!(5), 1)) }]).unwrap();
        assert_eq!(new, "format = 1\n\n[ui]\nscale = 5\n");
        let null = Edit { key: "ui.x".into(), value: Some((Value::Null, 1)) };
        assert!(matches!(apply(None, &[null]), Err(ApplyError::Conflict(_))));
        let through_a_value = Edit { key: "ui.scale.deep".into(), value: Some((json!(1), 1)) };
        assert!(matches!(apply(Some("[ui]\nscale = 1\n"), &[through_a_value]), Err(ApplyError::Conflict(_))));
        assert!(matches!(apply(Some("[ui"), &[]), Err(ApplyError::Unparsable(_))));
    }

    #[test]
    fn dotted_and_inline_forms_are_read_like_tables() {
        let doc = parse("ui.scale = 5\naudio = { bindings = { voice = \"x\" } }\n").unwrap();
        assert_eq!(lookup(&doc, "ui.scale"), Some(json!(5)));
        assert_eq!(lookup(&doc, "audio.bindings.voice"), Some(json!("x")));
        assert_eq!(lookup(&doc, "audio.bindings"), Some(json!({ "voice": "x" })));
    }
}
