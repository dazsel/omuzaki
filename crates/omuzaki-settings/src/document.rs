//! One settings file as text: reading keys out of it and merging edits into it without disturbing anything else.
//!
//! The file is edited as text (with `toml_edit` locating the bytes), so comments, order, spacing, line endings and keys
//! no build registers survive a write byte for byte (D5).
//! A key `ui.scale` is `scale` under `[ui]`. A key's stored version lives in the `[versions]` table, which lists only
//! keys above version 1; that table name is therefore reserved and no setting may use `versions` as its area.

use std::collections::BTreeMap;

use serde_json::Value;
use toml_edit::{Document, DocumentMut, InlineTable, Item, Key, Table, TableLike};

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

/// The result of [`apply_each`]: the new text, and the edits it could not make.
#[derive(Debug)]
pub(crate) struct Applied {
    /// The text with every edit that could be made.
    pub text: String,
    /// The key and reason of each edit the file's shape refused; the text is as if the edit was never asked.
    pub refused: Vec<(String, String)>,
}

/// Applies `edits` to `text` (a missing file is empty) one at a time, so an edit the file cannot take is reported
/// beside the others instead of failing them. Only text that is not TOML fails the whole call.
///
/// An existing file is edited as text: a changed value replaces only its own bytes, a removed key takes its own line,
/// and a new key is inserted at the end of its table (or in a new table at the end of the file), using the file's own
/// line ending. Every other byte stays as it was, so interleaved dotted keys are not reordered and `\r\n` stays
/// `\r\n`; keys among dotted keys and inside inline tables are edited in place too, and a setting stored as
/// `[tables]` is replaced or removed whole. A shape that cannot be edited in place (a key under an array of tables, a
/// new key in a dotted inline table) is refused, never a rewrite of the file. A new file is built whole, with the
/// `format` marker.
pub(crate) fn apply_each(
    text: Option<&str>,
    edits: &[Edit],
    needs: &BTreeMap<String, String>,
) -> Result<Applied, ApplyError> {
    // An edit that needs another's key is made after the others, and only if that one was made.
    let ordered: Vec<&Edit> = edits
        .iter()
        .filter(|e| !needs.contains_key(&e.key))
        .chain(edits.iter().filter(|e| needs.contains_key(&e.key)))
        .collect();
    let mut made: Vec<&str> = Vec::new();
    let mut refused = Vec::new();
    let mut note = |edit: &Edit, error: ApplyError| match error {
        ApplyError::Conflict(reason) => {
            refused.push((edit.key.clone(), reason));
            Ok(())
        }
        unparsable @ ApplyError::Unparsable(_) => Err(unparsable),
    };
    let text = match text {
        Some(t) if !t.trim().is_empty() => {
            parse(t).map_err(ApplyError::Unparsable)?;
            let mut current = t.to_owned();
            for edit in ordered {
                if let Some(wait) = unmet(needs, &made, edit) {
                    note(edit, ApplyError::Conflict(wait))?;
                    continue;
                }
                match apply_one(&current, edit) {
                    Ok(done) => {
                        current = done;
                        made.push(&edit.key);
                    }
                    Err(error) => note(edit, error)?,
                }
            }
            current
        }
        _ => {
            let mut doc = DocumentMut::new();
            for edit in ordered {
                if let Some(wait) = unmet(needs, &made, edit) {
                    note(edit, ApplyError::Conflict(wait))?;
                    continue;
                }
                let mut next = doc.clone();
                match edit_document(&mut next, edit) {
                    Ok(()) => {
                        doc = next;
                        made.push(&edit.key);
                    }
                    Err(error) => note(edit, error)?,
                }
            }
            doc.insert("format", toml_edit::value(FORMAT));
            doc.to_string()
        }
    };
    Ok(Applied { text, refused })
}

/// Why `edit` must wait: the key it needs was not written.
fn unmet(needs: &BTreeMap<String, String>, made: &[&str], edit: &Edit) -> Option<String> {
    let wanted = needs.get(&edit.key)?;
    (!made.contains(&wanted.as_str())).then(|| format!("kept until `{wanted}` can be written"))
}

/// [`apply_each`] for callers that want every edit or an error.
#[cfg(test)]
pub(crate) fn apply(text: Option<&str>, edits: &[Edit]) -> Result<String, ApplyError> {
    let applied = apply_each(text, edits, &BTreeMap::new())?;
    match applied.refused.into_iter().next() {
        Some((key, reason)) => Err(ApplyError::Conflict(format!("`{key}`: {reason}"))),
        None => Ok(applied.text),
    }
}

/// One edit on existing text, made by splicing bytes. An edit that cannot be placed in the text, or that does not read
/// back as the edit says, is refused: the file is never re-serialised to make room for it.
fn apply_one(text: &str, edit: &Edit) -> Result<String, ApplyError> {
    let new = edit
        .value
        .as_ref()
        .map(|(value, _)| json_to_toml(value))
        .transpose()
        .map_err(|e| ApplyError::Conflict(format!("`{}`: {e}", edit.key)))?;
    let version = edit.value.as_ref().map_or(1, |(_, version)| *version);
    let stamp = (version > 1).then(|| toml_edit::Value::from(i64::from(version)));
    let segments: Vec<&str> = edit.key.split('.').collect();
    let done = splice(text, &segments, new.as_ref()).map_err(ApplyError::Conflict)?;
    let done = splice(&done, &[VERSIONS, &edit.key], stamp.as_ref()).map_err(ApplyError::Conflict)?;
    if reads_back(text, &done, edit) {
        Ok(done)
    } else {
        Err(ApplyError::Conflict(format!("`{}` cannot be edited in place in this file", edit.key)))
    }
}

