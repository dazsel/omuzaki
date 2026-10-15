//! Regression tests for the review of the first version: visibility and identity, seed validation, and apply
//! containment. Each test fails on the behaviour it replaced, except the argument test, which pins a rule the
//! recipe expansion already enforced.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use omuzaki_doc_api::DocRead;
use omuzaki_library_api::{Bindings, ItemManifest, ItemScope, ItemSource, Library, LibraryError, Query, VersionReq};
use omuzaki_library_mock::{MockLibrary, apply_for_test, fixtures, seed};
use omuzaki_ops_api::{ErrorCode, ErrorDetail};

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

// --- Round 2: errors keep their structure, availability agrees with apply ---

fn refused_error(steps: &str, bindings: &Bindings) -> omuzaki_ops_api::OpError {
    apply_for_test(&setting("omuzaki/colourways/seed-bad", steps), &support::doc(), bindings).unwrap_err()
}

#[test]
fn a_planned_ops_error_keeps_its_code_hint_and_detail_with_only_the_step_put_in_front() {
    let refuse = |how: &str| {
        format!("[[step]]\nop = \"mock.refuse\"\n[step.args]\ncharacter = \"$character\"\nhow = \"{how}\"\n")
    };
    let missing = refused_error(&refuse("missing"), &bound());
    assert_eq!(missing.code, ErrorCode::NotFound);
    assert_eq!(missing.hint.as_deref(), Some("restore it first"));
    assert_eq!(missing.message, "step/0: the thing is gone");
    let person = refused_error(&refuse("person"), &bound());
    assert_eq!(person.code, ErrorCode::Denied);
    assert!(matches!(person.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })), "{person:?}");
    // An error that names a field gets the step in front of the field instead.
    let tint = "[[step]]\nop = \"part.tint\"\n[step.args]\ncharacter = \"$character\"\npart = \"nowhere\"\ncolour = \"#000000\"\n";
    let invalid = refused_error(tint, &bound());
    assert_eq!((invalid.code, invalid.field.as_deref()), (ErrorCode::Invalid, Some("step/0/part")));
}

#[test]
fn expansion_and_library_failures_keep_their_kind_field_and_hint() {
    // An unbound placeholder keeps the hint saying what to pass.
    let unbound = refused_error(RIBBON, &bound());
    assert_eq!(unbound.code, ErrorCode::Invalid);
    assert!(unbound.field.as_deref().is_some_and(|f| f.contains("step/0")) && unbound.hint.is_some(), "{unbound:?}");
    let doc = support::doc();
    let library = MockLibrary::fixtures();
    let apply = |m: &ItemManifest| omuzaki_library_mock::apply_with_library(&library, m, &doc, &bound()).unwrap_err();
    // A missing dependency is NotFound on its step, not Invalid.
    let dangling = apply(&fixtures::dangling_bundle());
    assert_eq!((dangling.code, dangling.field.as_deref()), (ErrorCode::NotFound, Some("step/0")));
    // A range nothing matches says what is installed; a publisher clash says to name one.
    let range = apply(&bundle_of("test/hair/ribbon", "^9"));
    assert_eq!(range.code, ErrorCode::Invalid);
    assert!(range.hint.as_deref().is_some_and(|h| h.contains("1.0.0")), "{range:?}");
    let clash = apply(&bundle_of("shared/colourways/bow", "^1"));
    assert_eq!((clash.code, clash.hint.as_deref()), (ErrorCode::Invalid, Some("name one publisher")));
}

fn bundle_of(dependency: &str, version: &str) -> ItemManifest {
    item(
        "omuzaki/outfits/seed-dep",
        "outfits",
        "bundle",
        "",
        &format!("[[step]]\nitem = \"{dependency}\"\nversion = \"{version}\"\n"),
    )
}

// --- availability agrees with apply, and every resolution path uses one tiering ---

#[test]
fn a_bundle_is_unavailable_exactly_when_applying_it_fails_on_a_dependency() {
    let mut library = MockLibrary::fixtures();
    let doc = support::doc();
    let nested = |item: &str| bundle_of(item, "^1");
    // Missing, unavailable through a nested bundle two levels down, and available.
    let mut cases = vec![
        (nested("test/hair/nonexistent"), true),
        (nested("test/outfits/ghosted"), true),
        (nested("test/outfits/casual"), false),
    ];
    for (i, (m, _)) in cases.iter_mut().enumerate() {
        m.id = format!("test/outfits/probe-{i}");
    }
    for (m, unavailable) in cases {
        library.insert(m.clone(), ItemScope::FirstParty).unwrap();
        let info = library.describe(&m.item_id().unwrap(), None).unwrap().info;
        assert_eq!(info.unavailable.is_some(), unavailable, "{}: {:?}", m.id, info.unavailable);
        let applied = omuzaki_library_mock::apply_with_library(&library, &m, &doc, &bindings_for_bundle());
        assert_eq!(applied.is_err(), unavailable, "{}: {applied:?}", m.id);
    }
}

