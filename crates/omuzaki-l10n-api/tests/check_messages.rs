//! The declared-messages check (`check::messages`): a clean table passes, and each planted fault fails exactly one rule.
//! The fixtures are `MessageDecl` literals in package `omuzaki-fixture`.

use omuzaki_l10n_api::check::{Failure, messages, prefixes, report};
use omuzaki_l10n_api::extract::PackageText;
use omuzaki_l10n_api::{ArgKind, MessageDecl};

/// A message declaration with the given id, English, translator note and arguments.
fn decl(
    id: &'static str,
    english: &'static str,
    note: &'static str,
    args: &'static [(&'static str, ArgKind)],
) -> MessageDecl {
    MessageDecl { id, english, note, args, verbatim: false }
}

/// Runs the check over the given messages of package `omuzaki-fixture`.
fn run(messages_of: &[MessageDecl]) -> Vec<Failure> {
    messages(&PackageText::new("omuzaki-fixture").with_messages(messages_of))
}

/// Asserts that `failures` is exactly one failure: its check, its id, and a problem containing `part`.
fn assert_one(failures: &[Failure], id: &str, part: &str) {
    assert_eq!(failures.len(), 1, "expected one failure, got:\n{}", report(failures));
    assert_eq!(failures[0].check, "messages");
    assert_eq!(failures[0].id, id);
    assert!(failures[0].problem.contains(part), "problem `{}` lacks `{part}`", failures[0].problem);
}

#[test]
fn a_clean_table_has_no_failures() {
    let failures = run(&[
        decl(
            "fixture-part-missing",
            "{ $part } is missing.",
            " Shown when a part is missing.",
            &[("part", ArgKind::Id)],
        ),
        decl("fixture-attr", "Count.\n    .title = { $n } items", " A count, with a title.", &[("n", ArgKind::Int)]),
        decl(
            "fixture-select",
            "{ $n ->\n    [one] One\n   *[other] Many\n}",
            " Plural by count.",
            &[("n", ArgKind::Int)],
        ),
    ]);
    assert!(failures.is_empty(), "{}", report(&failures));
}

#[test]
fn a_message_without_a_note_fails() {
    let failures = run(&[decl("fixture-x", "Plain.", "", &[])]);
    assert_one(&failures, "fixture-x", "translator note");
}

#[test]
fn a_message_with_the_wrong_prefix_fails() {
    let failures = run(&[decl("char-missing", "Plain.", " A note.", &[])]);
    assert_one(&failures, "char-missing", "must start with `fixture-`");
}

#[test]
fn a_message_with_a_reserved_op_id_fails() {
    let failures = run(&[decl("op-face-set", "Plain.", " A note.", &[])]);
    assert_one(&failures, "op-face-set", "reserved prefix");
}

#[test]
fn english_that_does_not_parse_fails() {
    let failures = run(&[decl("fixture-broken", "{ $part", " A note.", &[("part", ArgKind::Id)])]);
    assert_one(&failures, "fixture-broken", "does not parse");
}

#[test]
fn a_placeable_with_no_declared_argument_fails() {
    let failures = run(&[decl("fixture-extra", "{ $part } and { $other }.", " A note.", &[("part", ArgKind::Id)])]);
    assert_one(&failures, "fixture-extra", "extra `other`");
}

#[test]
fn a_declared_argument_never_used_fails() {
    let failures = run(&[decl(
        "fixture-unused",
        "{ $part } is here.",
        " A note.",
        &[("part", ArgKind::Id), ("count", ArgKind::Int)],
    )]);
    assert_one(&failures, "fixture-unused", "missing `count`");
}

#[test]
fn a_selector_on_an_undeclared_variable_fails() {
    let failures = run(&[decl(
        "fixture-select-extra",
        "{ $count ->\n    [one] One\n   *[other] Many\n}",
        " Plural by count.",
        &[],
    )]);
    assert_one(&failures, "fixture-select-extra", "extra `count`");
}

#[test]
fn a_duplicate_id_fails_once() {
    let failures =
        run(&[decl("fixture-dup", "First.", " A note.", &[]), decl("fixture-dup", "Second.", " A note.", &[])]);
    assert_one(&failures, "fixture-dup", "declared 2 times");
}

#[test]
fn the_prefixes_of_a_package() {
    assert_eq!(prefixes("omuzaki-ui"), vec!["ds-", "ui-"]);
    assert_eq!(prefixes("omuzaki-char"), vec!["char-"]);
    assert_eq!(prefixes("omuzaki-io-ops"), vec!["io-ops-"]);
}
