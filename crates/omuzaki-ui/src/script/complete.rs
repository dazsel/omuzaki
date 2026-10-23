//! The console's completions (#1097, `scripting.md` section 8), generated from the registry and the op schemas (P8):
//! after `ops::` the areas; after `ops::<area>::` each op with its summary and scope; inside the argument map of a call
//! (`ops::face::set_profile(#{ ... })`) the op's fields, required ones first, with type, range, enum values and doc;
//! after `get("` the document paths the caller offers (the selection's). No op name is written here: everything comes
//! from the [`OpInfo`] list.
//!
//! Pure: the editor passes the source, the cursor (in characters) and the ops, and draws what comes back, so this is
//! tested without a window.

use std::collections::BTreeMap;

use omuzaki_ops_api::{OpInfo, Scope};
use serde_json::Value;

/// What a completion is, for the line the list shows after its label.
#[derive(Clone, Debug, PartialEq)]
pub enum Detail {
    /// An area of ops, with how many it holds.
    Area {
        /// Its ops.
        ops: usize,
    },
    /// An op.
    Op {
        /// What it needs.
        scope: Scope,
        /// What it does.
        summary: &'static str,
    },
    /// A field of an op's argument map.
    Field(Field),
    /// A document path.
    Path,
}

/// One field of an op's arguments, as its schema states it.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// Its kind.
    pub kind: FieldKind,
    /// Whether the op needs it.
    pub required: bool,
    /// The smallest number it takes.
    pub min: Option<f64>,
    /// The largest number it takes.
    pub max: Option<f64>,
    /// The values it takes, when they are a closed list.
    pub options: Vec<String>,
    /// Its doc.
    pub doc: Option<String>,
}

/// A field's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// A number.
    Number,
    /// A whole number.
    Integer,
    /// Text.
    Text,
    /// True or false.
    Bool,
    /// A list.
    List,
    /// A map.
    Map,
    /// Anything, or a kind the schema doesn't say.
    Any,
}

/// One entry of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    /// What the list shows.
    pub label: String,
    /// What replaces the word being typed.
    pub insert: String,
    /// What it is.
    pub detail: Detail,
}

/// The completions at a cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct Completions {
    /// The character the word being typed starts at: accepting replaces from here to the cursor.
    pub start: usize,
    /// The entries, in the order shown.
    pub items: Vec<Completion>,
}

/// An op as a script calls it.
#[derive(Clone, Debug)]
pub struct Callable {
    /// The area as a script spells it.
    pub area: String,
    /// The verb as a script spells it.
    pub verb: String,
    /// The op.
    pub info: OpInfo,
}

/// The ops a script can name, spelled as a script spells them; `spell` is
/// [`omuzaki_script::script_call`] (passed in so a test can stand in for it).
pub fn callables(ops: &[OpInfo], spell: impl Fn(&str) -> Option<String>) -> Vec<Callable> {
    let mut out: Vec<Callable> = ops
        .iter()
        .filter_map(|info| {
            let call = spell(info.name)?;
            let mut parts = call.splitn(3, "::").skip(1);
            let (area, verb) = (parts.next()?.to_owned(), parts.next()?.to_owned());
            Some(Callable { area, verb, info: info.clone() })
        })
        .collect();
    out.sort_by(|a, b| (&a.area, &a.verb).cmp(&(&b.area, &b.verb)));
    out
}

/// The completions for the cursor at character `cursor` of `source`, or `None` where nothing completes.
pub fn complete(ops: &[Callable], source: &str, cursor: usize, paths: &[String]) -> Option<Completions> {
    let before: String = source.chars().take(cursor).collect();
    if let Some((start, typed)) = in_get_path(&before) {
        let items: Vec<Completion> = paths
            .iter()
            .filter(|p| p.starts_with(typed))
            .map(|p| Completion { label: p.clone(), insert: p.clone(), detail: Detail::Path })
            .collect();
        return (!items.is_empty()).then_some(Completions { start, items });
    }
    let word: String =
        before.chars().rev().take_while(|c| is_ident(*c)).collect::<Vec<_>>().into_iter().rev().collect();
    let start = cursor - word.chars().count();
    let head = &before[..before.len() - word.len()];
    let items = if let Some(area) = head.strip_suffix("::").and_then(area_after_namespace) {
        verbs(ops, area, &word)
    } else if head.strip_suffix(NAMESPACE).is_some_and(name_ends_here) {
        areas(ops, &word)
    } else if let Some((op, written)) = in_argument_map(ops, head) {
        fields(op, &written, &word)
    } else {
        return None;
    };
    (!items.is_empty()).then_some(Completions { start, items })
}

