//! Regression tests for the last review's findings: `insert` runs `ItemManifest::check`, a hash held by two
//! publishers resolves the same whatever the load order, and an asset's provenance counts towards publishability.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_library_api::{ItemScope, Library, LibraryError, Query, Usage};
use omuzaki_library_mock::{MockLibrary, fixtures};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source};

fn storage_error(library: &mut MockLibrary, m: omuzaki_library_api::ItemManifest) -> String {
    match library.insert(m, ItemScope::FirstParty) {
        Err(LibraryError::Storage(why)) => why,
        other => panic!("expected a storage refusal, got {other:?}"),
    }
}

fn listed(library: &MockLibrary) -> usize {
    library.search(&Query { limit: 100, ..Query::default() }).unwrap().total
}

#[test]
fn insert_rejects_a_manifest_that_fails_check_and_lists_nothing() {
    let mut library = MockLibrary::new();

    let mut no_steps = fixtures::setting();
    no_steps.steps.clear();
    let why = storage_error(&mut library, no_steps);
    assert!(why.contains("fails check") && why.contains("step"), "names the problem: {why}");

    // An item step in a manifest whose kind is not `bundle`.
    let mut stray = fixtures::bundle();
    stray.kind = fixtures::setting().kind;
    let why = storage_error(&mut library, stray);
    assert!(why.contains("fails check"), "{why}");

    // Every problem is named, not only the first: the scene target is reserved.
    let why = storage_error(&mut library, fixtures::scene());
    assert!(why.contains("target"), "the reserved scene target is refused: {why}");

    assert_eq!(listed(&library), 0, "nothing refused is listed");
    library.insert(fixtures::setting(), ItemScope::FirstParty).unwrap();
    assert_eq!(listed(&library), 1, "a well-formed manifest is accepted");
}

#[test]
fn two_publishers_of_one_hash_resolve_the_same_whatever_the_load_order() {
    let (a, b) = (fixtures::cosigned(fixtures::SIGNER_SEED), fixtures::cosigned(fixtures::RIVAL_SEED));
    assert_eq!(a.hash(), b.hash(), "the premise: publisher and signature are outside the hash");
    let load = |first: &omuzaki_library_api::ItemManifest, second: &omuzaki_library_api::ItemManifest| {
        let mut library = MockLibrary::new();
        for m in [first, second] {
            library.insert(m.clone(), ItemScope::Mine).unwrap();
        }
        library.by_hash(&a.hash()).unwrap()
    };
    let (forward, backward) = (load(&a, &b), load(&b, &a));
    assert_eq!(forward, backward, "same entry and same publisher check in either order");
}

fn private_test_asset() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "CC0-1.0".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: true,
    }
}

fn publishable(library: &MockLibrary, id: &str) -> bool {
    library.describe(&id.parse().unwrap(), None).unwrap().info.rights.publish_ok
}

#[test]
fn an_item_is_publishable_only_if_its_assets_are() {
    let hash = ContentHash::of(b"texture");
    let mut m = fixtures::signed_ember("1.0.0");
    m.assets.insert("tex".into(), hash);
    let id = m.id.clone();
    // The ember fixture's rights are permissive, so only the asset can block it. (The manifest is re-signed by
    // the fixture helper's key only for the signature check, which `assets` is not part of here: use an unsigned copy.)
    m.publisher = None;
    m.signature = None;

    let mut library = MockLibrary::new();
    library.insert(m.clone(), ItemScope::Mine).unwrap();
    assert!(!publishable(&library, &id), "an asset of unknown provenance fails closed");
    let hidden = library.search(&Query { usable_for: Some(Usage::Publish), limit: 100, ..Query::default() }).unwrap();
    assert!(hidden.items.iter().all(|i| i.id.as_str() != id), "and is not offered for publishing");

    let mut cleared = Provenance { private_test: false, ..private_test_asset() };
    library.set_asset_provenance([(hash, cleared.clone())]);
    assert!(publishable(&library, &id), "a cleared asset lets a permissive item publish");

    library.set_asset_provenance([(hash, private_test_asset())]);
    assert!(!publishable(&library, &id), "a private-test asset blocks a permissive item");
    cleared.commercial = Commercial::No;
    library.set_asset_provenance([(hash, cleared)]);
    assert!(!publishable(&library, &id), "a non-commercial asset blocks it");
}

#[test]
fn a_cjk_query_finds_a_title_that_contains_it_in_korean_japanese_and_chinese() {
    let mut library = MockLibrary::new();
    for (slug, title) in [("ko", "푸른머리"), ("ja", "ぎんがみ"), ("zh", "银白长发")] {
        let mut m = fixtures::setting();
        m.id = format!("test/colourways/{slug}");
        m.title = title.into();
        m.summary = "Nothing relevant.".into();
        m.tags.clear();
        library.insert(m, ItemScope::FirstParty).unwrap();
    }
    let hits = |text: &str| -> Vec<String> {
        let q = Query { text: Some(text.into()), limit: 100, ..Query::default() };
        library.search(&q).unwrap().items.into_iter().map(|i| i.id.to_string()).collect()
    };
    assert_eq!(hits("머리"), ["test/colourways/ko"], "Korean: a syllable bigram inside the title");
    assert_eq!(hits("푸른"), ["test/colourways/ko"]);
    assert_eq!(hits("がみ"), ["test/colourways/ja"], "Japanese: voiced kana keep their marks");
    assert_eq!(hits("长发"), ["test/colourways/zh"], "Chinese: a bigram inside the title");
}
