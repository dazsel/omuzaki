//! Extraction: the English Fluent file of a package, generated from the text its registries and `messages!` tables
//! submit (design `docs/design/l10n-ops.md` §3.1, §3.3 and §11).
//!
//! The output is deterministic. Entries are sorted by group and id, so the same text in any input order gives the same
//! bytes, and a blessed file never churns.

use std::cmp::Ordering;

use serde_json::{Map, Value};

use crate::ids;
use crate::registry::{ArgKind, MessageDecl, MessageTable, RegisteredText, TextSource};
use crate::schema_text::{self, is_null_schema, walk};

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
/// The order is fixed by the output: groups in their order, entries by id, an owner's attributes by name. The one
/// order the output keeps is the caller's own, because it is part of the text: an enum's values follow the schema's
/// order, and a message's `# $arg` lines follow its declaration. Both come from the same schema or declaration every
/// time, so they are deterministic too.
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
    // Attributes render in name order, so the output does not depend on the order the registry gave them in.
    let mut attributes: Vec<&(String, String)> = item.attributes.iter().collect();
    attributes.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, value) in attributes {
        lines.push(format!(".{name} = {}", pattern(value)));
    }
    // The owner's entries are built here and appended only when its whole schema reads. An ambiguous schema leaves no
    // partial entries behind; the `registered` check reports the ambiguity.
    let mut own = vec![Entry { group, id: item.owner.clone(), text: lines.join("\n") }];

    let Some(schema) = &item.schema else {
        entries.extend(own);
        return;
    };
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
        if let Some(description) = description_of(node) {
            lines.push(format!(".description = {}", pattern(description)));
        }
        own.push(Entry { group, id, text: lines.join("\n") });
    });
    if outcome.is_ok() {
        entries.extend(own);
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

/// The argument comment of design §11: the facts of the node, from [`facts`].
fn type_range_default(node: &Map<String, Value>) -> String {
    facts(node)
}

/// The facts an argument comment shows, merged through the combinators the walk passes through.
///
/// Each alternative is one way the value can be, described by its own type and constraints. `allOf` intersects its
/// members. `oneOf` and `anyOf` form a union of their non-null branches, and a `null` branch is left out, so an
/// `Option` reads as its value. Alternatives that describe alike appear once, and different ones follow one another
/// joined with " or ": `integer, at least 0 or string, length at most 8`. Where several alternatives are each a bare
/// constant, they gather into one `one of` phrase. A keyword the comment cannot show (`$ref`, `not`, `if`, or any
/// keyword it does not know) reports "constraints not shown", so a constraint that could change the type or range is
/// never dropped silently. The default is the first alternative's.
fn facts(node: &Map<String, Value>) -> String {
    let alternatives = expand(node);
    let default = alternatives.first().and_then(|alternative| alternative.default);
    let mut parts: Vec<String> = Vec::new();
    let mut values: Vec<String> = Vec::new();
    for alternative in &alternatives {
        if let (true, Some(constant)) = (alternatives.len() > 1, alternative.bare_constant()) {
            let text = literal(constant);
            if !values.contains(&text) {
                values.push(text);
            }
            continue;
        }
        let text = alternative.describe();
        if !text.is_empty() && !parts.contains(&text) {
            parts.push(text);
        }
    }
    if !values.is_empty() {
        parts.push(format!("one of {}", values.join(", ")));
    }
    let mut text = parts.join(" or ");
    if let Some(default) = default {
        if !text.is_empty() {
            text.push_str(", ");
        }
        text.push_str("default ");
        text.push_str(&literal(default));
    }
    text
}

/// The keywords the comment understands. Any other keyword means the comment cannot show every constraint.
const KNOWN_KEYWORDS: &[&str] = &[
    "type",
    "title",
    "description",
    "default",
    "examples",
    "$schema",
    "$id",
    "$comment",
    "$defs",
    "deprecated",
    "readOnly",
    "writeOnly",
    "const",
    "enum",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "items",
    "prefixItems",
    "additionalProperties",
    "properties",
    "required",
    "oneOf",
    "anyOf",
    "allOf",
];

/// One alternative of a schema, with its keywords gathered. [`merge`] intersects two alternatives (an `allOf`), and
/// [`expand`] forms the union of a node's `oneOf` and `anyOf` branches.
#[derive(Clone, Default)]
struct Facts<'a> {
    /// The types it allows, without `null`; `None` when it states no type. An empty list allows no value.
    kinds: Option<Vec<&'a str>>,
    lower: Option<Bound<'a>>,
    upper: Option<Bound<'a>>,
    multiple_of: Vec<&'a Value>,
    min_length: Option<&'a Value>,
    max_length: Option<&'a Value>,
    min_items: Option<&'a Value>,
    max_items: Option<&'a Value>,
    min_properties: Option<&'a Value>,
    max_properties: Option<&'a Value>,
    unique_items: bool,
    patterns: Vec<&'a str>,
    format: Option<&'a str>,
    /// The values it admits, without `null`; `None` when it states no enum. An empty list admits no value.
    values: Option<Vec<&'a Value>>,
    constant: Option<&'a Value>,
    default: Option<&'a Value>,
    /// A keyword the comment cannot show is present.
    not_shown: bool,
}