/// Whether `new` is `old` with exactly the edit made: the key and its version read back as asked, and every other key
/// has the value it had (a removed entry must not take a neighbour with it, nor re-attach its prefix to one).
fn reads_back(old: &str, new: &str, edit: &Edit) -> bool {
    let (Ok(before), Ok(after)) = (parse(old), parse(new)) else { return false };
    let found = lookup(&after, &edit.key);
    let target = match &edit.value {
        Some((value, version)) => found.as_ref() == Some(value) && stored_version(&after, &edit.key) == *version,
        None => found.is_none() && stored_version(&after, &edit.key) == 1,
    };
    let segments: Vec<&str> = edit.key.split('.').collect();
    let others = |doc: &DocumentMut| {
        let mut json = item_to_json(doc.as_item());
        remove_path(&mut json, &segments);
        remove_path(&mut json, &[VERSIONS, &edit.key]);
        json
    };
    target && others(&before) == others(&after)
}

/// Removes the value at `path` from a JSON object tree, and the objects the removal leaves empty above it. Says
/// whether `json` itself is now an empty object.
fn remove_path(json: &mut Value, path: &[&str]) -> bool {
    let (Some((first, rest)), Some(object)) = (path.split_first(), json.as_object_mut()) else { return false };
    if rest.is_empty() || object.get_mut(*first).is_some_and(|child| remove_path(child, rest)) {
        object.remove(*first);
    }
    object.is_empty()
}

/// The edit applied to a parsed document: `toml_edit` re-serialises the whole file afterwards.
fn edit_document(doc: &mut DocumentMut, edit: &Edit) -> Result<(), ApplyError> {
    let segments: Vec<&str> = edit.key.split('.').collect();
    if let Some((value, version)) = &edit.value {
        let new = json_to_toml(value).map_err(|e| ApplyError::Conflict(format!("`{}`: {e}", edit.key)))?;
        put(doc.as_table_mut(), &segments, new).map_err(ApplyError::Conflict)?;
        set_version(doc, &edit.key, *version)
    } else {
        remove(doc.as_table_mut(), &segments);
        set_version(doc, &edit.key, 1)
    }
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

/// Records the key's version in `[versions]`, or clears it at version 1. A `versions` that is not a table cannot hold
/// the marker, and a write that dropped it would read back at version 1 and be migrated again, so that is a conflict.
fn set_version(doc: &mut DocumentMut, key: &str, version: u32) -> Result<(), ApplyError> {
    if version > 1 {
        let versions = doc.entry(VERSIONS).or_insert_with(|| Item::Table(Table::new()));
        let table = versions
            .as_table_like_mut()
            .ok_or_else(|| ApplyError::Conflict(format!("`{VERSIONS}` is a value, not a table")))?;
        table.insert(key, toml_edit::value(i64::from(version)));
    } else if let Some(table) = doc.get_mut(VERSIONS).and_then(Item::as_table_like_mut) {
        table.remove(key);
    }
    Ok(())
}

/// Edits the text of `segments` (the value `new`, or its removal) in place. `Err` says why the file's shape cannot take
/// the edit without rewriting more than the edit.
fn splice(text: &str, segments: &[&str], new: Option<&toml_edit::Value>) -> Result<String, String> {
    let doc = Document::parse(text).map_err(|e| e.to_string())?;
    let Some((leaf, prefix)) = segments.split_last() else { return Err("an empty key".to_owned()) };
    let mut table = doc.as_table();
    // `base` is where the dotted keys under the last real table (root or `[header]`) begin in the path.
    let (mut resolved, mut base) = (0, 0);
    for (i, segment) in prefix.iter().enumerate() {
        match table.get(segment) {
            None => break,
            Some(Item::Table(next)) => {
                table = next;
                if !next.is_dotted() {
                    base = i + 1;
                }
            }
            Some(Item::Value(v)) => {
                return match v.as_inline_table() {
                    Some(inline) => splice_inline(text, segments, inline, &prefix[i + 1..], leaf, new),
                    None => not_a_table(text, segment, new),
                };
            }
            Some(_) => return Err(format!("`{segment}` is an array of tables")),
        }
        resolved = i + 1;
    }
    let missing = resolved < prefix.len();
    let Some(new) = new else {
        return match (missing, table.get(leaf)) {
            (true, _) | (false, None) => Ok(text.to_owned()),
            (false, Some(Item::Value(old))) => remove_line(text, table, leaf, old)
                .ok_or_else(|| format!("`{leaf}` shares its line with other text, so it cannot be removed in place")),
            (false, Some(Item::Table(t))) if t.is_dotted() => peel_dotted(text, segments, &first_leaf_of(t), None),
            (false, Some(_)) => Ok(remove_sections(text, &doc, segments)),
        };
    };
    let repr = new.to_string();
    let repr = repr.trim();
    if !missing {
        match table.get(leaf) {
            Some(Item::Value(old)) => return replace_span(text, old, repr),
            Some(Item::Table(t)) if t.is_dotted() => return peel_dotted(text, segments, &first_leaf_of(t), Some(new)),
            Some(_) => {
                // A setting stored as `[a.b]` tables or `[[a.b]]` entries is replaced whole: those sections go, then
                // the new value is written where a new key would go.
                let cleared = remove_sections(text, &doc, segments);
                return if cleared == text {
                    Err(format!("`{leaf}` cannot be replaced in place"))
                } else {
                    splice(&cleared, segments, Some(new))
                };
            }
            None => {}
        }
    }
    if table.is_dotted() {
        let path: Vec<_> = segments[base..].iter().map(|s| Key::new(*s).display_repr().into_owned()).collect();
        return insert_after(text, last_value(table), 0, &format!("{} = {repr}", path.join(".")))
            .ok_or_else(|| "a dotted key without a value".to_owned());
    }
    let line = format!("{} = {repr}", Key::new(*leaf).display_repr());
    if prefix.is_empty() {
        // A key of the top level goes after the last top-level value, or before the first header.
        if let Some(last) = last_value(table) {
            return insert_after(text, Some(last), 0, &line).ok_or_else(|| "no place for the key".to_owned());
        }
        let mut headers = Vec::new();
        collect_headers(doc.as_item(), &mut Vec::new(), &mut headers);
        let at = headers.iter().map(|(_, start)| line_start(text, *start)).min().unwrap_or(text.len());
        let eol = eol_of(text);
        let gap = if at == text.len() && !text.is_empty() && !text.ends_with('\n') { eol } else { "" };
        return Ok(format!("{}{gap}{line}{eol}{}", &text[..at], &text[at..]));
    }
    if !missing && !table.is_implicit() {
        let header = table.span().ok_or_else(|| "a table without a header".to_owned())?;
        insert_after(text, last_value(table), header.end, &line).ok_or_else(|| "no place for the key".to_owned())
    } else {
        let header: Vec<_> = prefix.iter().map(|s| Key::new(*s).display_repr().into_owned()).collect();
        Ok(append_table(text, &header.join("."), &line))
    }
}

/// A setting written as dotted keys (`mig.percent = 125`) is a table of several assignments, each on a line of its
/// own. They are removed one at a time, each by the plain-value rules, until the setting is gone; then the new value, if
/// any, goes where a new key would. `first` is the path from the setting to its first assignment.
fn peel_dotted(
    text: &str,
    segments: &[&str],
    first: &[String],
    new: Option<&toml_edit::Value>,
) -> Result<String, String> {
    let full: Vec<&str> = segments.iter().copied().chain(first.iter().map(String::as_str)).collect();
    let one = splice(text, &full, None)?;
    if one == text {
        return Err(format!("`{}` is written as dotted keys that cannot be removed in place", segments.join(".")));
    }
    splice(&one, segments, new)
}

/// The path, below a dotted table, to the first assignment in it.
fn first_leaf_of(table: &Table) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = table.iter().next();
    while let Some((key, item)) = current {
        path.push(key.to_owned());
        current = match item {
            Item::Table(t) if t.is_dotted() => t.iter().next(),
            _ => None,
        };
    }
    path
}

