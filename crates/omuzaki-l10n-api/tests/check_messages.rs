//! The `messages` check (`check::messages`): a clean table passes, and each planted fault fails exactly one rule.

use omuzaki_l10n_api::check::{Failure, messages, prefixes, report};
use omuzaki_l10n_api::extract::PackageText;
use omuzaki_l10n_api::{ArgKind, MessageDecl};

/// A message of package `omuzaki-fixture` with a note, no arguments and the given id and English.
fn decl(id: &'static str, english: &'static str) -> MessageDecl {
    MessageDecl { id, english, note: " A note.", args: &[], verbatim: false }
}

/// Runs the check over the given messages of `omuzaki-fixture`.
fn run(decls: &[MessageDecl]) -> Vec<Failure> {
    messages(&PackageText::new("omuzaki-fixture").with_messages(decls))
}

/// Asserts that `failures` is exactly one failure of `id` whose problem contains `part`.
fn assert_one(failures: &[Failure], id: &str, part: &str) {
    assert_eq!(failures.len(), 1, "expected one failure, got:\n{}", report(failures));
    assert_eq!(failures[0].check, "messages");
    assert_eq!(failures[0].id, id);
    assert!(failures[0].problem.contains(part), "problem `{}` lacks `{part}`", failures[0].problem);
}

#[test]
fn a_clean_table_has_no_failures() {
    let failures = run(&[
        decl("fixture-plain", "Hello."),
        MessageDecl {
            id: "fixture-count",
            english: "{ $count ->\n    [one] One part.\n   *[other] { $count } parts.\n}",
            note: " A count.",
            args: &[("count", ArgKind::Int)],
            verbatim: false,
        },
        MessageDecl {
            id: "fixture-attr",
            english: "Value.\n    .tip = Tip for { $part }.",
            note: " An attribute.",
            args: &[("part", ArgKind::Id)],
            verbatim: false,
        },
    ]);
    assert!(failures.is_empty(), "{}", report(&failures));
}

#[test]
fn a_message_without_a_note_fails() {
    let mut bare = decl("fixture-bare", "Hello.");
    bare.note = " \n ";
    assert_one(&run(&[bare]), "fixture-bare", "no translator note");
}

#[test]
fn another_crates_prefix_fails() {
    assert_one(&run(&[decl("char-hello", "Hello.")]), "char-hello", "must start with `fixture-`");
}

#[test]
fn a_reserved_id_fails_once() {
    assert_one(&run(&[decl("op-hello", "Hello.")]), "op-hello", "reserved");
    assert_one(&run(&[decl("kind-hello", "Hello.")]), "kind-hello", "reserved");
}

#[test]
fn english_that_does_not_parse_fails() {
    assert_one(&run(&[decl("fixture-broken", "{ $x")]), "fixture-broken", "does not parse");
}

#[test]
fn english_that_is_more_than_one_message_fails() {
    assert_one(&run(&[decl("fixture-two", "One.\nother = Two.")]), "fixture-two", "exactly one message");
}

#[test]
fn a_placeable_without_an_argument_fails() {
    assert_one(&run(&[decl("fixture-open", "Hello { $who }.")]), "fixture-open", "undeclared $who");
}

#[test]
fn a_declared_argument_never_used_fails() {
    let unused = MessageDecl { args: &[("who", ArgKind::Id)], ..decl("fixture-unused", "Hello.") };
    assert_one(&run(&[unused]), "fixture-unused", "never uses declared $who");
}

#[test]
fn a_selector_on_an_undeclared_variable_fails() {
    let selector = decl("fixture-select", "{ $n ->\n    [one] One.\n   *[other] Many.\n}");
    assert_one(&run(&[selector]), "fixture-select", "undeclared $n");
}

#[test]
fn a_variable_in_an_attribute_counts() {
    let attribute = decl("fixture-attr", "Value.\n    .tip = Tip for { $part }.");
    assert_one(&run(&[attribute]), "fixture-attr", "undeclared $part");
}

#[test]
fn a_variable_deep_in_a_variant_counts() {
    let deep = decl("fixture-deep", "{ $n ->\n    [one] { $a }\n   *[other] { NUMBER($b) }\n}");
    let deep = MessageDecl { args: &[("n", ArgKind::Int)], ..deep };
    assert_one(&run(&[deep]), "fixture-deep", "undeclared $a, $b");
}

#[test]
fn a_duplicate_id_fails_once() {
    let same = decl("fixture-same", "Hello.");
    assert_one(&run(&[same, same, same]), "fixture-same", "more than once");
}

#[test]
fn prefixes_follow_the_package_name() {
    assert_eq!(prefixes("omuzaki-char"), ["char-"]);
    assert_eq!(prefixes("omuzaki-io-ops"), ["io-ops-"]);
    assert_eq!(prefixes("omuzaki-ui"), ["ds-", "ui-"]);
}

#[test]
fn the_ui_package_accepts_both_prefixes() {
    let table = [decl("ds-button", "Press."), decl("ui-panel", "Panel."), decl("char-x", "X.")];
    let failures = messages(&PackageText::new("omuzaki-ui").with_messages(&table));
    assert_one(&failures, "char-x", "`ds-` or `ui-`");
}