/// A numeric bound. `exclusive` means the bound itself is not allowed.
#[derive(Clone, Copy)]
struct Bound<'a> {
    value: &'a Value,
    exclusive: bool,
}

impl Facts<'_> {
    /// Drops the keywords that cannot constrain any type the alternative allows. JSON Schema applies `maximum` to
    /// numbers only, so an untyped `maximum` met by a `string` branch says nothing about the string.
    fn keep_applicable(mut self) -> Self {
        let Some(kinds) = &self.kinds else { return self };
        let allows = |names: &[&str]| kinds.iter().any(|kind| names.contains(kind));
        if !allows(&["integer", "number"]) {
            self.lower = None;
            self.upper = None;
            self.multiple_of.clear();
        }
        if !allows(&["string"]) {
            self.min_length = None;
            self.max_length = None;
            self.patterns.clear();
            self.format = None;
        }
        if !allows(&["array"]) {
            self.min_items = None;
            self.max_items = None;
            self.unique_items = false;
        }
        if !allows(&["object"]) {
            self.min_properties = None;
            self.max_properties = None;
        }
        self
    }

    /// Whether anything narrows the alternative beyond its type.
    fn narrows(&self) -> bool {
        self.lower.is_some()
            || self.upper.is_some()
            || !self.multiple_of.is_empty()
            || self.min_length.is_some()
            || self.max_length.is_some()
            || self.min_items.is_some()
            || self.max_items.is_some()
            || self.min_properties.is_some()
            || self.max_properties.is_some()
            || self.unique_items
            || !self.patterns.is_empty()
            || self.format.is_some()
            || self.values.is_some()
            || self.not_shown
    }

    /// The constant of an alternative that is nothing but a constant.
    fn bare_constant(&self) -> Option<&Value> {
        self.constant.filter(|_| !self.narrows())
    }

    /// The alternative as its comment reads, without its default.
    fn describe(&self) -> String {
        if self.kinds.as_ref().is_some_and(Vec::is_empty) || self.values.as_ref().is_some_and(Vec::is_empty) {
            return "no value fits".to_owned();
        }
        let mut parts = Vec::new();
        if let Some(kinds) = &self.kinds {
            parts.push(kinds.join(" or "));
        }
        parts.extend(self.range());
        parts.extend(count_phrase(self.min_items, self.max_items, "items"));
        parts.extend(count_phrase(self.min_properties, self.max_properties, "properties"));
        if self.unique_items {
            parts.push("unique items".to_owned());
        }
        parts.extend(self.multiple_of.iter().map(|step| format!("multiple of {}", literal(step))));
        parts.extend(self.min_length.map(|min| format!("length at least {}", literal(min))));
        parts.extend(self.max_length.map(|max| format!("length at most {}", literal(max))));
        parts.extend(self.patterns.iter().map(|pattern| format!("pattern {}", quoted(pattern))));
        parts.extend(self.format.map(|format| format!("format {}", quoted(format))));
        if let Some(values) = &self.values {
            parts.push(format!("one of {}", values.iter().map(|value| literal(value)).collect::<Vec<_>>().join(", ")));
        }
        parts.extend(self.constant.map(|constant| format!("only {}", literal(constant))));
        if self.not_shown {
            parts.push("constraints not shown".to_owned());
        }
        parts.join(", ")
    }

    /// The numeric bounds as phrases. A closed pair reads "0 to 1"; any exclusive bound, or a half-open range, reads as
    /// phrases, lower then upper, so the wording never claims an exclusive bound is inclusive.
    fn range(&self) -> Vec<String> {
        match (self.lower, self.upper) {
            (Some(lower), Some(upper)) if !lower.exclusive && !upper.exclusive => {
                vec![format!("{} to {}", literal(lower.value), literal(upper.value))]
            }
            (lower, upper) => lower
                .map(|bound| {
                    format!("{} {}", if bound.exclusive { "greater than" } else { "at least" }, literal(bound.value))
                })
                .into_iter()
                .chain(upper.map(|bound| {
                    format!("{} {}", if bound.exclusive { "less than" } else { "at most" }, literal(bound.value))
                }))
                .collect(),
        }
    }
}