/// The same for a dotted inline table.
fn first_leaf_of_inline(table: &InlineTable) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = table.iter().next();
    while let Some((key, value)) = current {
        path.push(key.to_owned());
        current = value.as_inline_table().filter(|t| t.is_dotted()).and_then(|t| t.iter().next());
    }
    path
}

/// Deletes every `[header]` / `[[header]]` section at or under `path` (each runs to the next header, less the comment
/// lines that sit directly above that header), so a setting stored as tables can be removed or replaced whole.
fn remove_sections(text: &str, doc: &Document<&str>, path: &[&str]) -> String {
    let mut headers = Vec::new();
    collect_headers(doc.as_item(), &mut Vec::new(), &mut headers);
    headers.sort_by_key(|(_, start)| *start);
    let (mut out, mut cursor) = (String::new(), 0);
    for (i, (header_path, start)) in headers.iter().enumerate() {
        if header_path.len() < path.len() || !header_path.iter().zip(path).all(|(a, b)| a == b) {
            continue;
        }
        let from = line_start(text, *start);
        // The next header on a later line (an implicit parent reports a position inside its first child's header).
        let next = headers[i + 1..]
            .iter()
            .find(|(_, later)| line_start(text, *later) > line_start(text, *start))
            .map_or(text.len(), |(_, later)| comments_above(text, line_start(text, *later)));
        out.push_str(&text[cursor.min(from)..from]);
        cursor = next.max(line_end(text, *start));
    }
    out.push_str(&text[cursor.min(text.len())..]);
    out
}

/// Start of the run of `#` comment lines directly above the line at `pos` (no blank line between), or `pos`.
fn comments_above(text: &str, mut pos: usize) -> usize {
    while pos > 0 {
        let before = line_start(text, pos - 1);
        if !text[before..pos].trim_start().starts_with('#') {
            break;
        }
        pos = before;
    }
    pos
}

