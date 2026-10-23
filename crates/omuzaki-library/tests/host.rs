//! The host library against the library contract, and its ops end to end on a document: apply as one undoable step,
//! the lock, the three-way update, detach, and install and uninstall into a folder.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use omuzaki_library::HostLibrary;
use omuzaki_library_api::{CompatStatus, CompatTarget, ItemManifest, ItemScope, Library, Query};
use omuzaki_library_mock::{conformance, fixtures};
use omuzaki_ops_api::{Actor, ErrorCode, ErrorDetail};
use serde_json::{Value, json};
use support::{Studio, character};

fn loaded() -> HostLibrary {
    let library = HostLibrary::empty(None);
    for fixture in fixtures::all() {
        library.insert(fixture.manifest, fixture.scope).unwrap();
    }
    library
}

#[test]
fn the_host_library_passes_the_library_conformance_suite() {
    conformance::run(&loaded());
}

#[test]
fn compatibility_filters_with_the_facts_the_caller_reads_from_the_document() {
    let library = loaded();
    let (id, facts) = fixtures::characters().remove(0);
    let query = Query { compatible_with: Some(CompatTarget::Character(id)), limit: 100, ..Query::default() };
    let page = library.search_for(&query, Some(&facts)).unwrap();
    let status = |item: &str| page.items.iter().find(|i| i.id.as_str() == item).map(|i| i.compat.clone());
    assert_eq!(status("test/colourways/fits"), Some(CompatStatus::Compatible));
    for gone in ["creature-only", "other-template", "needs-wings"] {
        assert_eq!(status(&format!("test/colourways/{gone}")), None, "{gone} is filtered out");
    }
    assert!(matches!(status("test/colourways/gothic"), Some(CompatStatus::StyleMismatch { .. })));
    let all = library.search(&Query { limit: 100, ..Query::default() }).unwrap();
    assert_eq!(page.total + 3, all.total, "exactly the three incompatible items are filtered");
    assert!(library.search(&query).is_err(), "without the facts a character target is refused, not ignored");
}

/// A first-party colourway that runs `steps`, at `version`.
fn item(id: &str, version: &str, extra: &str, steps: &str) -> ItemManifest {
    item_of(id, version, "colourways", "setting", extra, steps)
}

