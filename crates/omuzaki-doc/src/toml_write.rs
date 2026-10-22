//! Document values → TOML text, editing in place.
//!
//! Saving a file is never "serialise the value and overwrite". The file's current text is parsed with `toml_edit`
//! and only the parts that differ from the new value are touched, so comments, blank lines, key order and number
//! formatting that a person wrote survive every save. A value that is already equal is left alone, which is also how
//! a date written as `2026-10-11` stays a date while the tree sees a string.
//!
//! Content that is new (a key that wasn't there, a replaced value) is written in a plain, stable style: keys in the
//! tree's sorted order, sub-tables as `[a.b]` sections, lists of objects as `[[a]]`, everything else inline.

use serde_json::{Map, Number, Value};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike};

use crate::toml_read::strip_bom;

/// Why a value can't be written to a TOML file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Unstorable {
    /// What is wrong.
    pub reason: &'static str,
    /// Where, relative to the value that was checked, as a `/` path (empty for the value itself).
    pub at: String,
}

/// Why a file couldn't be rendered.
#[derive(Debug)]
pub(crate) enum RenderError {
    /// The existing text isn't valid TOML, so there is nothing safe to edit.
    Syntax(toml_edit::TomlError),
    /// The new value can't be stored.
    Unstorable(Unstorable),
}

impl Unstorable {
    fn new(reason: &'static str) -> Self {
        Self { reason, at: String::new() }
    }

    /// Prefixes the location with `segment` (errors are built from the inside out).
    fn within(mut self, segment: impl std::fmt::Display) -> Self {
        self.at = if self.at.is_empty() { segment.to_string() } else { format!("{segment}/{}", self.at) };
        self
    }
}

/// Checks that `value` and everything under it can be written to TOML: no `null`, and integers within `i64`.
pub(crate) fn check_storable(value: &Value) -> Result<(), Unstorable> {
    match value {
        Value::Null => Err(Unstorable::new("TOML can't store null; remove the key instead")),
        Value::Number(n) => number(n).map(drop),
        Value::Array(items) => {
            items.iter().enumerate().try_for_each(|(i, v)| check_storable(v).map_err(|e| e.within(i)))
        }
        Value::Object(map) => map.iter().try_for_each(|(k, v)| check_storable(v).map_err(|e| e.within(k))),
        Value::Bool(_) | Value::String(_) => Ok(()),
    }
}

/// The text of a file holding `table`, edited from `existing` (the file's current text) when there is one.
pub(crate) fn render(existing: Option<&str>, table: &Map<String, Value>) -> Result<String, RenderError> {
    let mut doc = match existing {
        Some(text) => strip_bom(text).parse::<DocumentMut>().map_err(RenderError::Syntax)?,
        None => DocumentMut::new(),
    };
    sync_table(doc.as_table_mut(), table, false).map_err(RenderError::Unstorable)?;
    let text = doc.to_string();
    // The parser reads `\r\n` as `\n`, so a file written on Windows would come back with every line changed.
    Ok(if existing.is_some_and(uses_crlf) { text.replace('\n', "\r\n") } else { text })
}

/// True if the first line break in `text` is `\r\n`.
fn uses_crlf(text: &str) -> bool {
    text.find('\n').is_some_and(|i| text[..i].ends_with('\r'))
}

fn number(n: &Number) -> Result<toml_edit::Value, Unstorable> {
    n.as_i64()
        .map(toml_edit::Value::from)
        .or_else(|| n.as_f64().filter(|_| n.is_f64()).map(toml_edit::Value::from))
        .ok_or(Unstorable::new("integers above 9223372036854775807 can't be stored in TOML"))
}

/// Makes `table` hold exactly `map`, touching as little as possible. `inline` says whether `table` is an inline
/// table, where new children must be inline too.
fn sync_table(table: &mut dyn TableLike, map: &Map<String, Value>, inline: bool) -> Result<(), Unstorable> {
    let stale: Vec<String> = table.iter().filter(|(k, _)| !map.contains_key(*k)).map(|(k, _)| k.to_owned()).collect();
    for key in stale {
        table.remove(&key);
    }
    // A dotted table (`a.b = 1`) can't hold `[sections]`, so anything new under one goes inline.
    let inline = inline || table.is_dotted();
    for (key, value) in map {
        let synced = match table.get_mut(key) {
            Some(item) => sync_item(item, value),
            None => new_item(value, inline).map(|item| {
                table.insert(key, item);
            }),
        };
        synced.map_err(|e| e.within(key))?;
    }
    Ok(())
}

