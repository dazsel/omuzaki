//! Extraction: the English Fluent file of a package, generated from the text its registries and `messages!` tables
//! submit (design `docs/design/l10n-ops.md` §3.1, §3.3 and §11).
//!
//! The output is deterministic. Entries are sorted by group and id, so the same text in any input order gives the same
//! bytes, and a blessed file never churns.

use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::ids;
use crate::registry::{ArgKind, MessageDecl, MessageTable, RegisteredText, TextSource};
use crate::schema_text::{self, walk};

/// Everything one package contributes to its English file.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct PackageText {
    /// The package whose file this is (`CARGO_PKG_NAME`).
    pub package: String,
    /// The text its registries contributed.
    pub registered: Vec<RegisteredText>,
    /// The messages its `messages!` tables declared.
    pub messages: Vec<MessageDecl>,
}

impl PackageText {
    /// An empty contribution for `package`.
    pub fn new(package: impl Into<String>) -> Self {
        Self { package: package.into(), ..Self::default() }
    }

    /// Sets the registered text, so the checks can run on text they planted.
    #[must_use]
    pub fn with_registered(mut self, text: Vec<RegisteredText>) -> Self {
        self.registered = text;
        self
    }

    /// Sets the messages, so the checks can run on messages they planted.
    #[must_use]
    pub fn with_messages(mut self, messages: &[MessageDecl]) -> Self {
        self.messages = messages.to_vec();
        self
    }

    /// What this binary links for `package`: every `TextSource`'s text for it, and every `MessageTable` it submitted.
    pub fn registered(package: &str) -> Self {
        let registered = inventory::iter::<TextSource>.into_iter().flat_map(|source| (source.text)(package)).collect();
        let messages = inventory::iter::<MessageTable>
            .into_iter()
            .filter(|table| table.package == package)
            .flat_map(|table| table.messages.iter().copied())
            .collect();
        Self { package: package.to_owned(), registered, messages }
    }
}

/// `package`'s generated English file, from what is linked into this binary.
pub fn english(package: &str) -> String {
    english_of(&PackageText::registered(package))
}

/// The English file of `text`. The same text, in any input order, gives the same bytes.
///
/// A package with no text gives `""`, so a crate with nothing to say keeps an empty file.
pub fn english_of(text: &PackageText) -> String {
    let mut entries = Vec::new();
    for item in &text.registered {
        owner_entries(item, &mut entries);
    }
    for decl in &text.messages {
        entries.push(message_entry(decl));
    }
    if entries.is_empty() {
        return String::new();
    }
    entries.sort();

    let mut blocks = vec![header(&text.package)];
    let mut current = None;
    for entry in entries {
        if current != Some(entry.group) {
            blocks.push(entry.group.heading().to_owned());
            current = Some(entry.group);
        }
        blocks.push(entry.text);
    }
    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

/// The groups of the file, in the order they are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Ops,
    Kinds,
    Registered,
    Messages,
}

impl Group {
    /// The group an owner's text belongs to, by the id prefix of the owner.
    fn of_owner(owner: &str) -> Self {
        if owner.starts_with("op-") {
            Self::Ops
        } else if owner.starts_with("kind-") {
            Self::Kinds
        } else {
            Self::Registered
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Self::Ops => "## Ops",
            Self::Kinds => "## Part kinds",
            Self::Registered => "## Registered text",
            Self::Messages => "## Messages",
        }
    }
}

/// One entry of the file. The derived order (group, then id, then text) is the file's order.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    group: Group,
    id: String,
    text: String,
}

/// The two resource comments that open a non-empty file.
fn header(package: &str) -> String {
    format!(
        "### Generated from {package}'s registered ops, part kinds and messages. Do not edit:\n\
         ### OMUZAKI_BLESS=1 cargo test -p {package} --test l10n rewrites it."
    )
}

