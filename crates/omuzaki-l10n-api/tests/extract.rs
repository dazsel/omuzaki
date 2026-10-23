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
        (json!({"type": "array", "minItems": 1}), "array, at least 1 items"),
        (json!({"type": "array", "maxItems": 3}), "array, at most 3 items"),
        (json!({"type": "array", "minItems": 1, "maxItems": 3}), "array, 1 to 3 items"),
        (json!({"type": "object", "minProperties": 2}), "object, at least 2 properties"),
        (json!({"type": "object", "maxProperties": 4}), "object, at most 4 properties"),
        (json!({"type": "array", "uniqueItems": true}), "array, unique items"),
        (json!({"type": "array", "uniqueItems": false}), "array"),
        (json!({"anyOf": [{"type": "integer", "minimum": 0}, {"type": "null"}]}), "integer, at least 0"),
        (json!({"oneOf": [{"type": "integer", "minimum": 0}, {"type": "null"}]}), "integer, at least 0"),
        (json!({"anyOf": [{"oneOf": [{"type": "integer", "maximum": 9}]}, {"type": "null"}]}), "integer, at most 9"),
        (json!({"anyOf": [{"type": "integer"}, {"type": "string"}]}), "integer or string"),
        (
            json!({"anyOf": [{"type": "integer", "minimum": 0}, {"type": "string", "maxLength": 8}]}),
            "integer, at least 0 or string, length at most 8",
        ),
        (json!({"anyOf": [{"type": "integer"}, {"type": "null"}], "default": 5}), "integer, default 5"),
        (json!({"anyOf": [{"oneOf": [{"const": "A"}, {"const": "B"}]}, {"type": "null"}]}), "one of \"A\", \"B\""),
        (json!({"type": "integer", "minimum": 9_007_199_254_740_993_i64}), "integer, at least 9007199254740993"),
        (
            json!({"type": "integer", "minimum": u64::MAX, "default": i64::MIN}),
            "integer, at least 18446744073709551615, default -9223372036854775808",
        ),
        (json!({"allOf": [{"type": "integer", "minimum": 0}, {"maximum": 9}]}), "integer, 0 to 9"),
        (json!({"allOf": [{"type": "string", "enum": ["a", "b"]}, {"enum": ["b", "c"]}]}), "string, one of \"b\""),
        (
            json!({"allOf": [{"type": "integer", "minimum": 0}, {"type": "integer", "minimum": 3}]}),
            "integer, at least 3",
        ),
        (json!({"allOf": [{"type": "integer"}, {"type": "string"}]}), "no value fits"),
        (
            json!({"allOf": [{"anyOf": [{"type": "integer", "minimum": 0}, {"type": "string"}]}, {"maximum": 9}]}),
            "integer, 0 to 9 or string",
        ),
        (
            json!({"anyOf": [{"allOf": [{"type": "integer", "minimum": 0}, {"maximum": 9}]}, {"type": "null"}]}),
            "integer, 0 to 9",
        ),
        (
            json!({"allOf": [{"$ref": "#/$defs/n"}], "$defs": {"n": {"type": "integer", "minimum": 0}}}),
            "constraints not shown",
        ),
        (json!({"allOf": [{"type": "integer", "minimum": 0}, {"anyOf": [{"type": "null"}]}]}), "no value fits"),
        (json!({"type": "integer", "minimum": 0, "not": {"const": 3}}), "integer, at least 0, constraints not shown"),
        (json!({"type": "string", "pattern": "^a+$"}), "string, pattern \"^a+$\""),
        (json!({"type": "string", "format": "uri"}), "string, format \"uri\""),
        // Fail closed: the comment cannot state these exactly, so the node reads "constraints not shown".
        (json!({"oneOf": [{"type": "integer"}], "anyOf": [{"type": "string"}]}), "constraints not shown"),
        (json!({"allOf": [{"minimum": 0}], "anyOf": [{"type": "integer"}]}), "constraints not shown"),
        (json!({"allOf": [{"const": 1}, {"const": 2}]}), "constraints not shown"),
        (json!({"allOf": [{"enum": ["a"]}, {"enum": ["b"]}]}), "constraints not shown"),
        (json!({"allOf": [{"enum": ["a", "b"]}, {"const": "c"}]}), "constraints not shown"),
        // Exact cases still render their facts.
        (json!({"allOf": [{"const": "x"}, {"const": "x"}]}), "only \"x\""),
        (json!({"allOf": [{"type": "integer"}, {"type": "number", "minimum": 0}]}), "integer, at least 0"),
        (
            json!({"allOf": [
                {"type": "integer", "minimum": 9_007_199_254_740_993_i64},
                {"type": "number", "minimum": 9_007_199_254_740_992.0}
            ]}),
            "integer, at least 9007199254740993",
        ),
        (
            json!({"allOf": [
                {"type": "number", "minimum": 9_007_199_254_740_992.0},
                {"type": "integer", "minimum": 9_007_199_254_740_993_i64}
            ]}),
            "integer, at least 9007199254740993",
        ),
        (
            json!({"allOf": [{"type": "number", "minimum": 0.5}, {"type": "integer", "minimum": 0}]}),
            "integer, at least 0.5",
        ),
    ];
    assert_renders(rows);
}

