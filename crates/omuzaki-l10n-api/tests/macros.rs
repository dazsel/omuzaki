//! A fixture crate for both macros: what `messages!` generates and what `register_locales!` submits.

use omuzaki_l10n_api::{Arg, ArgKind, Locale, MessageTable, OVERRIDES, Resource, SHIPPED, Text, Unit};

omuzaki_l10n_api::messages! {
    /// An op named a part the character doesn't have.
    pub part_not_found = "l10n-api-part-not-found" (part: Id, character: Id)
        => "{ $character } has no part { $part }.";
    /// After a bulk remove; shows a count, so the plural is Fluent's job.
    pub parts_removed = "l10n-api-parts-removed" (count: Int)
        => "{ $count -> [one] Removed one part. *[other] Removed { $count } parts. }";
    /// Every kind of argument, to check the generated signature.
    pub every_kind = "l10n-api-every-kind" (
        id: Id, why: Verbatim, n: Int, x: Num, len: Qty, nested: Text, items: List,
    ) => "{ $id } { $why } { $n } { $x } { $len } { $nested } { $items }";
    /// A key legend: not prose, so the pseudo-locale leaves it alone.
    #[verbatim]
    pub key_enter = "l10n-api-key-enter" () => "Enter";
}

omuzaki_l10n_api::register_locales!();

fn table() -> Option<&'static MessageTable> {
    inventory::iter::<MessageTable>.into_iter().find(|t| t.package == "omuzaki-l10n-api")
}

#[test]
fn a_generated_function_builds_the_text() {
    let text = part_not_found("hair-b", String::from("mira"));
    assert_eq!(text.id, "l10n-api-part-not-found");
    assert_eq!(text.arg("part"), Some(&Arg::Id("hair-b".into())));
    assert_eq!(text.arg("character"), Some(&Arg::Id("mira".into())));
    assert_eq!(text.args.iter().map(|(n, _)| &**n).collect::<Vec<_>>(), ["part", "character"]);
    assert_eq!(parts_removed(3), Text::new("l10n-api-parts-removed").with("count", Arg::Int(3)));
    assert_eq!(key_enter(), Text::new("l10n-api-key-enter"));
}

#[test]
fn every_argument_kind_has_its_parameter_type() {
    let text = every_kind("a", "b", 1, 2.5, (12.5, Unit::Cm), key_enter(), vec![Arg::Int(1)]);
    assert_eq!(text.arg("why"), Some(&Arg::Verbatim("b".into())));
    assert_eq!(text.arg("len"), Some(&Arg::Qty { value: 12.5, unit: Unit::Cm }));
    assert_eq!(text.arg("nested"), Some(&Arg::Text(Box::new(key_enter()))));
    assert_eq!(text.arg("items"), Some(&Arg::List(vec![Arg::Int(1)])));
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "must be finite")]
fn a_non_finite_number_parameter_panics_in_debug() {
    let _ = every_kind("a", "b", 1, f64::INFINITY, (1.0, Unit::Cm), key_enter(), vec![]);
}

#[test]
#[cfg(not(debug_assertions))]
fn a_non_finite_number_parameter_cannot_make_an_unstorable_text() {
    let text = every_kind("a", "b", 1, f64::INFINITY, (f64::NAN, Unit::Cm), key_enter(), vec![]);
    assert_eq!(text.arg("x"), Some(&Arg::Verbatim("inf".into())));
    assert!(serde_json::to_string(&text).is_ok());
}

#[test]
fn the_table_records_ids_english_notes_kinds_and_the_verbatim_marker() {
    let decls = table().expect("the fixture's table is registered").messages;
    assert_eq!(decls.len(), 4);
    let removed = &decls[1];
    assert_eq!(removed.id, "l10n-api-parts-removed");
    assert!(removed.english.contains("[one]"));
    assert_eq!(removed.note_text(), "After a bulk remove; shows a count, so the plural is Fluent's job.");
    assert_eq!(removed.args, [("count", ArgKind::Int)]);
    assert!(!removed.verbatim);
    let kinds: Vec<ArgKind> = decls[2].args.iter().map(|(_, k)| *k).collect();
    assert_eq!(
        kinds,
        [ArgKind::Id, ArgKind::Verbatim, ArgKind::Int, ArgKind::Num, ArgKind::Qty, ArgKind::Text, ArgKind::List]
    );
    assert!(decls[3].verbatim && decls[3].args.is_empty());
}

#[test]
fn locales_register_one_resource_per_language_and_match_the_shipped_list() {
    let mut langs: Vec<&str> =
        inventory::iter::<Resource>.into_iter().filter(|r| r.package == "omuzaki-l10n-api").map(|r| r.locale).collect();
    langs.sort_unstable();
    let mut expected: Vec<&str> =
        SHIPPED.iter().chain(OVERRIDES.iter().map(|(over, _)| over)).map(Locale::as_str).collect();
    expected.sort_unstable();
    assert_eq!(langs, expected, "register_locales! must list SHIPPED plus OVERRIDES");
}

#[test]
fn the_macros_record_their_registrations_for_the_link_gate() {
    let kinds: Vec<&str> = omuzaki_core::link::registered()
        .into_iter()
        .filter(|r| r.package == "omuzaki-l10n-api")
        .map(|r| r.kind)
        .collect();
    assert!(kinds.contains(&"messages") && kinds.contains(&"locales"), "{kinds:?}");
}