/// Pushes an owner's entry, then one entry per argument node its schema walk visits.
fn owner_entries(item: &RegisteredText, entries: &mut Vec<Entry>) {
    let group = Group::of_owner(&item.owner);
    let mut lines = comment_lines(&item.context);
    lines.push(format!("{} = {}", item.owner, pattern(&item.value)));
    for (name, value) in &item.attributes {
        lines.push(format!(".{name} = {}", pattern(value)));
    }
    entries.push(Entry { group, id: item.owner.clone(), text: lines.join("\n") });

    let Some(schema) = &item.schema else {
        return;
    };
    // An ambiguous schema is reported by the `registered` check, which fails the test run. Extraction keeps the entries
    // walked before the ambiguity, so its output stays deterministic.
    let outcome = walk(schema, |path, node| {
        // The root holds the owner's own text, which the owner entry already carries.
        if path.is_empty() {
            return;
        }
        let id = ids::arg(&item.owner, path);
        let key = path.last().copied().unwrap_or_default();
        let mut lines = Vec::new();
        let facts = type_range_default(node);
        if !facts.is_empty() {
            lines.push(format!("# {facts}"));
        }
        lines.push(format!("{id} = {}", pattern(&schema_text::title(node, key))));
        if let Some(description) = schema_text::description(node) {
            lines.push(format!(".description = {}", pattern(description)));
        }
        entries.push(Entry { group, id, text: lines.join("\n") });
    });
    if let Err(_ambiguous) = outcome {
        // Reported by the `registered` check; nothing more is written for this owner.
    }
}

/// A message's entry: the translator note, one example line per argument, the verbatim marker, then the message.
fn message_entry(decl: &MessageDecl) -> Entry {
    let mut lines = Vec::new();
    let note = decl.note_text();
    if !note.is_empty() {
        lines.push(format!("# {note}"));
    }
    for &(name, kind) in decl.args {
        lines.push(format!("# ${name} {}", example(kind)));
    }
    if decl.verbatim {
        lines.push("# verbatim: translators and the pseudo-locale leave it as it is".to_owned());
    }
    // The English is already Fluent, so it is written as it is.
    lines.push(format!("{} = {}", decl.id, decl.english));
    Entry { group: Group::Messages, id: decl.id.to_owned(), text: lines.join("\n") }
}

/// What a translator sees for an argument of `kind`: the kind and an example value.
fn example(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::Id => "(id, never translated): \"hair-b\"",
        ArgKind::Int => "(int): 3",
        ArgKind::Num => "(number): 1.5",
        ArgKind::Qty => "(quantity): 12.5 cm",
        ArgKind::Text => "(text): a nested phrase",
        ArgKind::List => "(list): a, b and c",
        ArgKind::Verbatim => "(verbatim English, never translated): \"file not found\"",
    }
}

/// The `#` lines of a context, one per line; none when the context is empty.
fn comment_lines(context: &str) -> Vec<String> {
    context
        .trim()
        .lines()
        .map(|line| {
            let comment = format!("# {line}");
            comment.trim_end().to_owned()
        })
        .collect()
}

/// The argument comment of design §11: the type without `null`, then the constraints and the default, joined with `, `.
fn type_range_default(node: &Map<String, Value>) -> String {
    let kinds: Vec<&str> = match node.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let named: Vec<&str> = kinds.into_iter().filter(|kind| *kind != "null").collect();

    let mut parts = Vec::new();
    if !named.is_empty() {
        parts.push(named.join(" or "));
    }
    parts.extend(range(node));
    if let Some(step) = node.get("multipleOf").filter(|value| value.is_number()) {
        parts.push(format!("multiple of {}", literal(step)));
    }
    if let Some(min) = node.get("minLength").filter(|value| value.is_number()) {
        parts.push(format!("length at least {}", literal(min)));
    }
    if let Some(max) = node.get("maxLength").filter(|value| value.is_number()) {
        parts.push(format!("length at most {}", literal(max)));
    }
    // A `null` in an enum is the `Option`'s, not one of its values.
    if let Some(Value::Array(values)) = node.get("enum") {
        let values: Vec<String> = values.iter().filter(|value| !value.is_null()).map(literal).collect();
        if !values.is_empty() {
            parts.push(format!("one of {}", values.join(", ")));
        }
    }
    if let Some(constant) = node.get("const").filter(|value| !value.is_null()) {
        parts.push(format!("only {}", literal(constant)));
    }
    if let Some(default) = node.get("default") {
        parts.push(format!("default {}", literal(default)));
    }
    parts.join(", ")
}