/// The combinators never describe a schema as more permissive than it is (#1896).
#[test]
fn the_comment_never_reads_wider_than_the_schema() {
    let rows = vec![
        // A union emptied of its `null` branches admits no non-null value, which is not the same as no union (#1896).
        (json!({"allOf": [{"type": "integer"}, {"anyOf": [{"type": "null"}, false]}]}), "no value fits"),
        // Boolean subschemas: `false` admits nothing, `true` changes nothing, a `true` branch admits anything.
        (json!({"type": "integer", "allOf": [false]}), "no value fits"),
        (json!({"type": "integer", "minimum": 0, "allOf": [true]}), "integer, at least 0"),
        (json!({"anyOf": [{"type": "integer"}, true]}), "constraints not shown"),
        // `oneOf` is exclusive: branches a value could match at once fail closed, apart ones read as a union.
        (json!({"oneOf": [{"type": "number"}, {"type": "integer"}]}), "constraints not shown"),
        (json!({"oneOf": [{"const": "a"}, {"const": "a"}]}), "constraints not shown"),
        (json!({"oneOf": [{"type": "integer"}, {"type": "string"}]}), "integer or string"),
        (json!({"oneOf": [{"type": "string", "const": "a"}, {"type": "string", "const": "b"}]}), "one of \"a\", \"b\""),
        (
            json!({"oneOf": [{"type": "object", "minProperties": 1}, {"type": "object"}]}),
            "object, at least 1 properties or object",
        ),
    ];
    assert_renders(rows);
}

fn assert_renders(rows: Vec<(serde_json::Value, &str)>) {
    let mut misses = Vec::new();
    for (node, expected) in rows {
        let schema = Schema::try_from(json!({"type": "object", "properties": {"arg": node}}))
            .unwrap_or_else(|error| panic!("{error}"));
        let op = RegisteredText { schema: Some(schema), ..owner("op-x-y", "Op.") };
        let out = english_of(&PackageText::new("omuzaki-x").with_registered(vec![op]));
        let want = format!("# {expected}\nop-x-y--arg = Arg\n");
        if !out.contains(&want) {
            misses.push(format!("expected `{expected}`, got:\n{out}"));
        }
    }
    assert!(misses.is_empty(), "{}", misses.join("\n----\n"));
}

