//! The `registered` check (`check::registered`): a clean package passes, and each planted fault fails exactly one rule.
//! The fixtures derive their schemas the way `omuzaki-ops-api` does; the fault schemas are hand-written where a
//! derive cannot produce the shape.
//! The fixture types exist only for their generated schemas, so their fields are never read.
#![allow(dead_code)]

use omuzaki_l10n_api::RegisteredText;
use omuzaki_l10n_api::check::{Failure, registered, report};
use omuzaki_l10n_api::extract::PackageText;
use schemars::{JsonSchema, Schema};
use serde_json::json;

/// The schema settings of `omuzaki-ops-api`'s `schema_of`: draft 2020-12, every subschema inlined.
fn schema_of<T: JsonSchema>() -> Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>()
}

/// An op owner with a summary and a schema.
fn op(id: &str, summary: &str, schema: Option<Schema>) -> RegisteredText {
    RegisteredText {
        owner: id.to_owned(),
        value: summary.to_owned(),
        attributes: Vec::new(),
        schema,
        context: String::new(),
    }
}

/// A part kind owner with a title and a summary attribute.
fn kind(id: &str, title: &str, summary: &str) -> RegisteredText {
    RegisteredText {
        owner: id.to_owned(),
        value: title.to_owned(),
        attributes: vec![("summary".to_owned(), summary.to_owned())],
        schema: None,
        context: String::new(),
    }
}

/// Runs the check over the given owners of one package.
fn run(owners: Vec<RegisteredText>) -> Vec<Failure> {
    registered(&PackageText::new("omuzaki-x").with_registered(owners))
}

/// Asserts that `failures` is exactly one failure: its check, its id, and a problem containing `part`.
fn assert_one(failures: &[Failure], id: &str, part: &str) {
    assert_eq!(failures.len(), 1, "expected one failure, got:\n{}", report(failures));
    assert_eq!(failures[0].check, "registered");
    assert_eq!(failures[0].id, id);
    assert!(failures[0].problem.contains(part), "problem `{}` lacks `{part}`", failures[0].problem);
}

/// Blends the face.
#[derive(JsonSchema)]
struct Blend {
    /// How far to move toward the profile.
    blend: f32,
    /// The ease curve.
    ease: Ease,
}

/// Easing mode.
#[derive(JsonSchema)]
enum Ease {
    /// Reaches the target at once.
    Instant,
    /// Reaches the target in a third of the time.
    Fast,
}

#[test]
fn a_clean_package_has_no_failures() {
    let failures = run(vec![
        op("op-face-set_profile", "Set the face.", Some(schema_of::<Blend>())),
        kind("kind-twin_tails", "Twin tails", "Two tails."),
    ]);
    assert!(failures.is_empty(), "{}", report(&failures));
}

#[test]
fn a_multi_line_summary_fails() {
    let failures = run(vec![op("op-face-set_profile", "Set the face.\nAnd more.", None)]);
    assert_one(&failures, "op-face-set_profile", "more than one line");
}

#[test]
fn an_empty_kind_summary_fails() {
    let failures = run(vec![kind("kind-twin_tails", "Twin tails", "")]);
    assert_one(&failures, "kind-twin_tails", "is empty");
}

/// A field with no doc comment.
#[derive(JsonSchema)]
struct Undocumented {
    blend: f32,
}

#[test]
fn a_field_without_a_doc_comment_fails() {
    let failures = run(vec![op("op-face-set_profile", "Set the face.", Some(schema_of::<Undocumented>()))]);
    assert_one(&failures, "op-face-set_profile--blend", "add a doc comment");
}

/// A field whose enum has a variant without a doc comment.
#[derive(JsonSchema)]
struct PartialEase {
    /// The ease curve.
    ease: PartialEaseMode,
}

/// Easing mode.
#[derive(JsonSchema)]
enum PartialEaseMode {
    /// Reaches the target at once.
    Instant,
    Slow,
}

#[test]
fn an_enum_variant_without_a_doc_comment_fails() {
    let failures = run(vec![op("op-face-set_profile", "Set the face.", Some(schema_of::<PartialEase>()))]);
    assert_one(&failures, "op-face-set_profile--ease--Slow", "add a doc comment");
}

#[test]
fn an_op_name_outside_the_grammar_fails() {
    let failures = run(vec![op("op-face-setProfile", "Set the face.", None)]);
    assert_one(&failures, "op-face-setProfile", "bad segment `setProfile`");
}

#[test]
fn an_owner_outside_op_and_kind_fails() {
    let failures = run(vec![op("lib-x", "Lib text.", None)]);
    assert_one(&failures, "lib-x", "only `op-` and `kind-` owners");
}

/// A field whose name is written with `--`, which `ids::arg` cannot tell from a path separator.
#[derive(JsonSchema)]
struct Dashed {
    /// The twin.
    #[schemars(rename = "twin--tails")]
    twin: bool,
}

#[test]
fn a_key_with_a_double_dash_fails() {
    let failures = run(vec![op("op-face-set_profile", "Set the face.", Some(schema_of::<Dashed>()))]);
    assert_one(&failures, "op-face-set_profile--twin--tails", "key `twin--tails`");
}

#[test]
fn two_branches_with_the_same_const_fail() {
    let schema = Schema::try_from(json!({
        "oneOf": [
            { "const": "A", "description": "One." },
            { "const": "A", "description": "Again." }
        ]
    }))
    .expect("a valid schema");
    let failures = run(vec![op("op-face-set_profile", "Set the face.", Some(schema))]);
    assert_one(&failures, "op-face-set_profile--A", "is written by the schema of `op-face-set_profile`, node 2");
}

#[test]
fn report_is_sorted_and_one_line_each() {
    let failures = vec![Failure::new("registered", "op-b", "second"), Failure::new("registered", "op-a", "first")];
    assert_eq!(report(&failures), "registered op-a: first\nregistered op-b: second");
}