fn item_of(id: &str, version: &str, section: &str, kind: &str, extra: &str, steps: &str) -> ItemManifest {
    let text = format!(
        "schema = 1\nid = \"{id}\"\nversion = \"{version}\"\nsection = \"{section}\"\nkind = \"{kind}\"\n\
         target = \"character\"\ntitle = \"{id}\"\nsummary = \"A host test item.\"\ntags = [\"test\"]\n\
         origin = \"test fixture\"\n{extra}\n[rights]\nauthor = \"Omuzaki\"\nlicence = \"CC0-1.0\"\n\
         commercial = \"ok\"\nowned = true\n\n{steps}"
    );
    ItemManifest::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn palette(slot: &str, colour: &str) -> String {
    format!(
        "[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\nslot = \"{slot}\"\n\
         colour = \"{colour}\"\n"
    )
}

/// `test/colourways/tide`: 1.0.0 sets the outfit; 1.1.0 sets a new outfit colour and the skin line too.
fn tide_library() -> HostLibrary {
    let library = loaded();
    library
        .insert(item("test/colourways/tide", "1.0.0", "", &palette("outfit", "#111111")), ItemScope::FirstParty)
        .unwrap();
    let steps = palette("outfit", "#222222") + &palette("skin_line", "#333333");
    library.insert(item("test/colourways/tide", "1.1.0", "", &steps), ItemScope::FirstParty).unwrap();
    library
}

fn apply(studio: &mut Studio, item: &str, version: Option<&str>) -> Value {
    let mut args = json!({ "item": item, "character": character() });
    if let Some(v) = version {
        args["version"] = json!(v);
    }
    studio.run("library.apply", args).unwrap()
}

#[test]
fn apply_lands_the_recipe_as_one_step_with_a_record_and_source_rows_and_undo_takes_it_all_back() {
    let mut studio = Studio::new(loaded());
    let before = studio.snapshot();

    let dry = studio.dry("library.apply", json!({ "item": "test/hair/ribbon", "character": character() })).unwrap();
    assert!(!dry.patch.is_empty(), "a dry run shows the patch");
    assert_eq!(studio.snapshot(), before, "and changes nothing");

    let out = apply(&mut studio, "test/hair/ribbon", None);
    assert_eq!(out["application"], "hairstyle", "an item with a slot is keyed by its slot");
    assert_eq!(out["created"]["ribbon"], "ribbon");
    assert_eq!(studio.at("parts/ribbon/kind"), Some(json!("mock-hair")));
    let record = studio.at("applied/hairstyle").expect("the application record");
    assert_eq!(record["item"], "test/hair/ribbon");
    assert_eq!(record["version"], "1.0.0");
    let source = studio.at("sources/ribbon").expect("a source row for the created part");
    assert_eq!(source["provenance"]["source"], json!({ "kind": "library", "item": "test/hair/ribbon@1.0.0" }));
    assert_eq!(source["provenance"]["licence"], "CC0-1.0", "the rights travel with the part");

    studio.undo();
    assert_eq!(studio.snapshot(), before, "one undo removes the part, its source row and the record");
}

#[test]
fn a_bundle_applies_its_items_and_its_own_step_as_one_patch() {
    let mut studio = Studio::new(loaded());
    let before = studio.snapshot();
    apply(&mut studio, "test/outfits/casual", None);
    assert!(studio.at("parts/ribbon").is_some(), "the bundle's part item ran");
    assert_eq!(studio.at("palette/outfit"), Some(json!("#112233")), "its own step runs after its items");
    studio.undo();
    assert_eq!(studio.snapshot(), before);
}

#[test]
fn an_incompatible_item_is_refused_and_a_style_mismatch_applies_with_a_warning() {
    let mut studio = Studio::new(loaded());
    let err =
        studio.run("library.apply", json!({ "item": "test/colourways/other-template", "character": character() }));
    assert_eq!(err.unwrap_err().code, ErrorCode::Invalid);
    studio.hand_edit("look", Some(json!({ "preset": "soft" })));
    let out = apply(&mut studio, "test/colourways/gothic", None);
    assert_eq!(out["warnings"].as_array().map(Vec::len), Some(1), "{out}");
}

#[test]
fn a_hand_made_part_of_the_same_id_is_kept_and_named_unless_replaced() {
    let mut studio = Studio::new(loaded());
    studio.hand_edit("parts/ribbon", Some(json!({ "kind": "mock-hair", "layer": 1 })));
    let out = apply(&mut studio, "test/hair/ribbon", None);
    assert_eq!(out["created"]["ribbon"], "ribbon-2", "{out}");
    assert_eq!(studio.at("parts/ribbon/layer"), Some(json!(1)), "the hand-made part is kept");
    assert_eq!(out["warnings"].as_array().map(Vec::len), Some(1));
    studio.undo();
    let args = json!({ "item": "test/hair/ribbon", "character": character(), "replace": ["ribbon"] });
    let out = studio.run("library.apply", args).unwrap();
    assert_eq!(out["created"]["ribbon"], "ribbon");
    assert_eq!(studio.at("parts/ribbon/layer"), Some(json!(5)), "replaced in the same step");
}

#[test]
fn applying_the_same_slotted_item_again_is_an_update_that_changes_nothing() {
    let mut studio = Studio::new(loaded());
    apply(&mut studio, "test/hair/ribbon", None);
    let before = studio.snapshot();
    let out = apply(&mut studio, "test/hair/ribbon", None);
    assert_eq!((out["updated"].clone(), out["changes"].clone()), (json!(true), json!([])), "{out}");
    assert_eq!(studio.snapshot(), before);
}

#[test]
fn the_lock_lists_each_application_and_sees_when_the_person_overrode_all_of_it() {
    let mut studio = Studio::new(tide_library());
    apply(&mut studio, "test/colourways/tide", Some("1.0.0"));
    let lock = studio.run("library.lock", json!({})).unwrap();
    let row = &lock["applications"][0];
    assert_eq!(lock["total"], 1);
    assert_eq!(row["resolves"], "ok");
    assert_eq!(row["newest"], "1.1.0", "the newest compatible version is offered");
    assert_eq!(row["stale"], false);
    studio.hand_edit("palette/outfit", Some(json!("#999999")));
    let lock = studio.run("library.lock", json!({ "character": character() })).unwrap();
    assert_eq!(lock["applications"][0]["stale"], true, "{lock}");
}

#[test]
fn a_pinned_version_wins_over_the_newest_when_applying_again() {
    let mut studio = Studio::new(tide_library());
    apply(&mut studio, "test/colourways/tide", Some("1.0.0"));
    let out = apply(&mut studio, "test/colourways/tide", None);
    assert_eq!(out["version"], "1.0.0", "the character's pin is the lock");
}

#[test]
fn update_takes_the_new_version_where_the_person_left_it_alone() {
    let mut studio = Studio::new(tide_library());
    let app = apply(&mut studio, "test/colourways/tide", Some("1.0.0"))["application"].clone();
    let out = studio.run("library.update", json!({ "character": character(), "application": app })).unwrap();
    assert_eq!(out["version"], "1.1.0");
    assert_eq!(studio.at("palette/outfit"), Some(json!("#222222")));
    assert_eq!(studio.at("palette/skin_line"), Some(json!("#333333")));
    assert_eq!(out["conflicts"], json!([]));
    assert_eq!(studio.at(&format!("applied/{}/version", app.as_str().unwrap())), Some(json!("1.1.0")));
    studio.undo();
    assert_eq!(studio.at("palette/outfit"), Some(json!("#111111")), "an update is one undo step");
}

#[test]
fn update_keeps_the_persons_change_and_reports_the_conflict_unless_the_item_is_preferred() {
    let mut studio = Studio::new(tide_library());
    let app = apply(&mut studio, "test/colourways/tide", Some("1.0.0"))["application"].clone();
    studio.hand_edit("palette/outfit", Some(json!("#999999")));
    let out = studio.run("library.update", json!({ "character": character(), "application": app })).unwrap();
    assert_eq!(studio.at("palette/outfit"), Some(json!("#999999")), "the person's colour is kept");
    assert_eq!(studio.at("palette/skin_line"), Some(json!("#333333")), "what only the item changed lands");
    assert_eq!(out["conflicts"], json!(["palette/outfit"]), "{out}");
    studio.undo();
    let args = json!({ "character": character(), "application": app, "prefer": "item" });
    studio.run("library.update", args).unwrap();
    assert_eq!(studio.at("palette/outfit"), Some(json!("#222222")), "prefer item takes the item's");
}

/// `test/hair/bow`, which creates the part `bow`: on layer 5 in 1.0.0 and on layer 6 in 1.1.0.
fn bow_library() -> HostLibrary {
    let library = loaded();
    for (version, layer) in [("1.0.0", 5), ("1.1.0", 6)] {
        let extra = format!("slot = \"bow\"\ncreates.ribbon = {{ kind = \"hair\", id = \"bow\", layer = {layer} }}\n");
        let step = format!(
            "[[step]]\nop = \"part.add\"\n[step.args]\ncharacter = \"$character\"\nkind = \"mock-hair\"\n\
             id = \"$parts.ribbon\"\nlayer = {layer}\n"
        );
        library
            .insert(item_of("test/hair/bow", version, "hair", "part", &extra, &step), ItemScope::FirstParty)
            .unwrap();
    }
    library
}

#[test]
fn an_update_moves_a_created_part_to_the_new_version_but_a_part_the_person_removed_stays_removed() {
    let mut studio = Studio::new(bow_library());
    apply(&mut studio, "test/hair/bow", Some("1.0.0"));
    studio.run("library.update", json!({ "character": character(), "application": "bow" })).unwrap();
    assert_eq!(studio.at("parts/bow/layer"), Some(json!(6)), "the new version's part lands under the old id");

    let mut studio = Studio::new(bow_library());
    apply(&mut studio, "test/hair/bow", Some("1.0.0"));
    studio.hand_edit("parts/bow", None);
    studio.run("library.update", json!({ "character": character(), "application": "bow" })).unwrap();
    assert_eq!(studio.at("parts/bow"), None, "the update doesn't bring back what the person removed");
}

#[test]
fn detach_forgets_a_record_only_once_nothing_it_made_remains() {
    let mut studio = Studio::new(tide_library());
    let app = apply(&mut studio, "test/colourways/tide", Some("1.0.0"))["application"].clone();
    let args = json!({ "character": character(), "application": app });
    let err = studio.run("library.detach", args.clone()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "the outfit is still the item's");
    studio.hand_edit("palette/outfit", Some(json!("#999999")));
    studio.run("library.detach", args).unwrap();
    assert_eq!(studio.at(&format!("applied/{}", app.as_str().unwrap())), None);
}

#[test]
fn install_writes_the_persons_library_folder_and_uninstall_removes_it_and_an_agent_needs_the_person() {
    let folder = tempfile::tempdir().unwrap();
    let mut studio = Studio::new(HostLibrary::open(Some(folder.path())));
    let manifest = toml::to_string(&fixtures::unverified()).unwrap();
    let id = fixtures::unverified().id;

    let agent = studio.run_as(
        &Actor::Agent { id: "agent".into(), model: None },
        "library.install",
        json!({ "manifest": manifest }),
    );
    assert!(
        matches!(agent.unwrap_err().detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })),
        "an agent needs the person"
    );

    let out = studio.run("library.install", json!({ "manifest": manifest })).unwrap();
    assert_eq!(out["installed"][0]["new"], true);
    let again = studio.run("library.install", json!({ "manifest": manifest })).unwrap();
    assert_eq!(again["installed"][0]["new"], false, "installing the same item twice is a no-op");

    let reopened = HostLibrary::open(Some(folder.path()));
    let mine = reopened.list(None, Some(&ItemScope::Mine), 0, 100).unwrap();
    assert!(mine.items.iter().any(|i| i.id.as_str() == id), "the item survives a restart: {mine:?}");

    studio.run("library.uninstall", json!({ "item": id })).unwrap();
    let reopened = HostLibrary::open(Some(folder.path()));
    assert_eq!(reopened.list(None, Some(&ItemScope::Mine), 0, 100).unwrap().total, 0);
}