/// The path and header offset of every `[table]` and `[[array of tables]]` entry in the document.
fn collect_headers(item: &Item, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, usize)>) {
    let children = |table: &Table, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, usize)>| {
        for (key, child) in table {
            path.push(key.to_owned());
            collect_headers(child, path, out);
            path.pop();
        }
    };
    match item {
        Item::Table(table) if !table.is_dotted() => {
            if let (false, Some(span)) = (path.is_empty(), table.span()) {
                out.push((path.clone(), span.start));
            }
            children(table, path, out);
        }
        Item::ArrayOfTables(array) => {
            for table in array {
                if let Some(span) = table.span() {
                    out.push((path.clone(), span.start));
                }
                children(table, path, out);
            }
        }
        _ => {}
    }
}

fn not_a_table(text: &str, segment: &str, new: Option<&toml_edit::Value>) -> Result<String, String> {
    if new.is_some() { Err(format!("`{segment}` is a value, not a table")) } else { Ok(text.to_owned()) }
}

fn replace_span(text: &str, old: &toml_edit::Value, repr: &str) -> Result<String, String> {
    let span = old.span().ok_or_else(|| "a value without a position".to_owned())?;
    Ok(format!("{}{repr}{}", &text[..span.start], &text[span.end..]))
}

/// [`splice`] inside an inline table `{ ... }`: the value, entry or text of the braces changes, nothing outside them.
fn splice_inline(
    text: &str,
    segments: &[&str],
    inline: &InlineTable,
    prefix: &[&str],
    leaf: &str,
    new: Option<&toml_edit::Value>,
) -> Result<String, String> {
    let (mut table, mut resolved) = (inline, 0);
    for (i, segment) in prefix.iter().enumerate() {
        let Some(value) = table.get(segment) else { break };
        let Some(next) = value.as_inline_table() else { return not_a_table(text, segment, new) };
        table = next;
        resolved = i + 1;
    }
    let missing = resolved < prefix.len();
    let Some(new) = new else {
        return match (missing, table.get_key_value(leaf)) {
            (true, _) | (false, None) => Ok(text.to_owned()),
            (false, Some((key, item))) => match item.as_value().and_then(toml_edit::Value::as_inline_table) {
                Some(t) if t.is_dotted() => peel_dotted(text, segments, &first_leaf_of_inline(t), None),
                _ => remove_entry(text, key, item),
            },
        };
    };
    let repr = new.to_string();
    let repr = repr.trim();
    if !missing && let Some(old) = table.get(leaf) {
        return match old.as_inline_table() {
            Some(t) if t.is_dotted() => peel_dotted(text, segments, &first_leaf_of_inline(t), Some(new)),
            _ => replace_span(text, old, repr),
        };
    }
    if table.is_dotted() {
        return Err("a dotted inline table cannot take a new key in place".to_owned());
    }
    let mut entry = format!("{} = {repr}", Key::new(leaf).display_repr());
    for segment in prefix[resolved..].iter().rev() {
        entry = format!("{} = {{ {entry} }}", Key::new(*segment).display_repr());
    }
    let span = table.span().ok_or_else(|| "an inline table without a position".to_owned())?;
    let last = table.iter().filter_map(|(_, v)| v.span()).map(|s| s.end).max();
    Ok(match last {
        Some(end) => format!("{}, {entry}{}", &text[..end], &text[end..]),
        None => format!("{} {entry} {}", &text[..=span.start], &text[span.start + 1..]),
    })
}

/// Deletes one entry of an inline table with the comma that joins it to its neighbour.
fn remove_entry(text: &str, key: &Key, item: &Item) -> Result<String, String> {
    let (Some(key_span), Some(value_span)) = (key.span(), item.as_value().and_then(toml_edit::Value::span)) else {
        return Err("an inline entry without a position".to_owned());
    };
    // A dotted entry `a.b.c = 1` has the span of its last name only: its entry starts at the first.
    let entry_start = dotted_key_start(text, key_span.start);
    let after = &text[value_span.end..];
    let gap = after.len() - after.trim_start().len();
    let (from, to) = if after[gap..].starts_with(',') {
        let tail = &after[gap + 1..];
        let blanks = tail.len() - tail.trim_start_matches([' ', '\t']).len();
        (entry_start, value_span.end + gap + 1 + blanks)
    } else {
        let before = text[..entry_start].trim_end();
        (if before.ends_with(',') { before.len() - 1 } else { entry_start }, value_span.end)
    };
    Ok(format!("{}{}", &text[..from], &text[to..]))
}

/// Where the dotted key whose last name starts at `pos` begins: the names before it, `a.b.` of `a.b.c`, are walked back.
fn dotted_key_start(text: &str, mut pos: usize) -> usize {
    loop {
        let Some(dotless) = text[..pos].trim_end().strip_suffix('.') else { return pos };
        let name = dotless.trim_end();
        let start = match name.chars().next_back() {
            Some(quote @ ('"' | '\'')) => name[..name.len() - 1].rfind(quote),
            _ => Some(name.trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-').len()),
        };
        match start {
            Some(at) if at < name.len() => pos = at,
            _ => return pos,
        }
    }
}

/// Puts `line` on a new line after the line of the last value `last` (or after the line holding `header_end` when
/// there is none), indented like that value's line.
fn insert_after(text: &str, last: Option<std::ops::Range<usize>>, header_end: usize, line: &str) -> Option<String> {
    if last.is_none() && header_end == 0 {
        return None;
    }
    let after = line_end(text, last.as_ref().map_or(header_end, |v| v.end));
    let indent_from = last.map_or(after, |v| line_start(text, v.start));
    let indent: String = text[indent_from..].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let eol = eol_of(text);
    let mut out = text[..after].to_owned();
    if !out.ends_with('\n') {
        out.push_str(eol);
    }
    out.extend([indent.as_str(), line, eol]);
    out.push_str(&text[after..]);
    Some(out)
}

/// The span of the last-written value of a table, looking through its dotted keys but not its `[sub.tables]`.
fn last_value(table: &Table) -> Option<std::ops::Range<usize>> {
    table
        .iter()
        .filter_map(|(_, item)| match item {
            Item::Value(v) => v.span(),
            Item::Table(t) if t.is_dotted() => last_value(t),
            _ => None,
        })
        .max_by_key(|span| span.end)
}

/// Adds a `[header]` section with one line at the end of the file, after a blank line.
fn append_table(text: &str, header: &str, line: &str) -> String {
    let eol = eol_of(text);
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(eol);
    }
    if !out.is_empty() && !out.ends_with("\n\n") && !out.ends_with("\n\r\n") {
        out.push_str(eol);
    }
    out.extend(["[", header, "]", eol, line, eol]);
    out
}

