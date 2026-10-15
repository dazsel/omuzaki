//! The translation-file check (`check::translations`): a full translation and an empty stub pass, and each planted fault
//! fails exactly one rule. The fixtures are `Resource` literals over a `PackageText` of two messages and one op.

use omuzaki_l10n_api::check::{Failure, report, translations};
use omuzaki_l10n_api::extract::PackageText;
use omuzaki_l10n_api::{ArgKind, MessageDecl, RegisteredText, Resource};

/// The package's text: two messages, one of them with a code span, and a registered op.
fn text() -> PackageText {
    PackageText::new("omuzaki-fixture")
        .with_registered(vec![RegisteredText {
            owner: "op-face-set_profile".to_owned(),
            value: "Set the face.".to_owned(),
            attributes: Vec::new(),
            schema: None,
            context: String::new(),
        }])
        .with_messages(&[
            MessageDecl {
                id: "fixture-part",
                english: "{ $part } uses `max_size`.",
                note: " A part and its size.",
                args: &[("part", ArgKind::Id)],
                verbatim: false,
            },
            MessageDecl {
                id: "fixture-count",
                english: "{ $count } items.",
                note: " A count.",
                args: &[("count", ArgKind::Int)],
                verbatim: false,
            },
        ])
}

/// A translation resource for `ja` of the fixture package.
fn ja(source: &'static str) -> Resource {
    Resource { package: "omuzaki-fixture", locale: "ja", source }
}

/// Runs the check over one translation resource.
fn run(source: &'static str) -> Vec<Failure> {
    translations(&text(), &[ja(source)])
}

/// Asserts that a check found nothing, printing what it found otherwise.
fn assert_clean(failures: &[Failure]) {
    assert!(failures.is_empty(), "{}", report(failures));
}

/// Asserts that `failures` is exactly one failure: its id, and a problem containing `part`.
fn assert_one(failures: &[Failure], id: &str, part: &str) {
    assert_eq!(failures.len(), 1, "expected one failure, got:\n{}", report(failures));
    assert_eq!(failures[0].check, "translations");
    assert_eq!(failures[0].id, id);
    assert!(failures[0].problem.contains(part), "problem `{}` lacks `{part}`", failures[0].problem);
}

const FULL: &str = "\
fixture-part = { $part } は `max_size` を使う。
fixture-count = { $count } 件。
";

const ORPHAN: &str = "\
fixture-part = { $part } は `max_size` を使う。
fixture-count = { $count } 件。
fixture-orphan = 余り。
";

#[test]
fn a_full_translation_passes() {
    assert_clean(&run(FULL));
}

#[test]
fn an_empty_stub_passes() {
    assert_clean(&run(""));
}

#[test]
fn english_resources_are_ignored() {
    let english = Resource { package: "omuzaki-fixture", locale: "en", source: "fixture-part = junk {\n" };
    assert_clean(&translations(&text(), &[english]));
}

#[test]
fn junk_fails_with_its_locale_and_line() {
    let failures = run("fixture-part = { $part } は `max_size` を使う。\nfixture-count = { $count\n");
    // The parser reports the line where it gives up: the placeable on line 2 is not closed before line 3.
    assert_one(&failures, "locales/ja/omuzaki-fixture.ftl", "(ja) does not parse at line 3");
}

#[test]
fn an_orphan_id_fails() {
    let failures = run(ORPHAN);
    assert_one(&failures, "fixture-orphan", "not defined in the English file");
}

#[test]
fn an_extra_placeable_fails() {
    let source = "fixture-part = { $part } は `max_size` を使う。\nfixture-count = { $count } 件 { $extra }。\n";
    let failures = run(source);
    assert_one(&failures, "fixture-count", "placeables the English lacks: `extra`");
}

#[test]
fn a_dropped_code_span_fails() {
    let source = "fixture-part = { $part } を使う。\nfixture-count = { $count } 件。\n";
    let failures = run(source);
    assert_one(&failures, "fixture-part", "code span `max_size`");
}

#[test]
fn a_changed_code_span_fails() {
    let source = "fixture-part = { $part } は `max-size` を使う。\nfixture-count = { $count } 件。\n";
    let failures = run(source);
    assert_one(&failures, "fixture-part", "code span `max_size`");
}
