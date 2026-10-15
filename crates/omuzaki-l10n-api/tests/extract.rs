//! Extraction: the generated English Fluent file (`extract`), from planted text and from the linked registry.
//! The argument fixtures exist only for their generated schemas, so their fields are never read.
#![allow(dead_code)]

use fluent_syntax::ast::{Entry, Expression, InlineExpression, Pattern, PatternElement};
use fluent_syntax::parser::parse;
use omuzaki_l10n_api::extract::{PackageText, english, english_of};
use omuzaki_l10n_api::{ArgKind, MessageDecl, RegisteredText, TextSource};
use schemars::{JsonSchema, Schema};
use serde_json::json;

/// The schema settings of `omuzaki-ops-api`'s `schema_of`: draft 2020-12, every subschema inlined.
fn schema_of<T: JsonSchema>() -> Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>()
}

/// A registered owner with a value, no attributes, no schema and no context.
fn owner(id: &str, value: &str) -> RegisteredText {
    RegisteredText {
        owner: id.to_owned(),
        value: value.to_owned(),
        attributes: Vec::new(),
        schema: None,
        context: String::new(),
    }
}

/// A message with a note, arguments and no verbatim marker.
const PART_NOT_FOUND: MessageDecl = MessageDecl {
    id: "char-part-not-found",
    english: "{ $part } is missing.",
    note: " Shown when an op names a part the character lacks.",
    args: &[("part", ArgKind::Id)],
    verbatim: false,
};

#[test]
fn an_empty_package_writes_an_empty_file() {
    assert_eq!(english_of(&PackageText::new("omuzaki-x")), "");
}

#[test]
fn entries_are_grouped_and_sorted() {
    let kind = RegisteredText {
        context: "part kind title".to_owned(),
        attributes: vec![("summary".to_owned(), "Two tails.".to_owned())],
        ..owner("kind-twin_tails", "Twin tails")
    };
    let op = RegisteredText {
        context: "tooltip".to_owned(),
        attributes: vec![("description".to_owned(), "Moves the outline.".to_owned())],
        ..owner("op-face-set_profile", "Set the face.")
    };
    // Given out of order: the file still sorts them into its groups.
    let text = PackageText::new("omuzaki-x")
        .with_registered(vec![owner("lib-x-thing", "Lib thing"), kind, op])
        .with_messages(&[PART_NOT_FOUND]);

    let expected = "\
### Generated from omuzaki-x's registered ops, part kinds and messages. Do not edit:
### OMUZAKI_BLESS=1 cargo test -p omuzaki-x --test l10n rewrites it.

## Ops

# tooltip
op-face-set_profile = Set the face.
.description = Moves the outline.

## Part kinds

# part kind title
kind-twin_tails = Twin tails
.summary = Two tails.

## Registered text

lib-x-thing = Lib thing

## Messages

# Shown when an op names a part the character lacks.
# $part (id, never translated): \"hair-b\"
char-part-not-found = { $part } is missing.
";
    assert_eq!(english_of(&text), expected);
}

/// Blends the face.
#[derive(JsonSchema)]
struct Blend {
    /// How far to move toward the profile: 0 keeps the current face, 1 is the full profile.
    #[schemars(range(min = 0, max = 1), default = "half")]
    blend: f32,
    /// The outline.
    #[schemars(title = "Outline")]
    outline_size: u32,
}

fn half() -> f32 {
    0.5
}

#[test]
fn arguments_get_titles_descriptions_and_type_comments() {
    let op = RegisteredText { schema: Some(schema_of::<Blend>()), ..owner("op-face-set_profile", "Set the face.") };
    let out = english_of(&PackageText::new("omuzaki-x").with_registered(vec![op]));

    assert!(
        out.contains(
            "# number, 0 to 1, default 0.5\n\
             op-face-set_profile--blend = Blend\n\
             .description = How far to move toward the profile: 0 keeps the current face, 1 is the full profile.\n"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "# integer, at least 0\n\
             op-face-set_profile--outline_size = Outline\n\
             .description = The outline.\n"
        ),
        "{out}"
    );
}

#[test]
fn messages_get_their_note_and_argument_examples() {
    const ARGS: &[(&str, ArgKind)] = &[
        ("part", ArgKind::Id),
        ("count", ArgKind::Int),
        ("size", ArgKind::Num),
        ("length", ArgKind::Qty),
        ("note", ArgKind::Text),
        ("items", ArgKind::List),
    ];
    let kitchen = MessageDecl {
        id: "char-kitchen",
        english: "{ $part } has { $count } items.",
        note: " Says how many parts the kitchen has.\n Shown after a move.",
        args: ARGS,
        verbatim: false,
    };
    let missing = MessageDecl {
        id: "ui-file-missing",
        english: "file not found",
        note: " A key legend.",
        args: &[("path", ArgKind::Verbatim)],
        verbatim: true,
    };
    let out = english_of(&PackageText::new("omuzaki-x").with_messages(&[kitchen, missing]));

    let expected = "\
### Generated from omuzaki-x's registered ops, part kinds and messages. Do not edit:
### OMUZAKI_BLESS=1 cargo test -p omuzaki-x --test l10n rewrites it.

## Messages

# Says how many parts the kitchen has. Shown after a move.
# $part (id, never translated): \"hair-b\"
# $count (int): 3
# $size (number): 1.5
# $length (quantity): 12.5 cm
# $note (text): a nested phrase
# $items (list): a, b and c
char-kitchen = { $part } has { $count } items.

# A key legend.
# $path (verbatim English, never translated): \"file not found\"
# verbatim: translators and the pseudo-locale leave it as it is
ui-file-missing = file not found
";
    assert_eq!(out, expected);
}