/// The alternatives a node allows: its own keywords, intersected with each `allOf` member, and with the union of its
/// non-null `oneOf` and `anyOf` branches. Each branch is expanded the same way, so the nesting has no depth limit.
fn expand(node: &Map<String, Value>) -> Vec<Facts<'_>> {
    let mut current = vec![own(node)];
    if let Some(members) = node.get("allOf").and_then(Value::as_array) {
        for member in members.iter().filter_map(Value::as_object) {
            current = cross(&current, &expand(member));
        }
    }
    let union: Vec<Facts<'_>> = ["oneOf", "anyOf"]
        .into_iter()
        .filter_map(|keyword| node.get(keyword).and_then(Value::as_array))
        .flatten()
        .filter(|branch| !is_null_schema(branch))
        .filter_map(Value::as_object)
        .flat_map(expand)
        .collect();
    cross(&current, &union).into_iter().map(Facts::keep_applicable).collect()
}

/// Every intersection of an alternative on the left with one on the right. An empty right side leaves the left alone.
fn cross<'a>(left: &[Facts<'a>], right: &[Facts<'a>]) -> Vec<Facts<'a>> {
    if right.is_empty() {
        return left.to_vec();
    }
    left.iter()
        .flat_map(|alternative| right.iter().map(move |other| merge(alternative.clone(), other.clone())))
        .collect()
}

/// The intersection of two alternatives: the types they both allow, the tighter bounds and counts, the values they both
/// admit, and every pattern and multiple. A keyword only one of them states is kept.
fn merge<'a>(a: Facts<'a>, b: Facts<'a>) -> Facts<'a> {
    Facts {
        kinds: match (a.kinds, b.kinds) {
            (Some(x), Some(y)) => Some(x.into_iter().filter(|kind| y.contains(kind)).collect()),
            (x, y) => x.or(y),
        },
        lower: tighter_lower(a.lower, b.lower),
        upper: tighter_upper(a.upper, b.upper),
        multiple_of: a.multiple_of.into_iter().chain(b.multiple_of).collect(),
        min_length: larger(a.min_length, b.min_length),
        max_length: smaller(a.max_length, b.max_length),
        min_items: larger(a.min_items, b.min_items),
        max_items: smaller(a.max_items, b.max_items),
        min_properties: larger(a.min_properties, b.min_properties),
        max_properties: smaller(a.max_properties, b.max_properties),
        unique_items: a.unique_items || b.unique_items,
        patterns: a.patterns.into_iter().chain(b.patterns).collect(),
        format: a.format.or(b.format),
        values: match (a.values, b.values) {
            (Some(x), Some(y)) => Some(x.into_iter().filter(|value| y.contains(value)).collect()),
            (x, y) => x.or(y),
        },
        constant: a.constant.or(b.constant),
        default: a.default.or(b.default),
        not_shown: a.not_shown || b.not_shown,
    }
}

/// The keywords of one node, without its combinators.
fn own(node: &Map<String, Value>) -> Facts<'_> {
    let number = |key: &str| node.get(key).filter(|value| value.is_number());
    let text = |key: &str| node.get(key).and_then(Value::as_str);
    let kinds = match node.get("type") {
        Some(Value::String(kind)) => Some(vec![kind.as_str()]),
        Some(Value::Array(kinds)) => Some(kinds.iter().filter_map(Value::as_str).collect()),
        _ => None,
    }
    .map(|kinds: Vec<&str>| kinds.into_iter().filter(|kind| *kind != "null").collect());
    let minimum = number("minimum").map(|value| Bound { value, exclusive: false });
    let exclusive_minimum = number("exclusiveMinimum").map(|value| Bound { value, exclusive: true });
    let maximum = number("maximum").map(|value| Bound { value, exclusive: false });
    let exclusive_maximum = number("exclusiveMaximum").map(|value| Bound { value, exclusive: true });
    Facts {
        kinds,
        lower: tighter_lower(minimum, exclusive_minimum),
        upper: tighter_upper(maximum, exclusive_maximum),
        multiple_of: number("multipleOf").into_iter().collect(),
        min_length: number("minLength"),
        max_length: number("maxLength"),
        min_items: number("minItems"),
        max_items: number("maxItems"),
        min_properties: number("minProperties"),
        max_properties: number("maxProperties"),
        unique_items: node.get("uniqueItems") == Some(&Value::Bool(true)),
        patterns: text("pattern").into_iter().collect(),
        format: text("format"),
        values: node
            .get("enum")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter(|value| !value.is_null()).collect()),
        constant: node.get("const").filter(|value| !value.is_null()),
        default: node.get("default"),
        not_shown: node.keys().any(|key| !KNOWN_KEYWORDS.contains(&key.as_str())),
    }
}