/// The numeric bounds as phrases. A closed pair reads "0 to 1"; any exclusive bound, or a half-open range, reads as
/// phrases, in the order lower then upper, so the wording never claims an exclusive bound is inclusive.
fn range(node: &Map<String, Value>) -> Vec<String> {
    let bound = |keyword: &str| node.get(keyword).filter(|value| value.is_number());
    let exclusive = bound("exclusiveMinimum").is_some() || bound("exclusiveMaximum").is_some();
    if let (false, Some(min), Some(max)) = (exclusive, bound("minimum"), bound("maximum")) {
        return vec![format!("{} to {}", literal(min), literal(max))];
    }
    [
        ("minimum", "at least"),
        ("exclusiveMinimum", "greater than"),
        ("maximum", "at most"),
        ("exclusiveMaximum", "less than"),
    ]
    .into_iter()
    .filter_map(|(keyword, phrase)| bound(keyword).map(|value| format!("{phrase} {}", literal(value))))
    .collect()
}

/// A bound, step, length, enum value, constant or default as a comment shows it. An integer prints exactly, at its full
/// width; only a float is shown in its shortest form, so `1.0` reads as `1`.
fn literal(value: &Value) -> String {
    match value {
        Value::Number(number) if number.is_f64() => {
            number.as_f64().map_or_else(|| number.to_string(), |float| float.to_string())
        }
        other => other.to_string(),
    }
}

/// A registered value as a Fluent pattern, to follow `= `. The whole value is trimmed, as the contract says; the
/// whitespace inside it is the value's own and is kept. Each continuation line is indented by four spaces, and a blank
/// line stays blank.
fn pattern(value: &str) -> String {
    let mut lines = value.trim().lines();
    let mut out = String::new();
    if let Some(first) = lines.next() {
        out.push_str(&line_text(first));
    }
    for line in lines {
        out.push('\n');
        if !line.is_empty() {
            out.push_str(&continuation(line));
        }
    }
    out
}

/// One continuation line, indented. Fluent strips the leading whitespace of a continuation line, so the value's own
/// indent, and a whitespace-only line, are written as string literals. A line that starts with `[`, `*` or `.` would
/// read as a select or an attribute, so that character is written as a literal too.
fn continuation(line: &str) -> String {
    let body = line.trim_start();
    let lead = &line[..line.len() - body.len()];
    let mut out = String::from("    ");
    if !lead.is_empty() {
        let _ = write!(out, "{{\"{lead}\"}}");
    }
    if body.is_empty() {
        return out;
    }
    let mut rest = body;
    if let Some(first) = body.chars().next().filter(|c| matches!(c, '[' | '*' | '.')) {
        let _ = write!(out, "{{\"{first}\"}}");
        rest = &body[first.len_utf8()..];
    }
    out.push_str(&line_text(rest));
    out
}

/// The text of a line, braces escaped. The whitespace at its end is written as a string literal, because Fluent drops
/// the trailing spaces of a line otherwise.
fn line_text(text: &str) -> String {
    let body = text.trim_end();
    let mut out = escape(body);
    let tail = &text[body.len()..];
    if !tail.is_empty() {
        let _ = write!(out, "{{\"{tail}\"}}");
    }
    out
}

/// Writes each brace as a string literal, so that a registered value cannot open a placeable.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '{' => out.push_str("{\"{\"}"),
            '}' => out.push_str("{\"}\"}"),
            _ => out.push(c),
        }
    }
    out
}