#[test]
fn a_dependency_cycle_is_unavailable_like_it_fails_to_apply() {
    let mut library = MockLibrary::new();
    let (a, b) = (bundle_of("test/outfits/b", "^1"), bundle_of("test/outfits/a", "^1"));
    for (m, id) in [(a, "test/outfits/a"), (b, "test/outfits/b")] {
        let mut m = m;
        m.id = id.into();
        library.insert(m, ItemScope::FirstParty).unwrap();
    }
    let a = "test/outfits/a".parse().unwrap();
    let why = library.describe(&a, None).unwrap().info.unavailable.expect("a cycle is unavailable");
    assert!(why.contains("cycle"), "{why}");
}

fn bindings_for_bundle() -> Bindings {
    let mut b = bound();
    b.parts.insert("ribbon".into(), "new-ribbon".parse().unwrap());
    b
}

#[test]
fn all_teams_are_one_tier_for_resolve_describe_and_versions() {
    let mut library = MockLibrary::new();
    // "atelier" sorts before "studio" and holds the older version.
    library.insert(fixtures::tide("1.0.0"), ItemScope::Team("atelier".into())).unwrap();
    library.insert(fixtures::tide("1.1.0"), ItemScope::Team("studio".into())).unwrap();
    let tide = "signer/colourways/tide".parse().unwrap();
    assert_eq!(library.resolve(&tide, &VersionReq::Any).unwrap().scope, ItemScope::Team("studio".into()));
    assert_eq!(library.describe(&tide, None).unwrap().info.version.to_string(), "1.1.0");
    assert_eq!(library.versions(&tide).unwrap().len(), 2);
    // Two publishers in two teams are ambiguous in every path; Mine still beats both.
    library.insert(fixtures::reef(fixtures::SIGNER_SEED), ItemScope::Team("atelier".into())).unwrap();
    library.insert(fixtures::reef(fixtures::RIVAL_SEED), ItemScope::Team("studio".into())).unwrap();
    let reef = "shared/colourways/reef".parse().unwrap();
    assert!(matches!(library.resolve(&reef, &VersionReq::Any), Err(LibraryError::Ambiguous { .. })));
    assert!(matches!(library.describe(&reef, None), Err(LibraryError::Ambiguous { .. })));
    assert!(matches!(library.versions(&reef), Err(LibraryError::Ambiguous { .. })));
}

#[test]
fn compatible_with_filters_by_body_template_and_parts_and_keeps_style_mismatches_with_the_warning() {
    use omuzaki_library_api::{CompatStatus, CompatTarget};
    let mut library = MockLibrary::fixtures();
    library.set_characters(fixtures::characters());
    let mira = support::character();
    let query = Query { compatible_with: Some(CompatTarget::Character(mira)), limit: 100, ..Query::default() };
    let page = library.search(&query).unwrap();
    let ids: Vec<&str> = page.items.iter().map(|i| i.id.as_str()).collect();
    for gone in ["creature-only", "other-template", "needs-wings"] {
        assert!(!ids.contains(&format!("test/colourways/{gone}").as_str()), "{gone}");
    }
    let gothic = page.items.iter().find(|i| i.id.as_str() == "test/colourways/gothic").expect("a style mismatch stays");
    assert!(matches!(gothic.compat, CompatStatus::StyleMismatch { .. }));
    // Without the facts the mock refuses rather than answering unfiltered.
    assert!(matches!(MockLibrary::fixtures().search(&query), Err(LibraryError::NotFound(_))));
    // A newer incompatible version is skipped for the newest one that fits.
    let mut older = fixtures::fits();
    older.id = "test/colourways/versioned".into();
    let mut newer = older.clone();
    newer.version = "2.0.0".into();
    newer.compat.templates = vec!["tall".into()];
    library.insert(older, ItemScope::FirstParty).unwrap();
    library.insert(newer, ItemScope::FirstParty).unwrap();
    let page = library.search(&query).unwrap();
    let row = page.items.iter().find(|i| i.id.as_str() == "test/colourways/versioned").unwrap();
    assert_eq!(row.version.to_string(), "1.0.0", "the row shows the newest version that fits");
}
