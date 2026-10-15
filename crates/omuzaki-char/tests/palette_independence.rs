//! Facts don't depend on the palette: every part kind linked into this binary (the mock kinds the kit characters are made
//! of) publishes the same facts whatever the palette is. For each of the four build-option combinations (Live and Offline,
//! each with and without `rig_export`), a kind's part is built with the character's own palette, then with five colour
//! alternatives that each differ from it in every entry, then with one extra unreferenced slot, and with the slots no
//! part names removed when the palette has any. The caching builder relies on it (see the `builder` module doc): it keys
//! a part that reads facts on the geometry of their publishers, not their colours.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char::builder::conformance::{Probes, check, check_all};
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
    let report = check_all(&kit_characters(), None, &Probes::built_in(), &BuildServices::default());
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
    let asked = check_all(&[], Some(&["body", "head"]), &Probes::built_in(), &BuildServices::default());
    let kinds: Vec<_> = asked.unreached.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(kinds, ["body", "head"]);
    assert!(asked.violations.is_empty() && asked.unprobed.is_empty());

    let everything = check_all(&[], None, &Probes::built_in(), &BuildServices::default());
    assert_eq!(everything.unreached.len(), omuzaki_char_api::kinds().count(), "every registered kind");
}
