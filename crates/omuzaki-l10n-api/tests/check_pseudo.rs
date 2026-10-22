//! The `pseudo` check (`check::pseudo`): clean sets pass, and each planted fault fails exactly one rule.

use omuzaki_l10n_api::check::{Failure, pseudo, report, samples};
use omuzaki_l10n_api::extract::PackageText;
use omuzaki_l10n_api::{Arg, ArgKind, Catalog, Locale, MessageDecl, Resource, Unit};

const PLURAL: &str = "{ $count ->\n    [one] Removed one part.\n   *[other] Removed { $count } parts.\n}";

fn decl(id: &'static str, english: &'static str, args: &'static [(&'static str, ArgKind)]) -> MessageDecl {
    MessageDecl { id, english, note: " A note.", args, verbatim: false }
}

/// Runs the check with catalogs built from the English of `decls`.
fn run(decls: &[MessageDecl]) -> Vec<Failure> {
    let source = decls.iter().map(|d| [d.id, " = ", d.english, "\n"].concat()).collect::<String>();
    let source: &'static str = Box::leak(source.into_boxed_str());
    let resources = [Resource { package: "omuzaki-fixture", locale: "en", source }];
    let pseudo_catalog = Catalog::from_sources(Locale::PSEUDO, &[], &resources);
    let english = Catalog::from_sources(Locale::EN, &[], &resources);
    pseudo(&PackageText::new("omuzaki-fixture").with_messages(decls), &pseudo_catalog, &english)
}

fn assert_one(failures: &[Failure], id: &str, part: &str) {
    assert_eq!(failures.len(), 1, "expected one failure, got:\n{}", report(failures));
    assert_eq!(failures[0].check, "pseudo");
    assert_eq!(failures[0].id, id);
    assert!(failures[0].problem.contains(part), "problem `{}` lacks `{part}`", failures[0].problem);
}

#[test]
fn a_clean_set_with_a_plural_passes_for_1_2_and_5() {
    let failures = run(&[
        decl("fixture-plural", PLURAL, &[("count", ArgKind::Int)]),
        decl("fixture-hello", "Hello { $who }.", &[("who", ArgKind::Id)]),
        decl("fixture-size", "Is { $len } by { $x }.", &[("len", ArgKind::Qty), ("x", ArgKind::Num)]),
        decl("fixture-list", "Has { $items } and { $why }.", &[("items", ArgKind::List), ("why", ArgKind::Verbatim)]),
    ]);
    assert!(failures.is_empty(), "{}", report(&failures));
}

#[test]
fn a_nested_text_uses_the_first_argument_free_message() {
    let failures = run(&[
        decl("fixture-title", "Title", &[]),
        decl("fixture-wrap", "About { $inner }.", &[("inner", ArgKind::Text)]),
    ]);
    assert!(failures.is_empty(), "{}", report(&failures));
    // Without a message to nest, the argument is an id.
    let failures = run(&[decl("fixture-wrap", "About { $inner }.", &[("inner", ArgKind::Text)])]);
    assert!(failures.is_empty(), "{}", report(&failures));
}

#[test]
fn a_verbatim_message_passes() {
    let legend = MessageDecl { verbatim: true, ..decl("fixture-legend", "Ctrl+S", &[]) };
    let failures = run(&[legend]);
    assert!(failures.is_empty(), "{}", report(&failures));
}

#[test]
fn an_undeclared_variable_is_a_formatting_error() {
    let failures = run(&[decl("fixture-open", "Hello { $who }.", &[])]);
    assert_one(&failures, "fixture-open", "does not format");
}

#[test]
fn ascii_text_the_transform_cannot_reach_fails() {
    let failures = run(&[decl("fixture-literal", "{ \"Save\" }", &[])]);
    assert_one(&failures, "fixture-literal", "plain ASCII");
}

#[test]
fn a_message_that_renders_empty_fails() {
    let failures = run(&[decl("fixture-empty", "{ \"\" }", &[])]);
    assert_one(&failures, "fixture-empty", "renders no text");
}

#[test]
fn a_message_missing_from_the_resources_fails() {
    let source = "other = Other.\n";
    let resources = [Resource { package: "omuzaki-fixture", locale: "en", source }];
    let pseudo_catalog = Catalog::from_sources(Locale::PSEUDO, &[], &resources);
    let english = Catalog::from_sources(Locale::EN, &[], &resources);
    let text = PackageText::new("omuzaki-fixture").with_messages(&[decl("fixture-lost", "Lost.", &[])]);
    assert_one(&pseudo(&text, &pseudo_catalog, &english), "fixture-lost", "not in the resources");
}

#[test]
fn a_message_that_does_not_grow_fails() {
    // The English catalog sees longer text than the pseudo one, as when the transform skipped the message.
    let short = [Resource { package: "omuzaki-fixture", locale: "en", source: "fixture-x = Hi\n" }];
    let long = [Resource { package: "omuzaki-fixture", locale: "en", source: "fixture-x = Hi there, long text\n" }];
    let pseudo_catalog = Catalog::from_sources(Locale::PSEUDO, &[], &short);
    let english = Catalog::from_sources(Locale::EN, &[], &long);
    let text = PackageText::new("omuzaki-fixture").with_messages(&[decl("fixture-x", "Hi", &[])]);
    assert_one(&pseudo(&text, &pseudo_catalog, &english), "fixture-x", "does not grow");
}

#[test]
fn samples_cover_each_kind() {
    assert_eq!(samples(ArgKind::Id), [Arg::Id("sample-id".into())]);
    assert_eq!(samples(ArgKind::Int), [Arg::Int(1), Arg::Int(2), Arg::Int(5)]);
    assert_eq!(samples(ArgKind::Num), [Arg::num(1.5).expect("finite")]);
    assert_eq!(samples(ArgKind::Qty), [Arg::qty(12.5, Unit::Cm).expect("finite")]);
    assert_eq!(samples(ArgKind::Verbatim), [Arg::Verbatim("Sample".into())]);
    assert!(matches!(samples(ArgKind::List).as_slice(), [Arg::List(items)] if items.len() == 2));
    assert_eq!(samples(ArgKind::Text).len(), 1);
}
