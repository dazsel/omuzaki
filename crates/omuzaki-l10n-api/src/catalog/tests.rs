//! The catalog against fixture resources: every step of the fallback chain, plurals, numbers, lists, the
//! pseudo-locale and the missing-text report.

use std::sync::Arc;

use super::Catalog;
use crate::{Arg, Locale, Missing, MissingReason, Resource, Text, Unit, ids};

crate::messages! {
    /// A key legend: not prose, so the pseudo-locale leaves it alone.
    #[verbatim]
    pub key_enter = "cat-key-enter" () => "Enter";
}

const EN: &str = "
hello = Hello
greet = Hi { $name }
count = { $count ->
    [one] One thing
   *[other] { $count } things
}
only-en = English only
holder = Holding { $part }
part-hair = Hair
broken = Value { $nope }
nums = { $n } and { $x }
items = Items: { $items }
size = Size { $len }
cat-key-enter = Enter
op-face-set_profile = Set the profile
    .description = Changes the shape of the face in profile.
kind-twin_tails = Twin tails
    .summary = Two tails of hair.
op-face-set_profile--blend = Blend
    .description = How much of the new profile to apply.
";

const ES_419: &str = "
hello = Hola
greet = Hola { $name }
count = { $count ->
    [one] Una cosa
   *[other] { $count } cosas
}
holder = Con { $part }
part-hair = Pelo
op-face-set_profile = Fijar el perfil
";

const ES_ES_ONE_ENTRY: &str = "hello = Hola, vosotros";

const JA: &str = "
hello = こんにちは
count = { $count ->
   *[other] { $count }個
}
holder = { $part }を持つ
part-hair = 髪
items = 項目: { $items }
";

const PT_BR: &str = "nums = { $n } e { $x }\nfixed = { NUMBER($n, minimumFractionDigits: 2) }";

fn resource(locale: &'static str, source: &'static str) -> Resource {
    Resource { package: "fixture", locale, source }
}

fn fixtures() -> Vec<Resource> {
    vec![
        resource("en", EN),
        resource("es-419", ES_419),
        resource("es-ES", ES_ES_ONE_ENTRY),
        resource("ja", JA),
        resource("pt-BR", PT_BR),
    ]
}

fn catalog(tag: &str) -> Catalog {
    let locale = Locale::new(tag).unwrap();
    let parents = if tag == "es-ES" { vec![Locale::ES_419] } else { Vec::new() };
    Catalog::from_sources(locale, &parents, &fixtures())
}

fn reasons(catalog: &Catalog, id: &str) -> Vec<MissingReason> {
    catalog.missing().into_iter().filter(|m| m.id == id).map(|m| m.reason).collect()
}

#[test]
fn step_one_the_translation_and_step_two_english() {
    let es = catalog("es-419");
    assert_eq!(es.render(&Text::new("hello")), "Hola");
    assert_eq!(es.render(&Text::new("greet").with("name", Arg::Id("Mira".into()))), "Hola Mira");
    // `only-en` has no Spanish: English is shown and the report says so, once, however often it is asked.
    for _ in 0..3 {
        assert_eq!(es.render(&Text::new("only-en")), "English only");
    }
    assert_eq!(
        es.missing(),
        [Missing { locale: Locale::ES_419, id: "only-en".into(), reason: MissingReason::Untranslated }]
    );
}

#[test]
fn english_has_nothing_to_fall_back_from() {
    let en = catalog("en");
    assert_eq!(en.render(&Text::new("only-en")), "English only");
    assert!(en.missing().is_empty());
}

#[test]
fn step_three_the_english_stored_with_the_record() {
    let es = catalog("es-419");
    let old = Text::new("gone-id").with("part", Arg::Id("hair-b".into()));
    assert_eq!(es.render_stored(&old, "No part hair-b."), "No part hair-b.");
    assert_eq!(
        es.missing(),
        [Missing { locale: Locale::ES_419, id: "gone-id".into(), reason: MissingReason::UnknownId }]
    );
    // A known id ignores the stored English.
    assert_eq!(es.render_stored(&Text::new("hello"), "stale"), "Hola");
}

#[test]
fn step_four_the_last_resort_is_never_blank() {
    let es = catalog("es-419");
    let text = Text::new("char-part-not-found")
        .with("part", Arg::Id("hair-b".into()))
        .with("character", Arg::Id("mira".into()));
    assert_eq!(es.render(&text), "⟦char-part-not-found: part=hair-b, character=mira⟧");
    assert_eq!(es.render(&Text::new("x")), "⟦x⟧");
}