/// Whether `c` can be part of a name.
fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The namespace every op call starts with.
const NAMESPACE: &str = "ops::";

/// Whether a name ends where `text` ends: `ops` right after it would be the namespace, not the tail of a longer name.
fn name_ends_here(text: &str) -> bool {
    text.chars().next_back().is_none_or(|c| !is_ident(c) && c != ':')
}

/// The area when `text` ends with `ops::<area>`.
fn area_after_namespace(text: &str) -> Option<&str> {
    let area_len = text.chars().rev().take_while(|c| is_ident(*c)).map(char::len_utf8).sum::<usize>();
    let (rest, area) = text.split_at(text.len() - area_len);
    let rest = rest.strip_suffix(NAMESPACE)?;
    (name_ends_here(rest) && !area.is_empty()).then_some(area)
}

/// The areas starting with `typed`.
fn areas(ops: &[Callable], typed: &str) -> Vec<Completion> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for op in ops.iter().filter(|op| op.area.starts_with(typed)) {
        *counts.entry(op.area.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(area, n)| Completion {
            label: area.to_owned(),
            insert: format!("{area}::"),
            detail: Detail::Area { ops: n },
        })
        .collect()
}

/// The ops of `area` whose verb starts with `typed`.
fn verbs(ops: &[Callable], area: &str, typed: &str) -> Vec<Completion> {
    ops.iter()
        .filter(|op| op.area == area && op.verb.starts_with(typed))
        .map(|op| Completion {
            label: op.verb.clone(),
            insert: format!("{}(#{{ ", op.verb),
            detail: Detail::Op { scope: op.info.scope, summary: op.info.summary },
        })
        .collect()
}

/// The fields of `op` starting with `typed` that the map doesn't hold yet: required first, each group by name.
fn fields(op: &Callable, written: &[String], typed: &str) -> Vec<Completion> {
    let mut all = schema_fields(&op.info);
    all.retain(|(name, _)| name.starts_with(typed) && !written.iter().any(|w| w == name));
    all.sort_by(|a, b| (!a.1.required, &a.0).cmp(&(!b.1.required, &b.0)));
    all.into_iter()
        .map(|(name, field)| Completion { insert: format!("{name}: "), label: name, detail: Detail::Field(field) })
        .collect()
}

/// The fields `info`'s schema states, with `$ref`s to its definitions followed.
pub fn schema_fields(info: &OpInfo) -> Vec<(String, Field)> {
    let root = info.schema.as_value();
    let Some(properties) = root.get("properties").and_then(Value::as_object) else { return Vec::new() };
    let required: Vec<&str> = root
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    properties
        .iter()
        .map(|(name, node)| {
            let node = resolved(root, node);
            let kind = kind_of(root, node);
            let number = |key: &str| node.get(key).and_then(Value::as_f64);
            let options = node
                .get("enum")
                .and_then(Value::as_array)
                .map(|values| values.iter().map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)).collect())
                .unwrap_or_default();
            let doc = node.get("description").and_then(Value::as_str).map(str::to_owned);
            let field = Field {
                kind,
                required: required.contains(&name.as_str()),
                min: number("minimum"),
                max: number("maximum"),
                options,
                doc,
            };
            (name.clone(), field)
        })
        .collect()
}

/// `node`, or what its `$ref` (or a one-element `allOf` or `anyOf` with a null) points to in `root`.
fn resolved<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    let mut node = node;
    for _ in 0..8 {
        if let Some(target) = node.get("$ref").and_then(Value::as_str) {
            let pointer = target.trim_start_matches('#');
            match root.pointer(pointer) {
                Some(found) => node = found,
                None => break,
            }
        } else if let Some([only]) = node.get("allOf").and_then(Value::as_array).map(Vec::as_slice) {
            node = only;
        } else if let Some(options) = node.get("anyOf").and_then(Value::as_array) {
            let real: Vec<&Value> = options.iter().filter(|o| o.get("type") != Some(&Value::from("null"))).collect();
            match real.as_slice() {
                [only] => node = only,
                _ => break,
            }
        } else {
            break;
        }
    }
    node
}

