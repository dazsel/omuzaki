//! The `translations` check (`check::translations`): full and empty translations pass, and each planted fault fails
//! exactly one rule.

use omuzaki_l10n_api::check::{Failure, report, translations};
use omuzaki_l10n_api::extract::PackageText;
use omuzaki_l10n_api::{MessageDecl, RegisteredText, Resource};

const DECLS: [MessageDecl; 2] = [
    MessageDecl {
        id: "fixture-saved",
        english: "Saved { $part } to `parts.json`.",
        note: " After a save.",
        args: &[],
        verbatim: false,
    },
    MessageDecl { id: "fixture-plain", english: "Plain.", note: " Plain.", args: &[], verbatim: false },
];

/// Two messages (`fixture-saved` declares no argument, so its placeable is checked against the English as written)
/// and one registered op with a description attribute that has a code span.
fn text() -> PackageText {
    let op = RegisteredText {
        owner: "op-face-set_profile".to_owned(),
        value: "Set the face.".to_owned(),
        attributes: vec![("description".to_owned(), "Same as `face.get_profile`.".to_owned())],
        schema: None,
        context: String::new(),
    };
    PackageText::new("omuzaki-fixture").with_messages(&DECLS).with_registered(vec![op])
}

fn ja(source: &'static str) -> Resource {
    Resource { package: "omuzaki-fixture", locale: "ja", source }
}

fn run(source: &'static str) -> Vec<Failure> {
    translations(&text(), &[ja(source)])
}

/// Asserts that `failures` is empty.
fn assert_clean(failures: &[Failure]) {
    assert!(failures.is_empty(), "{}", report(failures));
}

fn assert_one(failures: &[Failure], id: &str, part: &str) {
    assert_eq!(failures.len(), 1, "expected one failure, got:\n{}", report(failures));
    assert_eq!(failures[0].check, "translations");
    assert_eq!(failures[0].id, id);
    assert!(failures[0].problem.contains(part), "problem `{}` lacks `{part}`", failures[0].problem);
}

const FULL: &str = "\
# en: 12345678
fixture-saved = { $part } を `parts.json` に保存しました。
fixture-plain = 平易。
op-face-set_profile = 顔を設定します。
    .description = `face.get_profile` と同じです。
";

#[test]
fn a_full_translation_and_an_empty_stub_pass() {
    assert_clean(&run(FULL));
    assert_clean(&run(""));
}

#[test]
fn a_partial_translation_passes() {
    assert_clean(&run("fixture-plain = 平易。\n"));
}

#[test]
fn english_resources_are_ignored() {
    let english = Resource { package: "omuzaki-fixture", locale: "en", source: "orphan = ??? {" };
    let other = Resource { package: "omuzaki-other", locale: "ja", source: "orphan = x" };
    assert_clean(&translations(&text(), &[english, other]));
}

#[test]
fn junk_fails_with_the_locale_and_the_line() {
    let failures = run("fixture-plain = 平易。\nthis is not fluent\n");
    assert_eq!(failures.len(), 1, "{}", report(&failures));
    assert!(failures[0].problem.contains("`ja` line 2"), "{}", failures[0].problem);
}

#[test]
fn an_orphan_id_fails() {
    assert_one(&run("fixture-gone = どこか。\n"), "fixture-gone", "does not");
}

#[test]
fn an_orphan_attribute_fails() {
    assert_one(&run("fixture-plain = 平易。\n    .tip = ヒント。\n"), "fixture-plain", "`.tip`");
}

#[test]
fn an_extra_placeable_fails() {
    let failures = run("fixture-plain = { $who } 平易。\n");
    assert_one(&failures, "fixture-plain", "uses $who");
}

#[test]
fn an_extra_placeable_in_an_attribute_fails() {
    let failures = run("op-face-set_profile = 顔。\n    .description = `face.get_profile` { $x }。\n");
    assert_one(&failures, "op-face-set_profile.description", "uses $x");
}

#[test]
fn a_dropped_code_span_fails() {
    assert_one(&run("fixture-saved = { $part } を保存しました。\n"), "fixture-saved", "`parts.json`");
}

#[test]
fn a_changed_code_span_fails() {
    assert_one(&run("fixture-saved = { $part } を `parts.JSON` に保存。\n"), "fixture-saved", "`parts.json`");
}

#[test]
fn a_dropped_code_span_of_an_attribute_fails() {
    let failures = run("op-face-set_profile = 顔。\n    .description = get_profile と同じ。\n");
    assert_one(&failures, "op-face-set_profile.description", "`face.get_profile`");
}