#[test]
fn a_nested_text_falls_back_on_its_own() {
    let es = catalog("es-419");
    let known = Text::new("holder").with("part", Arg::Text(Box::new(Text::new("part-hair"))));
    assert_eq!(es.render(&known), "Con Pelo");
    let unknown = Text::new("holder").with("part", Arg::Text(Box::new(Text::new("part-unknown"))));
    assert_eq!(es.render(&unknown), "Con ⟦part-unknown⟧");
    let outer_unknown = Text::new("holder-gone").with("part", Arg::Text(Box::new(Text::new("part-hair"))));
    assert_eq!(es.render(&outer_unknown), "⟦holder-gone: part=Pelo⟧", "last-resort arguments are rendered too");
    let nested_unknown = Text::new("holder-gone").with("part", Arg::Text(Box::new(Text::new("part-gone"))));
    assert_eq!(es.render(&nested_unknown), "⟦holder-gone: part=⟦part-gone⟧⟧");
    assert!(
        es.missing().iter().any(|m| m.id == "part-gone" && m.reason == MissingReason::UnknownId),
        "nested ids are reported"
    );
}

#[test]
fn a_regional_override_chain_is_override_then_parent_then_english() {
    let es_es = catalog("es-ES");
    assert_eq!(es_es.render(&Text::new("hello")), "Hola, vosotros", "the override's own entry");
    assert_eq!(es_es.render(&Text::new("part-hair")), "Pelo", "the parent's entry");
    assert_eq!(es_es.render(&Text::new("only-en")), "English only", "English last");
    // The override is sparse: what the parent has is not missing; only what nobody translated is.
    assert_eq!(
        es_es.missing(),
        [Missing { locale: Locale::new("es-ES").unwrap(), id: "only-en".into(), reason: MissingReason::Untranslated }]
    );
}

#[test]
fn plurals_follow_the_locale() {
    let count = |n| Text::new("count").with("count", Arg::Int(n));
    let en = catalog("en");
    assert_eq!(en.render(&count(1)), "One thing");
    assert_eq!(en.render(&count(5)), "5 things");
    let ja = catalog("ja");
    assert_eq!(ja.render(&count(1)), "1個");
    assert_eq!(ja.render(&count(5)), "5個");
    assert_eq!(catalog("es-419").render(&count(1)), "Una cosa");
}

#[test]
fn numbers_use_the_locales_digits_and_marks() {
    let nums = |n, x| Text::new("nums").with("n", Arg::Int(n)).with("x", Arg::num(x).unwrap());
    assert_eq!(catalog("en").render(&nums(1_234_567, 1234.5)), "1,234,567 and 1,234.5");
    assert_eq!(catalog("pt-BR").render(&nums(1_234_567, 1234.5)), "1.234.567 e 1.234,5");
    assert_eq!(catalog("en").render(&nums(i64::MAX, 0.25)), "9,223,372,036,854,775,807 and 0.25");
}

#[test]
fn quantities_are_the_number_and_the_unit_symbol() {
    let size = |value, unit| Text::new("size").with("len", Arg::qty(value, unit).unwrap());
    assert_eq!(catalog("en").render(&size(12.5, Unit::Cm)), "Size 12.5 cm");
    assert_eq!(catalog("pt-BR").render(&size(12.5, Unit::Cm)), "Size 12,5 cm");
    assert_eq!(catalog("en").render(&size(30.0, Unit::Deg)), "Size 30°");
    assert_eq!(catalog("en").render(&size(40.0, Unit::Percent)), "Size 40%");
}

#[test]
fn lists_use_the_locales_pattern() {
    let items = |n: usize| {
        let list = ["a", "b", "c", "d"][..n].iter().map(|s| Arg::Id((*s).into())).collect();
        Text::new("items").with("items", Arg::List(list))
    };
    let en = catalog("en");
    assert_eq!(en.render(&items(0)), "Items: ");
    assert_eq!(en.render(&items(1)), "Items: a");
    assert_eq!(en.render(&items(2)), "Items: a and b");
    assert_eq!(en.render(&items(3)), "Items: a, b and c");
    assert_eq!(en.render(&items(4)), "Items: a, b, c and d");
    assert_eq!(catalog("ja").render(&items(3)), "項目: a、b、c");
    assert_eq!(
        catalog("es-419").render(&items(3)),
        "Items: a, b y c",
        "no Spanish `items`: English text, Spanish list"
    );
    assert_eq!(catalog("pt-BR").render(&items(2)), "Items: a e b");
    assert_eq!(catalog("ko").render(&items(2)), "Items: a 및 b");
    assert_eq!(catalog("zh-Hans").render(&items(3)), "Items: a、b和c");
}