/// What kind of value `node` takes.
fn kind_of(root: &Value, node: &Value) -> FieldKind {
    if node.get("enum").is_some() || node.get("const").is_some() {
        return FieldKind::Text;
    }
    let node = resolved(root, node);
    let named = match node.get("type") {
        Some(Value::String(t)) => Some(t.as_str()),
        Some(Value::Array(types)) => types.iter().filter_map(Value::as_str).find(|t| *t != "null"),
        _ => None,
    };
    match named {
        Some("number") => FieldKind::Number,
        Some("integer") => FieldKind::Integer,
        Some("string") => FieldKind::Text,
        Some("boolean") => FieldKind::Bool,
        Some("array") => FieldKind::List,
        Some("object") => FieldKind::Map,
        _ => FieldKind::Any,
    }
}

/// The op whose argument map the text before the cursor is inside, at a place a key goes, with the keys already written
/// at the map's top level.
fn in_argument_map<'a>(ops: &'a [Callable], head: &str) -> Option<(&'a Callable, Vec<String>)> {
    let open = unclosed_map(head)?;
    // A key goes right after `#{` or a `,`.
    let gap = head[open + 2..].trim_end();
    if !(gap.is_empty() || gap.ends_with(',')) {
        return None;
    }
    let call = head[..open].trim_end().strip_suffix('(')?.trim_end();
    let verb_len = call.chars().rev().take_while(|c| is_ident(*c)).map(char::len_utf8).sum::<usize>();
    let (rest, verb) = call.split_at(call.len() - verb_len);
    let area = area_after_namespace(rest.strip_suffix("::")?)?;
    let op = ops.iter().find(|op| op.area == area && op.verb == verb)?;
    Some((op, written_keys(&head[open + 2..])))
}

/// The byte index of the `#{` the end of `head` is inside, skipping strings, comments and closed braces.
fn unclosed_map(head: &str) -> Option<usize> {
    let bytes = head.as_bytes();
    let mut stack: Vec<(usize, bool)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'`' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'#' if bytes.get(i + 1) == Some(&b'{') => {
                stack.push((i, true));
                i += 1;
            }
            b'{' => stack.push((i, false)),
            b'}' => {
                stack.pop();
            }
            _ => {}
        }
        i += 1;
    }
    match stack.last() {
        Some((at, true)) => Some(*at),
        _ => None,
    }
}

/// The keys written at the top level of a map's text so far.
fn written_keys(body: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut depth = 0usize;
    let mut expect_key = true;
    let mut word = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '`' => {
                while let Some(n) = chars.next() {
                    if n == '\\' {
                        chars.next();
                    } else if n == c {
                        break;
                    }
                }
            }
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => expect_key = true,
            ':' if depth == 0 && expect_key => {
                if !word.is_empty() {
                    keys.push(std::mem::take(&mut word));
                }
                expect_key = false;
            }
            c if is_ident(c) && depth == 0 && expect_key => word.push(c),
            c if c.is_whitespace() => {}
            _ => word.clear(),
        }
    }
    keys
}

