//! Regression tests for the review of the first version: visibility and identity, seed validation, and apply
//! containment. Each test fails on the behaviour it replaced, except the argument test, which pins a rule the
//! recipe expansion already enforced.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use omuzaki_doc_api::DocRead;
use omuzaki_library_api::{Bindings, ItemManifest, ItemScope, ItemSource, Library, LibraryError, Query, VersionReq};
use omuzaki_library_mock::{MockLibrary, apply_for_test, fixtures, seed};
use omuzaki_ops_api::ErrorCode;

#[derive(Default)]
struct Source(Vec<ItemManifest>);

impl ItemSource for Source {
    fn id(&self) -> &'static str {
        "review-source"
    }

    fn items(&self) -> Vec<ItemManifest> {
        self.0.clone()
    }
}

/// A valid `omuzaki/` item whose recipe is `steps`, appended after the header. `extra` goes before `[rights]`.
fn item(id: &str, section: &str, kind: &str, extra: &str, steps: &str) -> ItemManifest {
    let text = format!(
        "schema = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nsection = \"{section}\"\nkind = \"{kind}\"\ntarget = \"character\"\n\
         title = \"{id}\"\nsummary = \"A review fixture.\"\ntags = [\"seed\"]\norigin = \"test fixture\"\n{extra}\n\
         [rights]\nauthor = \"Omuzaki\"\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\nowned = true\n\n{steps}"
    );
    ItemManifest::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn setting(id: &str, steps: &str) -> ItemManifest {
    item(id, "colourways", "setting", "", steps)
}

const PALETTE: &str = "[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\nslot = \"outfit\"\ncolour = \"#2a2346\"\n";

fn bound() -> Bindings {
    Bindings { character: Some(support::character()), ..Bindings::default() }
}

fn rules(problems: &[seed::SeedProblem]) -> Vec<(&str, &str)> {
    problems.iter().map(|p| (p.item.as_str(), p.rule)).collect()
}

// --- Class A: visibility and identity ---

#[test]
fn search_and_list_hide_unlisted_and_deprecated_items_before_paging_and_facets_but_they_still_resolve() {
    let mut library = MockLibrary::fixtures();
    let before = library.search(&Query { limit: 100, ..Query::default() }).unwrap();
    let mut hidden = fixtures::setting();
    hidden.id = "test/colourways/hidden".into();
    hidden.listed = false;
    let mut old = fixtures::setting();
    old.id = "test/colourways/retired".into();
    let mut retired = old.clone();
    retired.version = "2.0.0".into();
    retired.deprecated = Some("replaced".into());
    for m in [hidden.clone(), old, retired] {
        library.insert(m, ItemScope::FirstParty).unwrap();
    }
    let search = library.search(&Query { limit: 100, ..Query::default() }).unwrap();
    assert_eq!(search, before, "total, rows and facets are unchanged by hidden items");
    let paged = library.search(&Query { offset: before.total - 1, limit: 1, ..Query::default() }).unwrap();
    assert_eq!(paged.items, before.items[before.total - 1..], "the last page has the last visible row");
    let listed = library.list(None, None, 0, 100).unwrap();
    assert_eq!(listed.total, before.total);
    for id in ["test/colourways/hidden", "test/colourways/retired"] {
        assert!(search.items.iter().all(|i| i.id.as_str() != id), "{id} is not in search");
        assert!(listed.items.iter().all(|i| i.id.as_str() != id), "{id} is not in a listing");
        let id = id.parse().unwrap();
        library.resolve(&id, &VersionReq::Any).expect("a pin or dependency still resolves it");
        library.describe(&id, None).expect("and describes it");
    }
    assert_eq!(
        library.resolve(&"test/colourways/retired".parse().unwrap(), &VersionReq::Any).unwrap().manifest.version,
        "2.0.0"
    );
}

#[test]
fn a_publisher_renaming_itself_between_versions_is_still_one_publisher() {
    let mut library = MockLibrary::new();
    let mut renamed = fixtures::signed_ember("1.1.0");
    renamed.publisher.as_mut().unwrap().name = "Signer Studio".into();
    assert_eq!(renamed.publisher.as_ref().unwrap().key, fixtures::signed_ember("1.0.0").publisher.unwrap().key);
    library.insert(fixtures::signed_ember("1.0.0"), ItemScope::Mine).unwrap();
    library.insert(renamed, ItemScope::Mine).unwrap();
    let ember = "signer/colourways/ember".parse().unwrap();
    assert_eq!(library.resolve(&ember, &VersionReq::Any).unwrap().manifest.version, "1.1.0");
    assert_eq!(library.versions(&ember).unwrap().len(), 2);
    // A different key under the same id and version is still ambiguous, whatever it calls itself.
    let mut rival = fixtures::shared_bow(fixtures::RIVAL_SEED);
    rival.publisher.as_mut().unwrap().name = "Signer".into();
    library.insert(fixtures::shared_bow(fixtures::SIGNER_SEED), ItemScope::Mine).unwrap();
    library.insert(rival, ItemScope::Mine).unwrap();
    let bow = "shared/colourways/bow".parse().unwrap();
    assert!(
        matches!(library.resolve(&bow, &VersionReq::Any), Err(LibraryError::Ambiguous { candidates, .. }) if candidates.len() == 2)
    );
}

#[test]
fn one_id_and_version_cannot_hold_different_content_in_two_scopes() {
    let mut library = MockLibrary::new();
    library.insert(fixtures::unverified(), ItemScope::Mine).unwrap();
    let mut changed = fixtures::unverified();
    changed.summary = "Different content, same version.".into();
    let team = ItemScope::Team("studio".into());
    let err = library.insert(changed.clone(), team.clone()).unwrap_err();
    assert!(matches!(&err, LibraryError::Storage(m) if m.contains("different hash")), "{err}");
    // The same content in a second scope is fine, and the clash is symmetric.
    library.insert(fixtures::unverified(), team.clone()).unwrap();
    let mut other = MockLibrary::new();
    other.insert(changed, team).unwrap();
    assert!(other.insert(fixtures::unverified(), ItemScope::Mine).is_err());
}

// --- Class B: seed validation ---

#[test]
fn a_repeated_seed_id_is_reported_whether_identical_or_a_new_version() {
    let doc = support::doc();
    let one = setting("omuzaki/colourways/seed-twice", PALETTE);
    let mut newer = one.clone();
    newer.version = "1.1.0".into();
    assert_eq!(seed::check_source(&Source(vec![one.clone()]), &doc), vec![]);
    for (items, what) in [(vec![one.clone(), one.clone()], "twice"), (vec![one.clone(), newer], "1.0.0 and 1.1.0")] {
        let problems = seed::check_source(&Source(items), &doc);
        assert_eq!(rules(&problems), [("omuzaki/colourways/seed-twice", "id")], "{problems:?}");
        assert!(problems[0].detail.contains(what), "{}", problems[0]);
    }
}

fn tint(kinds: &str) -> ItemManifest {
    item(
        "omuzaki/bangs/seed-tint",
        "bangs",
        "setting",
        &format!("target_part = [{kinds}]\n"),
        "[[step]]\nop = \"part.tint\"\n[step.args]\ncharacter = \"$character\"\npart = \"$target\"\ncolour = \"#112233\"\n",
    )
}

#[test]
fn target_binds_the_one_part_of_the_wanted_kind_and_reports_none_or_several() {
    let doc = support::doc();
    // The mock character has one `mock-hair` part, two `mock-garment` parts and no `mock-wing`.
    assert_eq!(seed::check_source(&Source(vec![tint("\"mock-hair\"")]), &doc), vec![]);
    let hair =
        apply_for_test(&tint("\"mock-hair\""), &doc, &Bindings { target: Some("hair".parse().unwrap()), ..bound() })
            .unwrap();
    assert_eq!(hair.changes[0].path.to_string(), "characters/mira/parts/hair/tint");

    let several = seed::check_source(&Source(vec![tint("\"mock-garment\"")]), &doc);
    assert_eq!(rules(&several), [("omuzaki/bangs/seed-tint", "apply")]);
    assert!(several[0].detail.contains("ambiguous") && several[0].detail.contains("shirt, vest"), "{}", several[0]);

    let none = seed::check_source(&Source(vec![tint("\"mock-wing\"")]), &doc);
    assert!(none[0].detail.contains("mock-wing") && none[0].detail.contains("none"), "{}", none[0]);
    // Either kind is eligible: the hair and the two garments are three candidates.
    let any = seed::check_source(&Source(vec![tint("\"mock-hair\", \"mock-garment\"")]), &doc);
    assert!(any[0].detail.contains("ambiguous") && any[0].detail.contains("hair, shirt, vest"), "{}", any[0]);
}

const RIBBON: &str = "[[step]]\nop = \"part.add\"\n[step.args]\ncharacter = \"$character\"\nkind = \"mock-hair\"\nid = \"$parts.ribbon\"\n";

fn ribbon_part() -> ItemManifest {
    item(
        "omuzaki/hair/seed-ribbon",
        "hair",
        "part",
        "slot = \"hairstyle\"\ncreates.ribbon = { kind = \"hair\", id = \"ribbon\", layer = 5 }\n",
        RIBBON,
    )
}

#[test]
fn a_bundle_binds_part_keys_only_from_its_own_dependency_closure() {
    let doc = support::doc();
    let nested = "[[step]]\nitem = \"omuzaki/hair/seed-ribbon\"\nversion = \"^1\"\n";
    let with_part = item("omuzaki/outfits/seed-look", "outfits", "bundle", "", &format!("{nested}{PALETTE}"));
    let own_step = item("omuzaki/outfits/seed-bare", "outfits", "bundle", "", &format!("{RIBBON}{PALETTE}"));
    // The nested part item is in the bundle's tree, so `ribbon` is bound for it.
    assert_eq!(seed::check_source(&Source(vec![ribbon_part(), with_part]), &doc), vec![]);
    // Another item of the source creating `ribbon` is not the bundle's: the key stays unbound and the apply says so.
    let problems = seed::check_source(&Source(vec![ribbon_part(), own_step]), &doc);
    assert_eq!(rules(&problems), [("omuzaki/outfits/seed-bare", "apply")], "{problems:?}");
}

// --- Class C: apply containment ---

fn refused(steps: &str, bindings: &Bindings) -> (ErrorCode, String) {
    let e = apply_for_test(&setting("omuzaki/colourways/seed-bad", steps), &support::doc(), bindings).unwrap_err();
    (e.code, e.to_string())
}

#[test]
fn a_dry_run_refuses_ops_above_the_recipe_ceiling() {
    let peek = "[[step]]\nop = \"mock.peek\"\n[step.args]\ncharacter = \"$character\"\n";
    let export = "[[step]]\nop = \"mock.export\"\n[step.args]\ncharacter = \"$character\"\n";
    let (code, message) = refused(peek, &bound());
    assert_eq!(code, ErrorCode::Denied);
    assert!(message.contains("mock.peek") && message.contains("edit ops"), "{message}");
    let (code, message) = refused(export, &bound());
    assert_eq!(code, ErrorCode::Denied);
    assert!(message.contains("mock.export") && message.contains("fs"), "{message}");
}

#[test]
fn a_dry_run_refuses_arguments_that_name_another_character_or_a_scene_and_needs_a_bound_character() {
    let other = PALETTE.replace("$character", "zed");
    let (code, message) = refused(&other, &bound());
    assert_eq!(code, ErrorCode::Invalid);
    assert!(message.contains("must be `$character`"), "{message}");
    let scene = PALETTE.replace("slot", "scene = \"$character\"\nslot");
    assert!(refused(&scene, &bound()).1.contains("scene"), "a scene argument is refused");
    // Without a bound character there is nothing to confine to.
    let (code, message) = refused(PALETTE, &Bindings::default());
    assert_eq!(code, ErrorCode::Invalid);
    assert!(message.contains("character"), "{message}");
}

#[test]
fn a_change_outside_the_bound_character_fails_the_dry_run_and_the_seed_check() {
    let stray = "[[step]]\nop = \"mock.stray\"\n[step.args]\ncharacter = \"$character\"\n";
    let (code, message) = refused(stray, &bound());
    assert_eq!(code, ErrorCode::Denied);
    assert!(message.contains("characters/other/mark") && message.contains("characters/mira"), "{message}");
    let doc = support::doc();
    assert!(
        doc.get(&omuzaki_doc_api::DocPath::parse("characters/other").unwrap()).is_none(),
        "the dry run changes nothing"
    );
    let problems = seed::check_source(&Source(vec![setting("omuzaki/colourways/seed-bad", stray)]), &doc);
    assert!(problems.iter().any(|p| p.rule == "apply" && p.detail.contains("outside the target")), "{problems:?}");
    // The same recipe on its own character is fine.
    apply_for_test(&setting("omuzaki/colourways/seed-good", PALETTE), &doc, &bound()).unwrap();
}