#[test]
fn a_formatting_error_keeps_fluents_output_and_is_recorded_once() {
    let en = catalog("en");
    for _ in 0..2 {
        assert_eq!(en.render(&Text::new("broken")), "Value {$nope}");
    }
    let found = reasons(&en, "broken");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(matches!(&found[0], MissingReason::Format(why) if why.contains("nope")), "{found:?}");
}

#[test]
fn a_duplicate_message_across_crates_is_reported() {
    let sources = [resource("en", "same = One"), Resource { package: "other", locale: "en", source: "same = Two" }];
    let en = Catalog::from_sources(Locale::EN, &[], &sources);
    assert_eq!(en.render(&Text::new("same")), "One", "the first definition stays");
    assert!(matches!(reasons(&en, "same").as_slice(), [MissingReason::Format(_)]));
}

#[test]
fn registered_text_comes_from_derived_ids() {
    let es = catalog("es-419");
    assert_eq!(es.op_summary("face.set_profile").as_deref(), Some("Fijar el perfil"));
    assert_eq!(es.op_description("face.set_profile").as_deref(), Some("Changes the shape of the face in profile."));
    assert_eq!(es.kind_title("twin_tails").as_deref(), Some("Twin tails"));
    assert_eq!(es.kind_summary("twin_tails").as_deref(), Some("Two tails of hair."));
    let owner = ids::op("face.set_profile");
    assert_eq!(es.arg_title(&owner, &["blend"]).as_deref(), Some("Blend"));
    assert_eq!(es.arg_description(&owner, &["blend"]).as_deref(), Some("How much of the new profile to apply."));
    for none in [
        es.op_summary("face.nope"),
        es.kind_title("nope"),
        es.arg_title(&owner, &["nope"]),
        es.arg_description(&owner, &["nope"]),
    ] {
        assert_eq!(none, None);
    }
    let missing = es.missing();
    assert!(missing.iter().any(|m| m.id == "op-face-set_profile.description"), "{missing:?}");
    assert!(missing.iter().all(|m| !m.id.contains("nope")), "an absent id is not a fallback: {missing:?}");
}

#[test]
fn static_text_is_cached_and_shared() {
    let es = catalog("es-419");
    let first = es.op_summary("face.set_profile").unwrap();
    let second = es.op_summary("face.set_profile").unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    let owner = ids::op("face.set_profile");
    assert!(Arc::ptr_eq(&es.arg_title(&owner, &["blend"]).unwrap(), &es.arg_title(&owner, &["blend"]).unwrap()));
    // A clone shares the cache and the report.
    let clone = es.clone();
    assert!(Arc::ptr_eq(&first, &clone.op_summary("face.set_profile").unwrap()));
}

#[test]
fn the_pseudo_locale_is_accented_english_with_no_plain_letters() {
    let pseudo = catalog("en-XA");
    assert_eq!(pseudo.locale(), &Locale::PSEUDO);
    let greeting = pseudo.render(&Text::new("greet").with("name", Arg::Id("Mira".into())));
    let (before, after) = greeting.split_once("Mira").expect("an Id value passes through");
    for part in [before, after] {
        assert!(!part.chars().any(|c| c.is_ascii_alphabetic()), "{greeting}");
    }
    assert!(greeting.starts_with('[') && greeting.ends_with(']'), "{greeting}");

    let verbatim = pseudo.render(&Text::new("holder").with("part", Arg::Verbatim("Ok".into())));
    assert!(
        verbatim.contains("Ok") && !verbatim.replace("Ok", "").chars().any(|c| c.is_ascii_alphabetic()),
        "{verbatim}"
    );

    // Nested text is framed once, by the outer text.
    let nested = pseudo.render(&Text::new("holder").with("part", Arg::Text(Box::new(Text::new("part-hair")))));
    assert_eq!(nested.matches('[').count(), 1, "{nested}");
    assert!(!nested.chars().any(|c| c.is_ascii_alphabetic()), "{nested}");

    let summary = pseudo.op_summary("face.set_profile").unwrap();
    assert!(!summary.chars().any(|c| c.is_ascii_alphabetic()), "{summary}");
    assert!(summary.chars().count() > "Set the profile".chars().count());

    // Plurals still work, and a key legend marked `#[verbatim]` is left alone.
    let count = pseudo.render(&Text::new("count").with("count", Arg::Int(1)));
    assert!(count.contains('['), "{count}");
    assert_eq!(pseudo.render(&key_enter()), "Enter");
    assert!(pseudo.missing().is_empty(), "{:?}", pseudo.missing());
}