/// The start and the typed part of a document path when the cursor is inside `get("…`.
fn in_get_path(before: &str) -> Option<(usize, &str)> {
    let at = before.rfind("get(\"")?;
    let typed = &before[at + 5..];
    if typed.contains('"') || typed.contains('\n') {
        return None;
    }
    Some((before[..at + 5].chars().count(), typed))
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{OpKind, OpMeta, Undo};
    use serde_json::json;

    use super::*;

    fn info(name: &'static str, scope: Scope, schema: Value) -> OpInfo {
        let meta = OpMeta { name, summary: "Does a thing.", kind: OpKind::Edit, scope, undo: Undo::Auto };
        OpInfo::new(meta, schemars::Schema::try_from(schema).unwrap_or_else(|e| panic!("{e}")))
    }

    fn ops() -> Vec<Callable> {
        let profile = json!({
            "type": "object",
            "properties": {
                "character": { "type": "string", "description": "The character." },
                "blend": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "profile": { "$ref": "#/$defs/Profile" },
                "note": { "type": ["string", "null"] }
            },
            "required": ["profile", "character"],
            "$defs": { "Profile": { "type": "string", "enum": ["egg-oval", "round"] } }
        });
        let ops = vec![
            info("face.set_profile", Scope::EditCharacter, profile),
            info("face.set_blush", Scope::EditCharacter, json!({ "type": "object" })),
            info("char.rename", Scope::EditCharacter, json!({ "type": "object" })),
            info("render.still", Scope::Render, json!({ "type": "object" })),
        ];
        callables(&ops, |name| name.split_once('.').map(|(a, v)| format!("ops::{a}::{v}")))
    }

    fn labels(c: Option<&Completions>) -> Vec<&str> {
        c.map(|c| c.items.iter().map(|i| i.label.as_str()).collect()).unwrap_or_default()
    }

    fn end_labels(source: &str) -> Vec<String> {
        labels(at_end(source).as_ref()).into_iter().map(str::to_owned).collect()
    }

    fn at_end(source: &str) -> Option<Completions> {
        complete(&ops(), source, source.chars().count(), &[])
    }

    #[test]
    fn after_ops_come_the_areas_and_after_an_area_its_ops() {
        assert_eq!(end_labels("let x = ops::"), ["char", "face", "render"]);
        assert_eq!(end_labels("ops::f"), ["face"]);
        let c = at_end("ops::face::");
        assert_eq!(labels(c.as_ref()), ["set_blush", "set_profile"]);
        let item = &c.unwrap_or_else(|| panic!("ops")).items[1];
        assert_eq!(item.insert, "set_profile(#{ ");
        assert_eq!(item.detail, Detail::Op { scope: Scope::EditCharacter, summary: "Does a thing." });
        assert_eq!(end_labels("ops::face::set_p"), ["set_profile"]);
        assert_eq!(at_end("ops::face::set_p").map(|c| c.start), Some(11));
        assert!(at_end("myops::").is_none(), "`ops` must be the namespace, not the end of a name");
    }

    #[test]
    fn inside_a_call_map_come_its_fields_required_first() {
        let c = at_end("ops::face::set_profile(#{ ");
        assert_eq!(labels(c.as_ref()), ["character", "profile", "blend", "note"]);
        let items = c.unwrap_or_else(|| panic!("fields")).items;
        assert_eq!(items[0].insert, "character: ");
        let Detail::Field(profile) = &items[1].detail else { panic!("a field") };
        assert!(profile.required);
        assert_eq!(profile.options, ["egg-oval", "round"], "the enum, through the $ref");
        let Detail::Field(blend) = &items[2].detail else { panic!("a field") };
        assert_eq!((blend.kind, blend.min, blend.max), (FieldKind::Number, Some(0.0), Some(1.0)));
        let Detail::Field(note) = &items[3].detail else { panic!("a field") };
        assert_eq!(note.kind, FieldKind::Text, "a nullable text is text");

        // The keys already written are left out, nested maps and strings don't count, and a value place is not a key.
        let c = at_end("ops::face::set_profile(#{ character: \"a, b: c\", blend: #{ profile: 1 }, ");
        assert_eq!(labels(c.as_ref()), ["profile", "note"]);
        assert!(at_end("ops::face::set_profile(#{ character: ").is_none());
        assert_eq!(end_labels("ops::face::set_profile(#{ b"), ["blend"]);
        assert!(at_end("ops::face::set_profile(#{ blend: 1 }); #{ ").is_none(), "a closed call's map is over");
    }

    #[test]
    fn inside_get_come_the_paths_offered() {
        let paths = ["characters/aki".to_owned(), "characters/aki/face".to_owned(), "scene".to_owned()];
        let source = "let f = get(\"characters/a";
        let c = complete(&ops(), source, source.chars().count(), &paths);
        assert_eq!(labels(c.as_ref()), ["characters/aki", "characters/aki/face"]);
        assert_eq!(c.map(|c| c.start), Some(13));
        let closed = "get(\"scene\") + ";
        assert!(complete(&ops(), closed, closed.chars().count(), &paths).is_none());
    }
}