/// Deletes the line holding `key = old` when it holds nothing else but a comment.
fn remove_line(text: &str, table: &Table, key: &str, old: &toml_edit::Value) -> Option<String> {
    let key_span = table.key(key)?.span()?;
    let value_span = old.span()?;
    let start = line_start(text, key_span.start);
    let end = line_end(text, value_span.end);
    let before = &text[start..key_span.start];
    let after = text[value_span.end..end].trim();
    let alone = !before.contains(['=', '{', ',', '[', '#']) && (after.is_empty() || after.starts_with('#'));
    alone.then(|| format!("{}{}", &text[..start], &text[end..]))
}

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// The offset just past the line that holds `at`, its line ending included.
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i + 1)
}

/// The file's line ending, by its first line.
fn eol_of(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if text[..i].ends_with('\r') => "\r\n",
        _ => "\n",
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
        fn unrelated_keys_and_comments_stay_byte_identical(text in file_text(), edits in edits(), crlf in any::<bool>()) {
            let text = if crlf { text.replace('\n', "\r\n") } else { text };
            let new = apply(Some(&text), &edits).unwrap();
            assert_subsequence(&text, &new)?;
            if crlf {
                prop_assert_eq!(new.matches('\n').count(), new.matches("\r\n").count(), "a bare LF was written:\n{:?}", new);
            }
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
    fn interleaved_dotted_keys_and_crlf_endings_are_not_reordered_or_normalised() {
        let text = "ui.scale = 5\r\nother = 1 # keep\r\nui.theme = \"x\"\r\n\r\n[a]\r\nk = [1,\r\n  2] # z\r\n";
        let edit = |key: &str, value: Option<Value>| Edit { key: key.into(), value: value.map(|v| (v, 1)) };
        let replaced = apply(Some(text), &[edit("ui.theme", Some(json!("y")))]).unwrap();
        assert_eq!(replaced, text.replace("\"x\"", "\"y\""));
        let removed = apply(Some(text), &[edit("ui.scale", None)]).unwrap();
        assert_eq!(removed, text.replace("ui.scale = 5\r\n", ""));
        let added = apply(Some(text), &[edit("a.n", Some(json!(3)))]).unwrap();
        assert_eq!(added, text.replace("2] # z\r\n", "2] # z\r\nn = 3\r\n"));
        let new_table = apply(Some(text), &[edit("b.c", Some(json!(true)))]).unwrap();
        assert_eq!(new_table, format!("{text}\r\n[b]\r\nc = true\r\n"));
    }

    fn set_edit(key: &str, value: Value) -> Edit {
        Edit { key: key.into(), value: Some((value, 1)) }
    }

    fn drop_edit(key: &str) -> Edit {
        Edit { key: key.into(), value: None }
    }

    #[test]
    fn a_key_added_to_a_dotted_table_goes_after_its_last_dotted_key_and_nothing_else_moves() {
        for eol in ["\n", "\r\n"] {
            let text = "ui.scale = 5\nother = 1\nui.theme = \"x\"\n\n[a]\nk = 1\n".replace('\n', eol);
            let new = apply(Some(&text), &[set_edit("ui.tier", json!("hi"))]).unwrap();
            let expected =
                "ui.scale = 5\nother = 1\nui.theme = \"x\"\nui.tier = \"hi\"\n\n[a]\nk = 1\n".replace('\n', eol);
            assert_eq!(new, expected);
        }
    }

    #[test]
    fn edits_inside_an_inline_table_change_only_the_braces() {
        let text = "a = 1\r\naudio = { bindings = { voice = \"x\" }, gain = 3 } # keep\r\nz = 2\r\n";
        let run = |edit| apply(Some(text), &[edit]).unwrap();
        assert_eq!(run(set_edit("audio.bindings.voice", json!("y"))), text.replace("\"x\"", "\"y\""));
        assert_eq!(run(set_edit("audio.bindings.other", json!(1))), text.replace("\"x\" }", "\"x\", other = 1 }"));
        assert_eq!(
            run(set_edit("audio.extra.k", json!(true))),
            text.replace("gain = 3", "gain = 3, extra = { k = true }")
        );
        assert_eq!(run(drop_edit("audio.gain")), text.replace(", gain = 3", ""));
        assert_eq!(run(drop_edit("audio.bindings")), text.replace("bindings = { voice = \"x\" }, ", ""));
        let empty = apply(Some("audio = {}\n"), &[set_edit("audio.k", json!(1))]).unwrap();
        assert_eq!(empty, "audio = { k = 1 }\n");
    }

    #[test]
    fn a_setting_stored_as_tables_is_replaced_or_removed_whole_with_the_rest_untouched() {
        for eol in ["\n", "\r\n"] {
            let text = "# top\n[audio]\ngain = 1\n\n# the bindings\n[audio.bindings] # b\nvoice = \"x\"\n\n\
                        [audio.bindings.deep]\nk = 1\n\n# next section\n[other]\nz = 2\n"
                .replace('\n', eol);
            let replaced = apply(Some(&text), &[set_edit("audio.bindings", json!({ "voice": "y" }))]).unwrap();
            let expected = "# top\n[audio]\ngain = 1\nbindings = { voice = \"y\" }\n\n# the bindings\n# next section\n[other]\nz = 2\n"
                .replace('\n', eol);
            assert_eq!(replaced, expected, "{eol:?}");
            let removed = apply(Some(&text), &[drop_edit("audio.bindings")]).unwrap();
            let expected =
                "# top\n[audio]\ngain = 1\n\n# the bindings\n# next section\n[other]\nz = 2\n".replace('\n', eol);
            assert_eq!(removed, expected, "{eol:?}");
        }
    }

    #[test]
    fn a_setting_stored_as_an_array_of_tables_is_replaced_or_removed_whole() {
        let text = "a = 1\n[[list]]\nx = 1\n[[list]]\nx = 2\n[list.sub]\ny = 1\n[keep]\nk = 1\n";
        let replaced = apply(Some(text), &[set_edit("list", json!([{ "x": 9 }]))]).unwrap();
        assert_eq!(replaced, "a = 1\nlist = [{ x = 9 }]\n[keep]\nk = 1\n");
        let removed = apply(Some(text), &[drop_edit("list")]).unwrap();
        assert_eq!(removed, "a = 1\n[keep]\nk = 1\n");
        let sub = apply(Some(text), &[set_edit("list.x", json!(1))]);
        assert!(matches!(sub, Err(ApplyError::Conflict(_))), "a key under an array of tables is refused: {sub:?}");
    }

    #[test]
    fn removing_a_dotted_inline_entry_takes_its_whole_key_and_leaves_its_neighbours_alone() {
        let run = |text: &str, key: &str| apply(Some(text), &[drop_edit(key)]).unwrap();
        let text = "audio = { bindings.voice = \"x\", gain = 3 }\n";
        assert_eq!(run(text, "audio.bindings.voice"), "audio = { gain = 3 }\n");
        assert_eq!(run(text, "audio.gain"), "audio = { bindings.voice = \"x\" }\n");
        let nested = "a = { x.y.z = 1, x.y.w = 2, \"q r\".s = 3, e = 4 }\r\n";
        assert_eq!(run(nested, "a.x.y.z"), "a = { x.y.w = 2, \"q r\".s = 3, e = 4 }\r\n");
        assert_eq!(run(nested, "a.x.y.w"), "a = { x.y.z = 1, \"q r\".s = 3, e = 4 }\r\n");
        assert_eq!(run(nested, "a.q r.s"), "a = { x.y.z = 1, x.y.w = 2, e = 4 }\r\n");
        assert_eq!(run(nested, "a.e"), "a = { x.y.z = 1, x.y.w = 2, \"q r\".s = 3 }\r\n");
    }

    #[test]
    fn a_setting_written_as_dotted_keys_is_replaced_or_removed_whole_with_the_rest_untouched() {
        for eol in ["\n", "\r\n"] {
            let under_header =
                "# top\n[ui]\nmig.percent = 125 # n\nmig.unit = \"x\"\nkeep = 1\n\n[other]\nk = 1\n".replace('\n', eol);
            let changed = apply(Some(&under_header), &[set_edit("ui.mig", json!(150))]).unwrap();
            let expected = "# top\n[ui]\nkeep = 1\nmig = 150\n\n[other]\nk = 1\n".replace('\n', eol);
            assert_eq!(changed, expected, "{eol:?}");
            let cleared = apply(Some(&under_header), &[drop_edit("ui.mig")]).unwrap();
            assert_eq!(cleared, "# top\n[ui]\nkeep = 1\n\n[other]\nk = 1\n".replace('\n', eol), "{eol:?}");

            let at_root = "a = 1\nui.mig.percent = 125\nz = 2\n".replace('\n', eol);
            let changed = apply(Some(&at_root), &[set_edit("ui.mig", json!(150))]).unwrap();
            assert_eq!(changed, "a = 1\nz = 2\n\n[ui]\nmig = 150\n".replace('\n', eol), "{eol:?}");

            let inline = "ui = { mig.percent = 125, mig.unit = 1, k = 2 } # c\n".replace('\n', eol);
            let changed = apply(Some(&inline), &[set_edit("ui.mig", json!(150))]).unwrap();
            assert_eq!(changed, "ui = { k = 2, mig = 150 } # c\n".replace('\n', eol), "{eol:?}");
            let cleared = apply(Some(&inline), &[drop_edit("ui.mig")]).unwrap();
            assert_eq!(cleared, "ui = { k = 2 } # c\n".replace('\n', eol), "{eol:?}");
        }
    }

    /// Every way the loader accepts a table-valued setting `ui.mig = { percent = 125 }`.
    const SHAPES: [&str; 8] = [
        "[ui]\nmig = { percent = 125 }\n",
        "[ui]\nmig.percent = 125\n",
        "[ui.mig]\npercent = 125\n",
        "ui.mig.percent = 125\n",
        "ui = { mig = { percent = 125 } }\n",
        "ui = { mig.percent = 125 }\n",
        "[ui]\nmig = { percent = 125 }\n[ui.sub]\nz = 1\n",
        "[ui]\nmig.percent = 125\nmig.unit = 1\n[ui.sub]\nz = 1\n",
    ];

    proptest! {
        #[test]
        fn every_shape_the_loader_accepts_can_be_changed_reset_and_reopened(
            shape in 0..SHAPES.len(),
            crlf in proptest::bool::ANY,
            comments in proptest::bool::ANY,
            object in proptest::bool::ANY,
        ) {
            let lead = if comments { "u0 = 1\n# c1\n" } else { "u0 = 1\n" };
            let trail = if comments { "# c2\n[uother]\nk = 1\n" } else { "[uother]\nk = 1\n" };
            let mut text = format!("{lead}{}{trail}", SHAPES[shape]);
            if crlf {
                text = text.replace('\n', "\r\n");
            }
            // The lines that are not the setting's own (or a header it may share) must all survive, in order.
            let own = |l: &str| l.is_empty() || ["mig", "percent", "unit", "[ui", "ui =", "z = 1"].iter().any(|m| l.contains(m));
            let unrelated: Vec<&str> = text.lines().filter(|l| !own(l)).collect();
            let kept = |new: &str| {
                let mut rest = new.lines();
                unrelated.iter().all(|l| rest.any(|n| n == *l))
            };
            let value = if object { json!({ "percent": 7, "x": 1 }) } else { json!(150) };
            let changed = apply(Some(&text), &[set_edit("ui.mig", value.clone())]).unwrap();
            prop_assert_eq!(lookup(&parse(&changed).unwrap(), "ui.mig"), Some(value));
            prop_assert!(kept(&changed), "lost unrelated lines\n{:?}\n{:?}", text, changed);
            if crlf {
                prop_assert_eq!(changed.matches('\n').count(), changed.matches("\r\n").count());
            }
            let cleared = apply(Some(&text), &[drop_edit("ui.mig")]).unwrap();
            prop_assert_eq!(lookup(&parse(&cleared).unwrap(), "ui.mig"), None);
            prop_assert!(kept(&cleared), "lost unrelated lines\n{:?}\n{:?}", text, cleared);
            let strip = |t: &str| {
                let mut json = item_to_json(parse(t).unwrap().as_item());
                remove_path(&mut json, &["ui", "mig"]);
                json
            };
            prop_assert_eq!(strip(&text), strip(&changed));
            prop_assert_eq!(strip(&text), strip(&cleared));
            prop_assert!(apply(Some(&changed), &[set_edit("ui.mig", json!(150))]).is_ok());
        }

        #[test]
        fn a_refused_edit_leaves_the_file_and_the_other_edits_alone(text in layouts()) {
            if text.trim().is_empty() || !text.contains("name = 'v'") {
                return Ok(());
            }
            // `name` is a plain value, so nothing can go under it.
            let good = set_edit("ui.tier", json!("hi"));
            let bad = set_edit("name.x", json!(1));
            let both = apply_each(Some(&text), &[good.clone(), bad], &BTreeMap::new()).unwrap();
            let only = apply_each(Some(&text), &[good], &BTreeMap::new()).unwrap();
            prop_assert_eq!(both.text, only.text);
            prop_assert_eq!(both.refused.len(), 1);
        }
    }

    #[test]
    fn an_edit_that_needs_a_refused_edit_waits_and_changes_nothing() {
        let text = "area = 0\n[legacy]\nvalue = 7\n";
        let edits = [set_edit("area.new", json!(7)), drop_edit("legacy.value")];
        let needs = BTreeMap::from([("legacy.value".to_owned(), "area.new".to_owned())]);
        let done = apply_each(Some(text), &edits, &needs).unwrap();
        assert_eq!(done.text, text, "the destination was refused, so the source stays");
        assert_eq!(done.refused.len(), 2);
        let ok = apply_each(Some("[legacy]\nvalue = 7\n"), &edits, &needs).unwrap();
        assert!(ok.refused.is_empty() && ok.text.contains("new = 7") && !ok.text.contains("value"), "{}", ok.text);
    }

    #[test]
    fn a_shape_that_cannot_be_edited_in_place_is_refused_not_rewritten() {
        let text = "a = { b.c = 1 }\n";
        let result = apply(Some(text), &[set_edit("a.d", json!(1))]);
        assert!(matches!(result, Err(ApplyError::Conflict(_))), "{result:?}");
        let result = apply(Some("[[arr]]\nx = 1\n"), &[set_edit("arr.y", json!(1))]);
        assert!(matches!(result, Err(ApplyError::Conflict(_))), "{result:?}");
    }

    #[test]
    fn comments_and_blank_lines_between_entries_stay_where_they_are() {
        let text = "# top\n\n[ui]\n  # a\n\n  scale   =   5   # t\n\n# z\n\n[o]\nx=1\n";
        let replaced = apply(Some(text), &[set_edit("ui.scale", json!(6))]).unwrap();
        assert_eq!(replaced, text.replace("5   # t", "6   # t"));
        let added = apply(Some(text), &[set_edit("ui.theme", json!("d"))]).unwrap();
        assert_eq!(added, text.replace("5   # t\n", "5   # t\n  theme = \"d\"\n"));
        let removed = apply(Some(text), &[drop_edit("ui.scale")]).unwrap();
        assert_eq!(removed, text.replace("  scale   =   5   # t\n", ""));
    }

    /// Chunks of a file's top level, in any order: dotted keys of one table apart from each other, inline tables,
    /// a multi-line array, comments and blank lines.
    const CHUNKS: [&str; 10] = [
        "ui.scale = 5 # c\n",
        "other.k = 1\n",
        "ui.theme = \"x\"\n",
        "audio = { a = 1, b = { c = 2 } }\n",
        "# note\n",
        "\n",
        "name = 'v'\n",
        "arr = [1,\n  2]\n",
        "  # indented note\n",
        "dn = { bindings.voice = \"v\", gain = 3, bindings.q.r = 1 }\n",
    ];

    fn layouts() -> impl Strategy<Value = String> {
        (Just(CHUNKS.to_vec()).prop_shuffle(), 0..=CHUNKS.len(), proptest::bool::ANY, proptest::bool::ANY).prop_map(
            |(chunks, keep, section, crlf)| {
                let mut text: String = chunks.into_iter().take(keep).collect();
                if section {
                    text.push_str("\n[quality]\ny = 1 # why\n");
                }
                if crlf { text.replace('\n', "\r\n") } else { text }
            },
        )
    }

    /// Whether `new` is `old` with one run of text inserted (`grew`) or deleted (not `grew`), and nothing else changed.
    fn is_one_run(old: &str, new: &str, grew: bool) -> bool {
        let (short, long) = if grew { (old, new) } else { (new, old) };
        let prefix = short.bytes().zip(long.bytes()).take_while(|(a, b)| a == b).count();
        let suffix = short.bytes().rev().zip(long.bytes().rev()).take_while(|(a, b)| a == b).count();
        prefix + suffix >= short.len()
    }

    proptest! {
        #[test]
        fn every_byte_outside_the_edit_survives_in_any_layout(text in layouts(), pick in 0usize..9) {
            let (grow, edit) = match pick {
                0 => (true, set_edit("ui.tier", json!("hi"))),
                1 => (true, set_edit("audio.d", json!(4))),
                2 => (true, set_edit("audio.b.e", json!(true))),
                3 => (true, set_edit("fresh.k", json!(1))),
                4 => (false, drop_edit("ui.scale")),
                5 => (false, drop_edit("audio.a")),
                6 => (false, drop_edit("dn.bindings.voice")),
                7 => (false, drop_edit("dn.gain")),
                _ => (false, drop_edit("dn.bindings.q.r")),
            };
            // A file of nothing but blank lines counts as a new file, which is built whole.
            let had = lookup(&parse(&text).unwrap(), &edit.key).is_some();
            if text.trim().is_empty() || had == grow {
                return Ok(());
            }
            // `ui.tier` needs a `ui` table to land in as a dotted key, or makes a new one: both are fine.
            let new = apply(Some(&text), std::slice::from_ref(&edit)).unwrap();
            prop_assert!(is_one_run(&text, &new, grow), "more than the edit changed\n--- old\n{:?}\n--- new\n{:?}", text, new);
            if text.contains("\r\n") {
                prop_assert_eq!(new.matches('\n').count(), new.matches("\r\n").count());
            }
            prop_assert!(reads_back(&text, &new, &edit));
            // Independently of `reads_back`: no other key changed its meaning.
            let strip = |t: &str| {
                let mut json = item_to_json(parse(t).unwrap().as_item());
                remove_path(&mut json, &edit.key.split('.').collect::<Vec<_>>());
                json
            };
            prop_assert_eq!(strip(&text), strip(&new));
        }

        #[test]
        fn a_replaced_value_changes_only_its_own_bytes(text in layouts(), flip in proptest::bool::ANY) {
            let (key, old, new) = if flip { ("ui.theme", "\"x\"", "\"yy\"") } else { ("audio.b.c", "c = 2", "c = 99") };
            if !text.contains(old) {
                return Ok(());
            }
            let value = if flip { json!("yy") } else { json!(99) };
            let done = apply(Some(&text), &[set_edit(key, value)]).unwrap();
            prop_assert_eq!(done, text.replacen(old, new, 1));
        }
    }

    #[test]
    fn a_versions_item_that_is_not_a_table_is_a_conflict_not_a_silent_skip() {
        let stamped = Edit { key: "ui.mig".into(), value: Some((json!(1), 3)) };
        for text in ["versions = 7\n", "versions = 7\n[ui]\nmig = 1\n"] {
            assert!(
                matches!(apply(Some(text), std::slice::from_ref(&stamped)), Err(ApplyError::Conflict(_))),
                "{text}"
            );
        }
        let plain = Edit { key: "ui.scale".into(), value: Some((json!(1), 1)) };
        assert!(apply(Some("versions = 7\n"), &[plain]).is_ok(), "a version-1 write needs no marker");
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