/// Reads a parsed pattern back as its text: literal text, and a string-literal placeable as its string.
fn read_back(pattern: &Pattern<&str>) -> String {
    pattern
        .elements
        .iter()
        .map(|element| match element {
            PatternElement::TextElement { value } => (*value).to_owned(),
            PatternElement::Placeable { expression } => match expression {
                Expression::Inline(InlineExpression::StringLiteral { value }) => (*value).to_owned(),
                other => panic!("an escaped brace should be a string literal, got {other:?}"),
            },
        })
        .collect()
}

#[test]
fn values_are_escaped_and_parse_back() {
    let value = "Braces {a} and }.\n[bracket\n*star\n.dot\n\nafter a blank";
    let attribute = "Attr {b}\n  [c]";
    let op =
        RegisteredText { attributes: vec![("description".to_owned(), attribute.to_owned())], ..owner("op-x-y", value) };
    let out = english_of(&PackageText::new("omuzaki-x").with_registered(vec![op]));

    let resource = match parse(out.as_str()) {
        Ok(resource) => resource,
        Err((_, errors)) => panic!("the English file does not parse: {errors:?}\n{out}"),
    };
    assert!(resource.body.iter().all(|entry| !matches!(entry, Entry::Junk { .. })), "junk in\n{out}");

    let message = resource
        .body
        .iter()
        .find_map(|entry| match entry {
            Entry::Message(message) if message.id.name == "op-x-y" => Some(message),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no op-x-y message in\n{out}"));
    assert_eq!(read_back(message.value.as_ref().expect("a value")), value.trim());
    let attr = message.attributes.first().expect("a description attribute");
    assert_eq!(read_back(&attr.value), attribute.trim());
}

fn registry_text(package: &str) -> Vec<RegisteredText> {
    if package == env!("CARGO_PKG_NAME") { vec![owner("lib-probe", "Probe text")] } else { Vec::new() }
}

omuzaki_l10n_api::messages! {
    /// A message declared in this test.
    pub registry_probe = "registry-probe" (count: Int) => "Probe { $count }.";
}

omuzaki_l10n_api::__inventory::submit! {
    TextSource { text: registry_text }
}

#[test]
fn english_reads_the_registry() {
    let out = english(env!("CARGO_PKG_NAME"));
    assert!(out.contains("lib-probe = Probe text\n"), "{out}");
    assert!(out.contains("registry-probe = Probe { $count }.\n"), "{out}");
}

#[test]
fn the_output_is_deterministic() {
    let forward = PackageText::new("omuzaki-x")
        .with_registered(vec![owner("lib-b", "B"), owner("lib-a", "A"), owner("op-q-r", "Q")])
        .with_messages(&[PART_NOT_FOUND]);
    let mut shuffled = forward.clone();
    shuffled.registered.reverse();

    let first = english_of(&forward);
    assert_eq!(first, english_of(&forward));
    assert_eq!(first, english_of(&shuffled));
}

/// Each schema keyword the comment reads, with the comment it must produce. One row per keyword and per wording, so a
/// bound that claims to be inclusive, or an integer that loses digits, fails by name.
#[test]
fn the_comment_renders_each_constraint() {
    let rows = vec![
        (json!({"type": "integer", "minimum": 0}), "integer, at least 0"),
        (json!({"type": "integer", "maximum": 9}), "integer, at most 9"),
        (json!({"type": "number", "minimum": 0, "maximum": 1}), "number, 0 to 1"),
        (json!({"type": "number", "exclusiveMinimum": 0}), "number, greater than 0"),
        (json!({"type": "number", "exclusiveMaximum": 1}), "number, less than 1"),
        (json!({"type": "number", "minimum": 0, "exclusiveMaximum": 1}), "number, at least 0, less than 1"),
        (json!({"type": "number", "multipleOf": 0.5}), "number, multiple of 0.5"),
        (json!({"type": "string", "minLength": 1}), "string, length at least 1"),
        (json!({"type": "string", "maxLength": 8}), "string, length at most 8"),
        (json!({"type": "string", "enum": ["a", "b"]}), "string, one of \"a\", \"b\""),
        (json!({"type": ["integer", "null"], "enum": [1, null]}), "integer, one of 1"),
        (json!({"type": "string", "const": "x"}), "string, only \"x\""),
        (json!({"type": "number", "default": 0.5}), "number, default 0.5"),
        (json!({"type": "number", "minimum": 1.0}), "number, at least 1"),
        (
            json!({"type": "integer", "minimum": u64::MAX, "default": i64::MIN}),
            "integer, at least 18446744073709551615, default -9223372036854775808",
        ),
    ];
    for (node, expected) in rows {
        let schema = Schema::try_from(json!({"type": "object", "properties": {"arg": node}}))
            .unwrap_or_else(|error| panic!("{error}"));
        let op = RegisteredText { schema: Some(schema), ..owner("op-x-y", "Op.") };
        let out = english_of(&PackageText::new("omuzaki-x").with_registered(vec![op]));
        let want = format!("# {expected}\nop-x-y--arg = Arg\n");
        assert!(out.contains(&want), "expected `{expected}` before the argument, got:\n{out}");
    }
}