/// A description the wrapper lacks is taken from its branch, through `allOf`, `oneOf` and `anyOf`; the wrapper's own
/// description always wins over a branch's.
#[test]
fn a_description_only_on_a_branch_is_used_when_the_wrapper_has_none() {
    let rows = vec![
        (json!({"anyOf": [{"type": "integer", "description": "Branch."}, {"type": "null"}]}), Some("Branch.")),
        (json!({"allOf": [{"oneOf": [{"type": "integer", "description": "Deep."}]}]}), Some("Deep.")),
        (
            json!({"description": "Wrapper.", "anyOf": [{"type": "integer", "description": "Branch."}, {"type": "null"}]}),
            Some("Wrapper."),
        ),
        (json!({"type": "integer"}), None),
    ];
    let mut misses = Vec::new();
    for (node, want) in rows {
        let schema = Schema::try_from(json!({"type": "object", "properties": {"arg": node}}))
            .unwrap_or_else(|error| panic!("{error}"));
        let op = RegisteredText { schema: Some(schema), ..owner("op-x-y", "Op.") };
        let out = english_of(&PackageText::new("omuzaki-x").with_registered(vec![op]));
        let found = out
            .lines()
            .skip_while(|line| *line != "op-x-y--arg = Arg")
            .nth(1)
            .and_then(|line| line.strip_prefix(".description = "));
        if found != want {
            misses.push(format!("expected {want:?}, got {found:?} in:\n{out}"));
        }
    }
    assert!(misses.is_empty(), "{}", misses.join("\n----\n"));
}

/// Whitespace inside a value or an attribute reads back unchanged. The whole value is trimmed; a line's own trailing
/// spaces, a whitespace-only line, and a blank line inside it are kept.
#[test]
fn whitespace_inside_a_value_and_an_attribute_survives_the_round_trip() {
    let value = "\n\nFirst  \n   \nSecond\t\n\nThird  \n\n";
    let attribute = "  Attr  \n   \nEnd\n";
    let op =
        RegisteredText { attributes: vec![("description".to_owned(), attribute.to_owned())], ..owner("op-x-y", value) };
    let out = english_of(&PackageText::new("omuzaki-x").with_registered(vec![op]));

    let resource = match parse(out.as_str()) {
        Ok(resource) => resource,
        Err((_, errors)) => panic!("the English file does not parse: {errors:?}\n{out}"),
    };
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

/// An ambiguous schema leaves no partial entries: the walk returns its error, and the owner's entries are dropped as a
/// whole. A clean owner beside it is unaffected. The `registered` check reports the error; its own test lives with it.
#[test]
fn an_ambiguous_schema_leaves_no_partial_entries() {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "aaa": { "type": "integer", "description": "Before." },
            "zzz": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "Named": { "type": "integer" } },
                        "required": ["Named"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": { "a": { "type": "integer" } },
                        "required": ["a"]
                    }
                ]
            }
        }
    }))
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(omuzaki_l10n_api::schema_text::walk(&schema, |_, _| {}).is_err(), "the schema is ambiguous");

    let bad = RegisteredText { schema: Some(schema), ..owner("op-bad-x", "Bad.") };
    let good = owner("op-good-y", "Good.");
    let out = english_of(&PackageText::new("omuzaki-x").with_registered(vec![bad, good]));
    assert!(out.contains("op-good-y = Good."), "{out}");
    assert!(!out.contains("op-bad-x"), "no partial entries for the ambiguous owner:\n{out}");
}

/// The order a registry gave an owner's attributes in does not change the bytes: they render in name order.
#[test]
fn attribute_order_does_not_change_the_bytes() {
    let attributes =
        vec![("summary".to_owned(), "Sums.".to_owned()), ("description".to_owned(), "Describes.".to_owned())];
    let forward = RegisteredText { attributes: attributes.clone(), ..owner("op-x-y", "Op.") };
    let mut reversed_attributes = attributes;
    reversed_attributes.reverse();
    let backward = RegisteredText { attributes: reversed_attributes, ..owner("op-x-y", "Op.") };
    assert_eq!(
        english_of(&PackageText::new("omuzaki-x").with_registered(vec![forward])),
        english_of(&PackageText::new("omuzaki-x").with_registered(vec![backward])),
    );
}