/// The tighter of two lower bounds: the greater value, and exclusive when either is.
fn tighter_lower<'a>(a: Option<Bound<'a>>, b: Option<Bound<'a>>) -> Option<Bound<'a>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(match compare(x.value, y.value) {
            Ordering::Greater => x,
            Ordering::Less => y,
            Ordering::Equal => Bound { value: x.value, exclusive: x.exclusive || y.exclusive },
        }),
        (x, y) => x.or(y),
    }
}

/// The tighter of two upper bounds: the smaller value, and exclusive when either is.
fn tighter_upper<'a>(a: Option<Bound<'a>>, b: Option<Bound<'a>>) -> Option<Bound<'a>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(match compare(x.value, y.value) {
            Ordering::Less => x,
            Ordering::Greater => y,
            Ordering::Equal => Bound { value: x.value, exclusive: x.exclusive || y.exclusive },
        }),
        (x, y) => x.or(y),
    }
}

/// The larger of two counts or lengths that both bound from below.
fn larger<'a>(a: Option<&'a Value>, b: Option<&'a Value>) -> Option<&'a Value> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if compare(x, y) == Ordering::Less { y } else { x }),
        (x, y) => x.or(y),
    }
}

/// The smaller of two counts or lengths that both bound from above.
fn smaller<'a>(a: Option<&'a Value>, b: Option<&'a Value>) -> Option<&'a Value> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if compare(x, y) == Ordering::Greater { y } else { x }),
        (x, y) => x.or(y),
    }
}

/// Orders two numbers exactly where both are integers, and as floats otherwise.
fn compare(a: &Value, b: &Value) -> Ordering {
    match (exact_integer(a), exact_integer(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => a.as_f64().partial_cmp(&b.as_f64()).unwrap_or(Ordering::Equal),
    }
}

/// An integer as an `i128`, which holds every `i64` and `u64`; `None` for a float.
fn exact_integer(value: &Value) -> Option<i128> {
    let number = value.as_number()?;
    if number.is_f64() {
        return None;
    }
    number.as_i64().map(i128::from).or_else(|| number.as_u64().map(i128::from))
}

/// A count bound as a phrase: "2 to 5 items" for a closed pair, otherwise "at least 2 items" or "at most 5 items".
fn count_phrase(min: Option<&Value>, max: Option<&Value>, noun: &str) -> Option<String> {
    match (min, max) {
        (Some(min), Some(max)) => Some(format!("{} to {} {noun}", literal(min), literal(max))),
        (Some(min), None) => Some(format!("at least {} {noun}", literal(min))),
        (None, Some(max)) => Some(format!("at most {} {noun}", literal(max))),
        (None, None) => None,
    }
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

/// A string as a comment shows it: in JSON quotes.
fn quoted(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// A node's description, or else the first description on a non-null branch: a wrapper's description is the field's,
/// and a description that sits only on the value's own branch still reaches the translator.
fn description_of(node: &Map<String, Value>) -> Option<&str> {
    schema_text::description(node).or_else(|| {
        ["allOf", "oneOf", "anyOf"]
            .into_iter()
            .filter_map(|keyword| node.get(keyword).and_then(Value::as_array))
            .flatten()
            .filter(|branch| !is_null_schema(branch))
            .filter_map(Value::as_object)
            .find_map(description_of)
    })
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
        out.push_str(&literal_placeable(lead));
    }
    if body.is_empty() {
        return out;
    }
    let mut rest = body;
    if let Some(first) = body.chars().next().filter(|c| matches!(c, '[' | '*' | '.')) {
        out.push_str(&literal_placeable(&first.to_string()));
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
        out.push_str(&literal_placeable(tail));
    }
    out
}

/// A placeable that holds `text` as a string literal, `{"text"}`. Fluent would drop or misread the whitespace or the
/// character it wraps, so it is written as a literal.
fn literal_placeable(text: &str) -> String {
    ["{\"", text, "\"}"].concat()
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