#[test]
fn the_registered_english_catalog_is_shared() {
    assert!(std::ptr::eq(Catalog::english(), Catalog::english()));
    assert_eq!(Catalog::english().locale(), &Locale::EN);
    // This crate's registered files are empty, so nothing is known: the last resort, not a panic.
    assert_eq!(Catalog::new(Locale::JA).render(&Text::new("hello")), "⟦hello⟧");
}

#[test]
fn explicit_number_calls_work_like_bare_placeables() {
    let fixed = Text::new("fixed").with("n", Arg::Int(1234));
    assert_eq!(catalog("pt-BR").render(&fixed), "1.234,00");
    let en =
        Catalog::from_sources(Locale::EN, &[], &[resource("en", "fixed = { NUMBER($n, minimumFractionDigits: 2) }")]);
    assert_eq!(en.render(&fixed), "1,234.00");
    assert!(en.missing().is_empty(), "{:?}", en.missing());
}

#[test]
fn a_catalog_is_shareable_between_threads() {
    fn assert_send_sync<T: Send + Sync + Clone>() {}
    assert_send_sync::<Catalog>();
}

#[test]
fn number_options_apply_to_rounded_values() {
    let source = "round = { NUMBER($n, maximumFractionDigits: 2) }\nwhole = { NUMBER($n, minimumFractionDigits: 0) }";
    let en = Catalog::from_sources(Locale::EN, &[], &[resource("en", source)]);
    let render = |id: &str, arg: Arg| en.render(&Text::new(id.to_owned()).with("n", arg));
    assert_eq!(render("round", Arg::Num(1234.567)), "1,234.57");
    assert_eq!(render("whole", Arg::Int(1234)), "1,234");
    assert!(en.missing().is_empty(), "{:?}", en.missing());
}

#[test]
fn exact_integers_keep_every_digit_through_number_options() {
    let source = "\
plain = { NUMBER($n) }
bare = { $n }
padded = { NUMBER($n, minimumFractionDigits: 2) }
ungrouped = { NUMBER($n, useGrouping: \"false\") }
both = { NUMBER($n, minimumFractionDigits: 2, useGrouping: \"false\") }
wide = { NUMBER($n, minimumIntegerDigits: 22) }
";
    let sources = [resource("en", source), resource("pt-BR", source)];
    let cases = [
        // (locale, id, value, expected)
        ("en", "plain", i64::MAX, "9,223,372,036,854,775,807"),
        ("en", "bare", i64::MAX, "9,223,372,036,854,775,807"),
        ("en", "plain", i64::MIN, "-9,223,372,036,854,775,808"),
        ("en", "padded", i64::MAX, "9,223,372,036,854,775,807.00"),
        ("en", "padded", i64::MIN, "-9,223,372,036,854,775,808.00"),
        ("en", "ungrouped", i64::MAX, "9223372036854775807"),
        ("en", "ungrouped", i64::MIN, "-9223372036854775808"),
        ("en", "both", i64::MAX, "9223372036854775807.00"),
        ("en", "both", i64::MIN, "-9223372036854775808.00"),
        ("en", "wide", i64::MAX, "0,009,223,372,036,854,775,807"),
        ("pt-BR", "plain", i64::MAX, "9.223.372.036.854.775.807"),
        ("pt-BR", "padded", i64::MIN, "-9.223.372.036.854.775.808,00"),
        ("pt-BR", "ungrouped", i64::MAX, "9223372036854775807"),
        ("pt-BR", "both", i64::MIN, "-9223372036854775808,00"),
        // Inside the exact range the ordinary number path gives the same answers.
        ("en", "both", -1234, "-1234.00"),
        ("pt-BR", "padded", 1234, "1.234,00"),
    ];
    for (tag, id, value, expected) in cases {
        let catalog = Catalog::from_sources(Locale::new(tag).unwrap(), &[], &sources);
        let text = Text::new(id).with("n", Arg::Int(value));
        assert_eq!(catalog.render(&text), expected, "{tag} {id} {value}");
        assert!(catalog.missing().is_empty(), "{tag} {id}: {:?}", catalog.missing());
    }
}