fn sync_item(item: &mut Item, value: &Value) -> Result<(), Unstorable> {
    if item_eq(item, value) {
        return Ok(());
    }
    match (&mut *item, value) {
        (Item::Table(table), Value::Object(map)) => {
            let was_empty = table.is_empty();
            sync_table(table, map, false)?;
            // A header written only to lead to sub-tables (`[a.b]`) is implicit, and an implicit table that has
            // lost everything would vanish from the file instead of staying as an empty `[a]`. An empty one that
            // gains only sub-tables goes back to implicit, so emptying and refilling it (an undo) gives back the
            // same text.
            if table.is_empty() {
                table.set_implicit(false);
            } else if was_empty && table.iter().all(|(_, i)| i.is_table() || i.is_array_of_tables()) {
                table.set_implicit(true);
            }
            Ok(())
        }
        (Item::ArrayOfTables(tables), Value::Array(items)) if table_array(items).is_some() => {
            sync_table_array(tables, items)
        }
        (Item::Value(current), value) => sync_value(current, value),
        _ => {
            *item = new_item(value, false)?;
            Ok(())
        }
    }
}

fn sync_value(current: &mut toml_edit::Value, value: &Value) -> Result<(), Unstorable> {
    if value_eq(current, value) {
        return Ok(());
    }
    match (&mut *current, value) {
        (toml_edit::Value::InlineTable(table), Value::Object(map)) => sync_table(table, map, true),
        (toml_edit::Value::Array(array), Value::Array(items)) => sync_array(array, items),
        _ => {
            // Keep the spacing and the trailing `# comment` of the value being replaced.
            let mut new = new_value(value)?;
            *new.decor_mut() = current.decor().clone();
            *current = new;
            Ok(())
        }
    }
}

fn sync_array(array: &mut Array, items: &[Value]) -> Result<(), Unstorable> {
    while array.len() > items.len() {
        array.remove(array.len() - 1);
    }
    for (i, value) in items.iter().enumerate() {
        match array.get_mut(i) {
            Some(current) => sync_value(current, value),
            None => new_value(value).map(|v| array.push(v)),
        }
        .map_err(|e| e.within(i))?;
    }
    Ok(())
}

fn sync_table_array(tables: &mut ArrayOfTables, items: &[Value]) -> Result<(), Unstorable> {
    while tables.len() > items.len() {
        tables.remove(tables.len() - 1);
    }
    for (i, value) in items.iter().enumerate() {
        let Value::Object(map) = value else { continue };
        match tables.get_mut(i) {
            Some(table) => sync_table(table, map, false),
            None => new_table(map).map(|t| tables.push(t)),
        }
        .map_err(|e| e.within(i))?;
    }
    Ok(())
}

fn new_item(value: &Value, inline: bool) -> Result<Item, Unstorable> {
    match value {
        Value::Object(map) if !inline => new_table(map).map(Item::Table),
        Value::Array(items) if !inline => match table_array(items) {
            Some(maps) => {
                let mut tables = ArrayOfTables::new();
                for (i, map) in maps.into_iter().enumerate() {
                    tables.push(new_table(map).map_err(|e| e.within(i))?);
                }
                Ok(Item::ArrayOfTables(tables))
            }
            None => new_value(value).map(Item::Value),
        },
        _ => new_value(value).map(Item::Value),
    }
}

fn new_table(map: &Map<String, Value>) -> Result<Table, Unstorable> {
    let mut table = Table::new();
    for (key, value) in map {
        table.insert(key, new_item(value, false).map_err(|e| e.within(key))?);
    }
    // A table holding only sub-tables needs no header of its own: `[a.b]` says it all.
    table.set_implicit(!table.is_empty() && table.iter().all(|(_, i)| i.is_table() || i.is_array_of_tables()));
    Ok(table)
}

fn new_value(value: &Value) -> Result<toml_edit::Value, Unstorable> {
    Ok(match value {
        Value::Null => return Err(Unstorable::new("TOML can't store null; remove the key instead")),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => number(n)?,
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => {
            let mut array = Array::new();
            for (i, item) in items.iter().enumerate() {
                array.push(new_value(item).map_err(|e| e.within(i))?);
            }
            array.into()
        }
        Value::Object(map) => {
            let mut table = InlineTable::new();
            for (key, item) in map {
                table.insert(key, new_value(item).map_err(|e| e.within(key))?);
            }
            table.into()
        }
    })
}

/// The objects in `items` if it is a non-empty list of objects, which TOML can write as `[[name]]` sections.
/// (An empty list must stay `[]`: zero `[[name]]` sections would make the key vanish.)
fn table_array(items: &[Value]) -> Option<Vec<&Map<String, Value>>> {
    let maps: Vec<_> = items.iter().filter_map(Value::as_object).collect();
    (!items.is_empty() && maps.len() == items.len()).then_some(maps)
}

