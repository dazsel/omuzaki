//! Facts don't depend on the palette: every part kind linked into this binary (the mock kinds the kit characters are made
//! of) publishes the same facts whatever the palette is. For each of the four build-option combinations (Live and Offline,
//! each with and without `rig_export`), a kind's part is built with the character's own palette, then with five colour
//! alternatives that each differ from it in every entry, with one extra unreferenced slot, with the slots no part names
//! removed (when the palette has any), and with each slot a part names removed on its own. At the base seed that is one
//! build plus up to 7 + k alternatives (k referenced slots) per option set; at each of four more seeds only the base and
//! the first colour alternative are built. The caching builder relies on it (see the `builder` module doc): it keys a
//! part that reads facts on the geometry of their publishers, not their colours.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char::builder::conformance::{
    Probes, Sweep, check, check_all, check_all_with, check_colours_through_parameters, check_with,
};
use omuzaki_char::outfit::common::LAYER_SHIRT;
use omuzaki_char::outfit::skirt::LAYER_SKIRT;
use omuzaki_char_api::fact::FactProbe;
use omuzaki_char_api::{BuildServices, CharacterDef, PartDef, PartId, PartKindId};
use omuzaki_char_api::{HeadFact, Publishes};
use omuzaki_char_mock::mock_character;
use omuzaki_face_mock::MockRasterizer;
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

/// The real kit character dressed: a shirt with a collar and a tie on its collar ring, and a skirt (the kinds that hang
/// on a shirt or go round the legs, which the plain characters can't build).
fn dressed() -> CharacterDef {
    let mut def = kit_characters().pop().unwrap();
    let on_collar = json!({ "to": "anchor", "part": "shirt", "anchor": "collar_ring" });
    for (id, layer, attach) in [
        ("shirt", LAYER_SHIRT, None),
        ("collar", LAYER_SHIRT + 1, Some(on_collar.clone())),
        ("tie", LAYER_SHIRT + 2, Some(on_collar)),
        ("skirt", LAYER_SKIRT, None),
    ] {
        let part = PartDef {
            kind: PartKindId::new(id).unwrap(),
            parent: None,
            enabled: true,
            layer,
            attach: attach.map(|a| serde_json::from_value(a).unwrap()),
            params: json!({}),
        };
        def.parts.insert(PartId::new(id).unwrap(), part);
    }
    def
}

/// What a check needs to build every kit kind: a rasteriser for the painted prints.
fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn every_registered_kind_publishes_the_same_facts_under_any_palette() {
    omuzaki_char::link();
    // Every seed and option set in the nightly run (which sets `OMUZAKI_FULL_TESTS`); the pull request gate, whose test
    // shards have a time limit (#2300), sweeps the base seed under one option set of each quality.
    let sweep = if std::env::var_os("OMUZAKI_FULL_TESTS").is_some() { Sweep::FULL } else { Sweep::QUICK };
    let mut report = check_all_with(&kit_characters(), None, &Probes::registered(), &services(), sweep);
    // The kinds the plain characters can't build, on the dressed one.
    let left: Vec<&str> = report.unreached.iter().map(|(kind, _)| kind.as_str()).collect();
    let dressed = check_with(&dressed(), Some(&left), &Probes::registered(), &services(), sweep);
    report.violations.extend(dressed.violations);
    report.unprobed.extend(dressed.unprobed);
    report.unreached = dressed.unreached;
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

#[test]
fn check_lists_a_fact_unprobed_when_the_only_same_name_probe_is_for_another_type() {
    // The kit's head publishes `head`. A probe for another fact that is also called "head" must not stand in
    // for `HeadFact`'s: it sees no values and would pass any palette-dependent one.
    omuzaki_char_api::fact! {
        /// A fact that shares the name of `HeadFact`.
        HeadClash = "head", omuzaki_char_api::Last<u32>;
    }
    let bases = kit_characters();
    let wrong = Probes::default().fact::<HeadClash>();
    // Which probe stands in does not hang on the seed or the options: the quick sweep shows it.
    let report = check_all_with(&bases, Some(&["head"]), &wrong, &BuildServices::default(), Sweep::QUICK);
    assert!(
        report.unprobed.iter().any(|(kind, fact)| kind == "head" && fact == "head"),
        "a wrong-type probe stood in: {:?}",
        report.unprobed
    );
    assert!(report.violations.is_empty(), "{:?}", report.violations);
    // With the right probe the fact is compared.
    let report =
        check_all_with(&bases, Some(&["head"]), &Probes::registered(), &BuildServices::default(), Sweep::QUICK);
    assert!(!report.unprobed.iter().any(|(_, fact)| fact == "head"), "{:?}", report.unprobed);
}

// The signature is `Differs`.
#[allow(clippy::unnecessary_wraps)]
fn always_differs(_: &Publishes, _: &Publishes) -> Option<String> {
    Some("stale probe used".to_owned())
}

static STALE_HEAD: FactProbe = FactProbe::new::<HeadFact>(always_differs);

/// The registered probe of `HeadFact`, which `check` should use.
fn head_probe() -> &'static FactProbe {
    omuzaki_char_api::fact::probes().find(|p| p.name == "head").expect("head has a probe")
}

#[test]
fn with_two_probes_for_one_fact_check_uses_the_last_one_given() {
    let others = || omuzaki_char_api::fact::probes().filter(|p| p.name != "head");
    let bases = kit_characters();
    let run =
        |probes: Probes| check_all_with(&bases, Some(&["head"]), &probes, &BuildServices::default(), Sweep::QUICK);
    // The stale probe first: the real one replaces it, so nothing differs.
    let last_wins = run(Probes::from_probes(others().chain([&STALE_HEAD, head_probe()])));
    assert!(last_wins.violations.is_empty(), "the stale probe was used: {:?}", last_wins.violations);
    // The stale probe last: it is the one used, and it says so.
    let stale_last = run(Probes::from_probes(others().chain([head_probe(), &STALE_HEAD])));
    assert!(stale_last.violations.iter().any(|v| v.contains("stale probe used")), "{:?}", stale_last.violations);
}

#[test]
fn probe_problems_come_back_sorted_by_fact_whatever_order_the_probes_come_in() {
    // A doubled `head` probe and a stray "test-registered-probe" one, given in both orders.
    let problems = |probes: [&'static FactProbe; 3]| Probes::from_probes(probes).probe_problems().expect("unique");
    let forward = problems([head_probe(), head_probe(), &STRAY]);
    let backward = problems([&STRAY, head_probe(), head_probe()]);
    assert_eq!(forward, backward);
    assert_eq!(forward.len(), 2, "{forward:?}");
    assert!(forward[0].contains("'head'") && forward[1].contains("'test-registered-probe'"), "{forward:?}");
}

/// The first palette contract (#645): every part of the kit characters, the dressed one included, builds the same
/// when only palette slots its parameters don't name change. The facts' half is the sweep above.
#[test]
fn every_kit_part_reads_colours_only_through_its_parameters() {
    for def in kit_characters().into_iter().chain([dressed()]) {
        let parts: Vec<String> = def.parts.keys().map(ToString::to_string).collect();
        let violations = check_colours_through_parameters(&def, &services()).unwrap();
        assert!(violations.is_empty(), "parts {parts:?}: {violations:#?}");
    }
}
