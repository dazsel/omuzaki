//! The fixtures, the mock library and the seed checks, on the mock character.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use omuzaki_char_api::PartId;
use omuzaki_doc_api::DocRead;
use omuzaki_library_api::{
    Bindings, ItemManifest, ItemScope, ItemSource, Library, LibraryError, Origin, PublisherCheck, VersionReq, check_ops,
};
use omuzaki_library_mock::{MockLibrary, apply_for_test, apply_with_library, conformance, fixtures, seed};
use omuzaki_ops_api::find;

fn bindings(m: &ItemManifest) -> Bindings {
    let mut b = Bindings { character: Some(support::character()), name: m.name.clone(), ..Bindings::default() };
    // The bundle's nested part item creates `ribbon`.
    for key in m.creates.keys().map(String::as_str).chain(["ribbon"]) {
        b.parts.insert(key.to_owned(), PartId::new(format!("new-{key}")).unwrap());
    }
    b
}

#[test]
fn the_mock_passes_its_own_conformance_suite() {
    conformance::run(&MockLibrary::fixtures());
}

#[test]
fn the_mock_passes_the_compat_conformance_on_the_fixture_character() {
    let mut library = MockLibrary::fixtures();
    library.set_characters(fixtures::characters());
    conformance::compat(&library, &support::character());
}

#[test]
fn the_fixture_character_facts_match_the_mock_character() {
    let doc = support::doc();
    let at = omuzaki_char_api::paths::character(&support::character());
    let node = doc.get(&at).expect("mira");
    let (_, facts) = fixtures::characters().remove(0);
    assert_eq!(node["template"], facts.template);
    let kinds: std::collections::BTreeSet<String> =
        node["parts"].as_object().unwrap().values().map(|p| p["kind"].as_str().unwrap().to_owned()).collect();
    assert_eq!(kinds, facts.part_kinds);
}

#[test]
fn every_fixture_checks_and_applies_on_the_mock_character_except_the_unavailable_ones() {
    let library = MockLibrary::fixtures();
    let doc = support::doc();
    for f in fixtures::all() {
        let m = &f.manifest;
        let result = apply_with_library(&library, m, &doc, &bindings(m));
        // The library reports an item unavailable exactly when applying it would fail for lack of the item itself.
        // (Two publishers on one id can't be described by id; the conformance suite covers them.)
        if let Ok(detail) = library.describe(&m.item_id().unwrap(), Some(&m.item_version().unwrap())) {
            let unavailable = detail.info.unavailable;
            assert_eq!(unavailable.is_some(), result.is_err(), "{}: {unavailable:?} vs {result:?}", m.id);
        }
        match m.id.as_str() {
            "test/stages/lamp" => {
                assert!(m.check().iter().any(|p| p.field == "target"), "the scene target is reserved");
            }
            "test/colourways/ghost" => {
                assert_eq!(check_ops(m, |n| find(n).map(|r| (r.meta)())).len(), 1, "unknown op");
            }
            "test/outfits/dangling" | "test/outfits/ghosted" => {
                assert_eq!(m.check(), vec![], "{}: well-formed, only its dependency is missing", m.id);
            }
            _ => {
                assert_eq!(m.check(), vec![], "{}", m.id);
                assert!(!result.expect(&m.id).is_empty(), "{} changes the document", m.id);
            }
        }
    }
}

#[test]
fn a_bundle_applies_its_items_in_order_and_needs_a_library() {
    let (doc, bundle) = (support::doc(), fixtures::bundle());
    let b = bindings(&bundle);
    assert!(apply_for_test(&bundle, &doc, &b).is_err());
    let patch = apply_with_library(&MockLibrary::fixtures(), &bundle, &doc, &b).unwrap();
    assert_eq!(patch.changes.len(), 3, "part, setting, own step");
    assert_eq!(patch.changes[2].after, Some(serde_json::json!("#112233")));
    assert!(
        doc.get(&omuzaki_char_api::paths::character(&support::character()).join("parts").join("new-ribbon")).is_none()
    );
}

#[test]
fn the_publisher_fixtures_verify_as_signed_and_unverified() {
    let verify = |m: &ItemManifest| m.verify(Origin::Installed);
    assert!(matches!(verify(&fixtures::signed_ember("1.0.0")), PublisherCheck::Verified { .. }));
    assert!(matches!(verify(&fixtures::unverified()), PublisherCheck::Unverified { .. }));
    assert_eq!(fixtures::part().verify(Origin::CompiledIn), PublisherCheck::FirstParty);
}

#[test]
fn inserting_refuses_a_clash_and_a_refused_publisher_and_ignores_a_repeat() {
    let mut library = MockLibrary::new();
    library.insert(fixtures::unverified(), ItemScope::Mine).unwrap();
    library.insert(fixtures::unverified(), ItemScope::Mine).unwrap();
    let mut changed = fixtures::unverified();
    changed.summary = "Different content, same version.".into();
    assert!(
        matches!(library.insert(changed, ItemScope::Mine), Err(LibraryError::Storage(m)) if m.contains("different hash"))
    );
    assert!(
        matches!(library.insert(fixtures::part(), ItemScope::Mine), Err(LibraryError::Storage(m)) if m.contains("refused"))
    );
    assert_eq!(library.versions(&fixtures::unverified().item_id().unwrap()).unwrap().len(), 1);
}

#[test]
fn a_version_range_with_no_match_lists_what_is_installed() {
    let library = MockLibrary::fixtures();
    let err = library.resolve(&fixtures::part().item_id().unwrap(), &VersionReq::parse("^2").unwrap()).unwrap_err();
    assert!(matches!(err, LibraryError::NoMatchingVersion { installed, .. } if installed.len() == 1));
}

#[derive(Default)]
struct Source(Vec<ItemManifest>);

impl ItemSource for Source {
    fn id(&self) -> &'static str {
        "fixture-source"
    }

    fn items(&self) -> Vec<ItemManifest> {
        self.0.clone()
    }
}

/// An `omuzaki/` item that is valid, which the broken variants below each spoil in one way.
fn seed_item() -> ItemManifest {
    let toml = include_str!("seed_item.toml");
    ItemManifest::parse(toml).unwrap()
}

#[test]
fn a_good_seed_source_passes() {
    seed::assert_source(&Source(vec![seed_item()]), &support::doc());
}

#[test]
fn seed_checks_name_the_item_and_the_rule() {
    let doc = support::doc();
    let mut unknown_op = seed_item();
    unknown_op.steps = fixtures::ghost().steps;
    let mut rights = seed_item();
    rights.rights.licence = "MIT".into();
    let mut no_origin = seed_item();
    no_origin.origin = None;
    let mut asset = seed_item();
    asset.assets.insert("tex".into(), omuzaki_store_api::ContentHash::of(b"x"));
    for (broken, rule) in [(unknown_op, "ops"), (rights, "rights"), (no_origin, "origin"), (asset, "assets")] {
        let problems = seed::check_source(&Source(vec![broken.clone()]), &doc);
        assert!(
            problems.iter().any(|p| p.item == "omuzaki/colourways/seed-dusk" && p.rule == rule),
            "{rule}: {problems:?}"
        );
    }
}