fn item_eq(item: &Item, value: &Value) -> bool {
    match (item, value) {
        (Item::Value(v), value) => value_eq(v, value),
        (Item::Table(t), Value::Object(map)) => table_eq(t, map),
        (Item::ArrayOfTables(tables), Value::Array(items)) => {
            tables.len() == items.len()
                && tables.iter().zip(items).all(|(t, v)| matches!(v, Value::Object(m) if table_eq(t, m)))
        }
        _ => false,
    }
}

fn value_eq(current: &toml_edit::Value, value: &Value) -> bool {
    use toml_edit::Value as T;
    match (current, value) {
        (T::String(a), Value::String(b)) => a.value() == b,
        (T::Integer(a), Value::Number(b)) => b.as_i64() == Some(*a.value()),
        (T::Float(a), Value::Number(b)) => b.is_f64() && b.as_f64() == Some(*a.value()),
        (T::Boolean(a), Value::Bool(b)) => a.value() == b,
        (T::Datetime(a), Value::String(b)) => a.value().to_string() == *b,
        (T::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| value_eq(x, y)),
        (T::InlineTable(a), Value::Object(b)) => table_eq(a, b),
        _ => false,
    }
}

fn table_eq(table: &dyn TableLike, map: &Map<String, Value>) -> bool {
    table.len() == map.len() && map.iter().all(|(k, v)| table.get(k).is_some_and(|item| item_eq(item, v)))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::toml_read::parse_table;

    fn map(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    fn fresh(v: Value) -> String {
        render(None, &map(v)).unwrap()
    }

    #[test]
    fn a_new_file_is_plain_and_stable() {
        let text = fresh(json!({
            "name": "mira",
            "size": 4,
            "tags": ["a", "b"],
            "eyes": {"iris": {"hue": 0.5}, "lid": 1},
            "layers": [{"id": "base"}, {"id": "shine"}],
        }));
        assert_eq!(
            text,
            "name = \"mira\"\nsize = 4\ntags = [\"a\", \"b\"]\n\n[eyes]\nlid = 1\n\n[eyes.iris]\nhue = 0.5\n\n\
             [[layers]]\nid = \"base\"\n\n[[layers]]\nid = \"shine\"\n"
        );
    }

    #[test]
    fn comments_order_and_formatting_survive_an_unrelated_change() {
        let before =
            "# face profile\nprofile = \"round\"  # keep\nblend =   0.50\n\n# the eyes\n[eyes]\nsize = 4 # px\n";
        let mut value = Value::Object(parse_table(before).unwrap());
        value["eyes"]["size"] = json!(5);
        let after = render(Some(before), &map(value)).unwrap();
        assert_eq!(
            after,
            "# face profile\nprofile = \"round\"  # keep\nblend =   0.50\n\n# the eyes\n[eyes]\nsize = 5 # px\n"
        );
    }

    #[test]
    fn an_equal_value_renders_the_exact_same_text() {
        let text = "a=1\nb   = [ 1,2 ,  3 ]\n[t]\n  c = 'x'\nd = 2026-10-11\n";
        let value = parse_table(text).unwrap();
        assert_eq!(render(Some(text), &value).unwrap(), text);
    }

    #[test]
    fn windows_line_endings_are_kept() {
        let before = "# note\r\na = 1\r\n\r\n[t]\r\nx = 1\r\n";
        let same = render(Some(before), &parse_table(before).unwrap()).unwrap();
        assert_eq!(same, before);
        let edited = render(Some(before), &map(json!({"a": 2, "b": 3, "t": {"x": 1, "y": 2}}))).unwrap();
        assert_eq!(edited, "# note\r\na = 2\r\nb = 3\r\n\r\n[t]\r\nx = 1\r\ny = 2\r\n");
        assert_eq!(render(Some("a = 1\n"), &map(json!({"a": 2}))).unwrap(), "a = 2\n");
    }

    #[test]
    fn keys_added_and_removed() {
        let before = "a = 1\nb = 2\n\n[t]\nx = 1\n";
        let after = render(Some(before), &map(json!({"a": 1, "c": 3, "t": {"y": 2}}))).unwrap();
        assert_eq!(Value::Object(parse_table(&after).unwrap()), json!({"a": 1, "c": 3, "t": {"y": 2}}));
        assert!(after.starts_with("a = 1\n"));
    }

    #[test]
    fn lists_edit_element_by_element() {
        let before = "xs = [1, 2, 3]\n\n[[ys]]\n# first\nid = 1\n\n[[ys]]\nid = 2\n";
        let after = render(Some(before), &map(json!({"xs": [1, 9], "ys": [{"id": 1}]}))).unwrap();
        assert_eq!(after, "xs = [1, 9]\n\n[[ys]]\n# first\nid = 1\n");
    }

    #[test]
    fn a_replaced_scalar_keeps_its_trailing_comment() {
        let after = render(Some("a = 1 # note\n"), &map(json!({"a": "text"}))).unwrap();
        assert_eq!(after, "a = \"text\" # note\n");
    }

    #[test]
    fn a_table_emptied_of_its_sub_tables_stays_as_an_empty_table() {
        let before = "v = false\n\n[a.a]\nc = false\n"; // `a` exists only to hold `a.a`
        let after = render(Some(before), &map(json!({"v": false, "a": {}}))).unwrap();
        assert_eq!(Value::Object(parse_table(&after).unwrap()), json!({"v": false, "a": {}}));
        let nested = render(Some(before), &map(json!({"v": false, "a": {"a": {}}}))).unwrap();
        assert_eq!(Value::Object(parse_table(&nested).unwrap()), json!({"v": false, "a": {"a": {}}}));
    }

    #[test]
    fn a_table_emptied_and_refilled_renders_the_text_it_started_from() {
        let before = "v = false\n\n[a.b]\nc = 1\n";
        let emptied = render(Some(before), &map(json!({"v": false, "a": {}}))).unwrap();
        let refilled = render(Some(&emptied), &map(json!({"v": false, "a": {"b": {"c": 1}}}))).unwrap();
        assert_eq!(refilled, before);
    }

    #[test]
    fn empty_containers_survive() {
        let value = json!({"t": {}, "xs": [], "u": {"v": {}}});
        let text = fresh(value.clone());
        assert_eq!(Value::Object(parse_table(&text).unwrap()), value);
    }

    #[test]
    fn dates_stay_dates_until_changed() {
        let before = "born = 2026-10-11\n";
        let kept = render(Some(before), &map(json!({"born": "2026-10-11"}))).unwrap();
        assert_eq!(kept, before);
        let changed = render(Some(before), &map(json!({"born": "later"}))).unwrap();
        assert_eq!(changed, "born = \"later\"\n");
    }

    #[test]
    fn int_and_float_stay_distinct() {
        let text = fresh(json!({"i": 1, "f": 1.0}));
        let back = parse_table(&text).unwrap();
        assert!(back["i"].is_i64() && back["f"].is_f64());
    }

    #[test]
    fn nulls_and_huge_integers_are_unstorable_with_their_location() {
        let err = check_storable(&json!({"a": [1, {"b": null}]})).unwrap_err();
        assert_eq!(err.at, "a/1/b");
        assert!(check_storable(&json!(u64::MAX)).is_err());
        assert!(check_storable(&json!(i64::MAX)).is_ok());
        assert!(matches!(render(None, &map(json!({"a": {"b": null}}))), Err(RenderError::Unstorable(_))));
    }

    #[test]
    fn broken_existing_text_is_an_error_not_an_overwrite() {
        assert!(matches!(render(Some("a = \n"), &map(json!({"a": 1}))), Err(RenderError::Syntax(_))));
    }

    /// Mostly three short names, so that two generated tables share keys and editing one into the other has work to
    /// do; sometimes any text at all.
    fn key() -> impl Strategy<Value = String> {
        prop_oneof![4 => "[abc]", 1 => any::<String>()]
    }

    /// Any value a file can hold: no null, integers within i64, finite floats; keys and strings are arbitrary text.
    fn storable_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Value::from),
            any::<String>().prop_map(Value::String),
        ];
        leaf.prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::btree_map(key(), inner, 0..4).prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    fn storable_table() -> impl Strategy<Value = Map<String, Value>> {
        prop::collection::btree_map(key(), storable_value(), 0..5).prop_map(|m| m.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            failure_persistence: None,
            rng_seed: proptest::test_runner::RngSeed::Fixed(0x5EED),
            ..ProptestConfig::default()
        })]

        /// What is written reads back as the same value, whether the file is new or edited from another one.
        #[test]
        fn rendering_then_parsing_is_the_identity(a in storable_table(), b in storable_table()) {
            let fresh = render(None, &a).unwrap();
            prop_assert_eq!(&parse_table(&fresh).unwrap(), &a);
            let edited = render(Some(&fresh), &b).unwrap();
            prop_assert_eq!(&parse_table(&edited).unwrap(), &b);
            // Rendering a value that already matches changes nothing.
            prop_assert_eq!(render(Some(&edited), &b).unwrap(), edited);
        }
    }

    #[test]
    fn dotted_keys_and_inline_tables_take_edits() {
        let before = "a.b = 1\nc = { d = 1, e = 2 }\n";
        let want = json!({"a": {"b": 2, "n": {"m": 1}}, "c": {"d": 1, "e": 3, "f": [1]}});
        let after = render(Some(before), &map(want.clone())).unwrap();
        assert_eq!(Value::Object(parse_table(&after).unwrap()), want);
    }
}
