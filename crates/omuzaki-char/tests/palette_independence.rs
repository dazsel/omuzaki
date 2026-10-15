//! Facts don't depend on the palette: every part kind linked into this binary (the mock kinds the kit characters are made
//! of) publishes the same facts whatever the palette is. For each of the four build-option combinations (Live and Offline,
//! each with and without `rig_export`), a kind's part is built with the character's own palette, then with five colour
//! alternatives that each differ from it in every entry, then with one extra unreferenced slot, and with the slots no
//! part names removed when the palette has any. The caching builder relies on it (see the `builder` module doc): it keys
//! a part that reads facts on the geometry of their publishers, not their colours.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char::builder::conformance::{Probes, check, check_all};
use omuzaki_char_api::fact::FactProbe;
use omuzaki_char_api::{BuildServices, CharacterDef, PartDef, PartId, PartKindId};
use omuzaki_char_mock::mock_character;
use serde_json::json;

/// The kit characters: the mock one, and the real body, head and hair (what the real accessories sit on).
fn kit_characters() -> Vec<CharacterDef> {
    let part = |kind: &str| PartDef {
        kind: PartKindId::new(kind).unwrap(),
        parent: None,
        enabled: true,
        layer: 0,
        attach: None,
        params: json!({}),
    };
    let mut real = mock_character().unwrap();
    real.parts.clear();
    for kind in ["body", "head", "hair"] {
        real.parts.insert(PartId::new(kind).unwrap(), part(kind));
    }
    vec![mock_character().unwrap(), real]
}

#[test]
fn every_registered_kind_publishes_the_same_facts_under_any_palette() {
    omuzaki_char::link();
    let report = check_all(&kit_characters(), None, &Probes::registered(), &BuildServices::default());
    assert!(report.violations.is_empty(), "facts depend on the palette:\n{}", report.violations.join("\n"));
    assert!(report.unprobed.is_empty(), "facts nothing compares (add a probe): {:?}", report.unprobed);
    assert!(report.unreached.is_empty(), "kinds the check cannot build: {:?}", report.unreached);
}

#[test]
fn the_check_reaches_the_kit_characters_parts_and_sees_what_they_publish() {
    omuzaki_char::link();
    let kinds = ["mock-body", "mock-head", "mock-hair", "mock-garment"];
    let report = check(&kit_characters()[0], Some(&kinds), &Probes::default(), &BuildServices::default());
    // With no probes every published fact is unprobed, which shows the body and head do publish and are looked at.
    assert!(report.unprobed.iter().any(|(kind, _)| kind == "mock-body"), "{:?}", report.unprobed);
    assert!(report.unprobed.iter().any(|(kind, _)| kind == "mock-head"), "{:?}", report.unprobed);
    assert!(report.violations.is_empty() && report.unreached.is_empty(), "{report:?}");
}

#[test]
fn a_check_with_no_base_character_reaches_nothing_and_says_so() {
    omuzaki_char::link();
    let asked = check_all(&[], Some(&["body", "head"]), &Probes::registered(), &BuildServices::default());
    let kinds: Vec<_> = asked.unreached.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(kinds, ["body", "head"]);
    assert!(asked.violations.is_empty() && asked.unprobed.is_empty());

    let everything = check_all(&[], None, &Probes::registered(), &BuildServices::default());
    assert_eq!(everything.unreached.len(), omuzaki_char_api::kinds().count(), "every registered kind");
    let order: Vec<_> = everything.unreached.iter().map(|(kind, _)| kind.clone()).collect();
    assert!(order.is_sorted(), "kinds are reported in sorted order: {order:?}");
}

// A fact and its probe, defined here: `Probes::registered()` compares it with no edit to `conformance.rs`.
omuzaki_char_api::fact! {
    /// A fact defined by a test.
    TestFact = "test-registered-probe", omuzaki_char_api::Last<u32>;
}
omuzaki_char_api::register_fact!(TestFact);
omuzaki_char_api::register_fact_probe!(TestFact);

#[test]
fn every_registered_fact_has_a_probe() {
    let missing = Probes::registered().unprobed_facts().expect("fact names are unique");
    assert!(missing.is_empty(), "facts with no `register_fact_probe!` beside their `register_fact!`: {missing:?}");
}

#[test]
fn no_probe_is_stray_or_doubled() {
    let problems = Probes::registered().probe_problems().expect("fact names are unique");
    assert!(problems.is_empty(), "probes that match no registered fact, or a fact with two: {problems:?}");
}

// A fact that shares its name with `TestFact` but is another type: `TestFact`'s probe does not cover it.
omuzaki_char_api::fact! {
    /// A second fact with the name of `TestFact`, built by hand below (it is never registered).
    NameClashFact = "test-registered-probe", omuzaki_char_api::Last<u32>;
}

#[test]
fn a_probe_made_for_another_fact_with_the_same_name_covers_nothing() {
    let wrong = Probes::default().fact::<NameClashFact>();
    let missing = wrong.unprobed_facts().expect("fact names are unique");
    assert!(missing.contains(&"test-registered-probe"), "the test fact counted as probed: {missing:?}");
    // The right type does cover it.
    let right = Probes::default().fact::<TestFact>();
    assert!(!right.unprobed_facts().expect("fact names are unique").contains(&"test-registered-probe"));
}

static STRAY: FactProbe = FactProbe::by_eq::<NameClashFact>();
static FIRST: FactProbe = FactProbe::by_eq::<TestFact>();
static SECOND: FactProbe = FactProbe::by_eq::<TestFact>();

#[test]
fn a_probe_for_no_registered_fact_and_a_second_probe_for_one_fact_are_problems() {
    let stray = Probes::from_probes([&STRAY]).probe_problems().expect("fact names are unique");
    assert_eq!(stray.len(), 1, "{stray:?}");
    assert!(stray[0].contains("test-registered-probe") && stray[0].contains("no registered fact"), "{stray:?}");
    let doubled = Probes::from_probes([&FIRST, &SECOND]).probe_problems().expect("fact names are unique");
    assert_eq!(doubled.len(), 1, "{doubled:?}");
    assert!(doubled[0].contains("more than one probe"), "{doubled:?}");
    let one = Probes::from_probes([&FIRST]).probe_problems().expect("fact names are unique");
    assert_eq!(one, Vec::<String>::new());
}

#[test]
fn the_gate_names_a_fact_without_a_probe() {
    let missing = Probes::default().unprobed_facts().expect("fact names are unique");
    for name in ["head", "face", "fit", "anchors", "colliders", "stature", "test-registered-probe"] {
        assert!(missing.contains(&name), "{name} not reported: {missing:?}");
    }
}
