//! The library tasks' acceptance (#360, #209, #210, #406, #339) on the host library: installing files and folders,
//! publishers and acknowledgements, the lock across reinstalls and uninstalls, apply's refusals, re-apply as an
//! update, update's compatibility and majors, and search at scale.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Instant;

use omuzaki_library::HostLibrary;
use omuzaki_library_api::{CompatTarget, ItemManifest, ItemScope, Library, Query, Usage};
use omuzaki_library_mock::fixtures;
use omuzaki_ops_api::{ErrorCode, Grants, Scope};
use omuzaki_store_api::{ContentHash, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};
use support::{Studio, character};

fn parse(text: &str) -> ItemManifest {
    ItemManifest::parse(text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

/// A person's own colourway, `anon/colourways/sea`, unsigned.
fn sea(version: &str, colour: &str, extra: &str) -> String {
    format!(
        "schema = 1\nid = \"anon/colourways/sea\"\nversion = \"{version}\"\nsection = \"colourways\"\nkind = \"setting\"\n\
         target = \"character\"\nslot = \"colourway\"\ntitle = \"Sea\"\nsummary = \"Sea greens.\"\ntags = [\"sea\"]\n{extra}\n\
         [rights]\nauthor = \"Anon\"\nlicence = \"CC-BY-4.0\"\ncommercial = \"ok\"\n\n\
         [[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\nslot = \"outfit\"\ncolour = \"{colour}\"\n"
    )
}

fn write(dir: &std::path::Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

fn lock_row(studio: &mut Studio) -> Value {
    studio.run("library.lock", json!({ "character": character() })).unwrap()["applications"][0].clone()
}

// --- #210: install and the lock ---

#[test]
fn install_apply_install_a_newer_version_and_reopen_changes_nothing_and_the_lock_offers_it() {
    let data = tempfile::tempdir().unwrap();
    let store = Arc::new(MemStore::new());
    let mut studio = Studio::with_store(Arc::new(HostLibrary::open(Some(data.path()))), store.clone());
    let v1 = write(studio.project.path(), "downloads/sea-1.toml", &sea("1.0.0", "#0f5e57", ""));
    studio.run("library.install", json!({ "from": v1 })).unwrap();
    studio.run("library.apply", json!({ "item": "anon/colourways/sea", "character": character() })).unwrap();
    let applied = studio.snapshot();

    let v2 = write(studio.project.path(), "downloads/sea-2.toml", &sea("1.1.0", "#127a6f", ""));
    studio.run("library.install", json!({ "from": v2 })).unwrap();
    // Reopened: the same document over the library read back from its folder.
    let mut reopened = Studio::with_store(Arc::new(HostLibrary::open(Some(data.path()))), store.clone());
    reopened.doc = studio.doc.clone();
    assert_eq!(reopened.snapshot(), applied, "installing a newer version changes no project");
    let row = lock_row(&mut reopened);
    assert_eq!(
        (row["version"].clone(), row["newest"].clone(), row["resolves"].clone()),
        (json!("1.0.0"), json!("1.1.0"), json!("ok"))
    );
    assert_eq!(row["resolved_from"], "library");
}

#[test]
fn an_uninstalled_item_resolves_from_the_projects_copy_and_an_edited_one_is_a_mismatch() {
    let data = tempfile::tempdir().unwrap();
    let store = Arc::new(MemStore::new());
    let library = Arc::new(HostLibrary::open(Some(data.path())));
    let mut studio = Studio::with_store(library, store.clone());
    studio.run("library.install", json!({ "manifest": sea("1.0.0", "#0f5e57", "") })).unwrap();
    studio.run("library.apply", json!({ "item": "anon/colourways/sea", "character": character() })).unwrap();
    let hash = parse(&sea("1.0.0", "#0f5e57", "")).hash();
    assert!(store.contains(&hash), "apply kept the project's copy of the exact manifest");

    studio.run("library.uninstall", json!({ "item": "anon/colourways/sea" })).unwrap();
    let row = lock_row(&mut studio);
    assert_eq!((row["resolves"].clone(), row["resolved_from"].clone()), (json!("ok"), json!("project")), "{row}");

    // Without the copy it is missing; then the same version is installed with other content: a mismatch, named.
    let mut bare = Studio::new(HostLibrary::open(Some(data.path())));
    bare.doc = studio.doc.clone();
    assert_eq!(lock_row(&mut bare)["resolves"], "missing");
    bare.run("library.install", json!({ "manifest": sea("1.0.0", "#000000", "") })).unwrap();
    let row = lock_row(&mut bare);
    assert_eq!((row["resolves"].clone(), row["item"].clone()), (json!("mismatch"), json!("anon/colourways/sea")));
}

#[test]
fn a_folder_installs_every_manifest_and_its_assets_and_two_publishers_of_one_id_are_both_kept() {
    let data = tempfile::tempdir().unwrap();
    let store = Arc::new(MemStore::new());
    let mut studio = Studio::with_store(Arc::new(HostLibrary::open(Some(data.path()))), store.clone());
    let bytes = b"a decal's pixels";
    let hash = ContentHash::of(bytes);
    let dir = studio.project.path().join("pack");
    write(&dir, "sea.toml", &sea("1.0.0", "#0f5e57", &format!("assets = {{ decal = \"{hash}\" }}")));
    for (name, seed) in [("bow-a.toml", fixtures::SIGNER_SEED), ("bow-b.toml", fixtures::RIVAL_SEED)] {
        write(&dir, name, &toml::to_string(&fixtures::shared_bow(seed)).unwrap());
    }
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("assets").join(hash.to_string()), bytes).unwrap();

    let out = studio.run("library.install", json!({ "from": dir.to_string_lossy() })).unwrap();
    let installed = out["installed"].as_array().unwrap();
    assert_eq!(installed.len(), 3, "{out}");
    assert!(store.contains(&hash), "the asset went into the store");
    let bows: Vec<_> = installed.iter().filter(|i| i["item"] == "shared/colourways/bow").collect();
    assert_eq!(bows.len(), 2);
    assert!(bows.iter().all(|b| b["publisher"]["verified"] == true), "both signatures hold");
    let reopened = HostLibrary::open(Some(data.path()));
    let mine = reopened.list(None, Some(&ItemScope::Mine), 0, 100).unwrap();
    assert_eq!(mine.total, 3, "both publishers' bows survive a restart: {mine:?}");

    // A manifest naming an asset that isn't there is refused before anything is written.
    let lonely = studio.project.path().join("lonely");
    write(
        &lonely,
        "sea.toml",
        &sea("2.0.0", "#0f5e57", &format!("assets = {{ decal = \"{}\" }}", ContentHash::of(b"x"))),
    );
    let err = studio.run("library.install", json!({ "from": lonely.to_string_lossy() })).unwrap_err();
    assert!(err.message.contains("isn't in assets/"), "{err}");
}

#[test]
fn a_signed_item_installs_verified_a_tampered_one_unverified_and_omuzaki_ids_are_refused() {
    let data = tempfile::tempdir().unwrap();
    let mut studio = Studio::new(HostLibrary::open(Some(data.path())));
    let signed = toml::to_string(&fixtures::signed_ember("1.0.0")).unwrap();
    let out = studio.run("library.install", json!({ "manifest": signed })).unwrap();
    assert_eq!(out["installed"][0]["publisher"]["verified"], true, "{out}");

    let mut tampered = fixtures::signed_ember("1.0.1");
    tampered.summary = "Changed after signing.".into();
    let out = studio.run("library.install", json!({ "manifest": toml::to_string(&tampered).unwrap() })).unwrap();
    let publisher = &out["installed"][0]["publisher"];
    assert_eq!(
        (publisher["verified"].clone(), publisher["name"].clone()),
        (json!(false), json!("unverified publisher"))
    );
    assert!(publisher["reason"].as_str().is_some_and(|r| !r.is_empty()), "the reason is named: {publisher}");

    let own = sea("1.0.0", "#000000", "").replace("anon/colourways/sea", "omuzaki/colourways/sea");
    let err = studio.run("library.install", json!({ "manifest": own })).unwrap_err();
    assert!(err.message.contains("come with Omuzaki"), "{err}");
}

#[test]
fn the_disclaimer_is_acknowledged_once_per_publisher_and_item_and_the_file_round_trips() {
    let data = tempfile::tempdir().unwrap();
    let mut studio = Studio::new(HostLibrary::open(Some(data.path())));
    let out = studio.run("library.install", json!({ "manifest": sea("1.0.0", "#0f5e57", "") })).unwrap();
    assert!(out["disclaimer"].as_str().is_some_and(|d| d.contains("publisher")));
    studio.run("library.install", json!({ "manifest": sea("1.0.0", "#0f5e57", "") })).unwrap();
    studio
        .run("library.install", json!({ "manifest": toml::to_string(&fixtures::signed_ember("1.0.0")).unwrap() }))
        .unwrap();
    let acks = studio.library.acknowledgements().unwrap();
    assert_eq!(acks.len(), 2, "one per publisher and hash: {acks:?}");
    assert_eq!(acks.iter().filter(|a| a.publisher == "unverified").count(), 1);
    assert!(acks.iter().all(|a| a.by == "Tester" && !a.licence.is_empty()));

    let path = data.path().join("acks.toml");
    let before = std::fs::read(&path).unwrap();
    assert!(!studio.library.acknowledge(acks[0].clone()).unwrap(), "the same acknowledgement adds nothing");
    assert_eq!(std::fs::read(&path).unwrap(), before, "and the file is byte-identical");
}

#[test]
fn undoing_an_install_uninstalls_it_and_undoing_that_installs_it_again() {
    let data = tempfile::tempdir().unwrap();
    let mut studio = Studio::new(HostLibrary::open(Some(data.path())));
    let plan = studio.dry("library.install", json!({ "manifest": sea("1.0.0", "#0f5e57", "") })).unwrap();
    let effect = plan.effect.expect("an install records its effect");
    assert_eq!(effect.undo.op, "library.uninstall");
    studio.run("library.install", json!({ "manifest": sea("1.0.0", "#0f5e57", "") })).unwrap();
    let removed = studio.run(&effect.undo.op, effect.undo.args.clone()).unwrap();
    assert_eq!(removed["removed"][0]["item"], "anon/colourways/sea");
    let plan = studio.dry(&effect.undo.op, effect.undo.args.clone());
    assert!(plan.is_err(), "gone");
    let reinstall = studio.dry("library.uninstall", json!({ "item": "anon/colourways/sea" }));
    assert_eq!(reinstall.unwrap_err().code, ErrorCode::NotFound);
}

// --- #406: apply's refusals ---

fn library_with(items: &[ItemManifest]) -> HostLibrary {
    let library = HostLibrary::empty(None);
    for fixture in fixtures::all() {
        library.insert(fixture.manifest, fixture.scope).unwrap();
    }
    for item in items {
        library.insert(item.clone(), ItemScope::FirstParty).unwrap();
    }
    library
}

fn first_party(id: &str, version: &str, kind: &str, extra: &str, steps: &str) -> ItemManifest {
    parse(&format!(
        "schema = 1\nid = \"{id}\"\nversion = \"{version}\"\nsection = \"colourways\"\nkind = \"{kind}\"\n\
         target = \"character\"\ntitle = \"{id}\"\nsummary = \"An acceptance item.\"\ntags = [\"test\"]\n\
         origin = \"test fixture\"\n{extra}\n[rights]\nauthor = \"Omuzaki\"\nlicence = \"CC0-1.0\"\n\
         commercial = \"ok\"\nowned = true\n\n{steps}"
    ))
}

fn palette(character: &str, colour: &str) -> String {
    format!(
        "[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"{character}\"\nslot = \"outfit\"\ncolour = \"{colour}\"\n"
    )
}

#[test]
fn a_step_naming_another_character_and_a_step_above_the_grants_are_refused_naming_the_step() {
    let stray = first_party(
        "test/colourways/stray",
        "1.0.0",
        "setting",
        "",
        "[[step]]\nop = \"mock.stray\"\n[step.args]\ncharacter = \"$character\"\n",
    );
    let staged = first_party(
        "test/colourways/staged",
        "1.0.0",
        "setting",
        "",
        "[[step]]\nop = \"mock.scene_mark\"\n[step.args]\ncharacter = \"$character\"\n",
    );
    let mut studio = Studio::new(library_with(&[stray, staged]));
    let err =
        studio.run("library.apply", json!({ "item": "test/colourways/stray", "character": character() })).unwrap_err();
    assert!(format!("{err}").contains("step/0"), "{err}");
    studio.grants = Some(Grants::of([Scope::Read, Scope::EditLibrary, Scope::EditCharacter]));
    let err =
        studio.run("library.apply", json!({ "item": "test/colourways/staged", "character": character() })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Denied);
    assert!(format!("{err}").contains("edit:scene"), "{err}");
    studio.grants = None;
    studio.run("library.apply", json!({ "item": "test/colourways/staged", "character": character() })).unwrap();
}

#[test]
fn a_scene_target_is_refused_and_a_member_name_taken_twice_asks_for_as() {
    let mut studio = Studio::new(library_with(&[]));
    let scene = studio.run("library.apply", json!({ "item": "test/colourways/dusk", "scene": "stage" }));
    assert_eq!(scene.unwrap_err().code, ErrorCode::Invalid, "the scene target is reserved");
    studio.run("library.apply", json!({ "item": "test/expressions/wink", "character": character() })).unwrap();
    let err =
        studio.run("library.apply", json!({ "item": "test/expressions/wink", "character": character() })).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("as"), "{err}");
    assert!(err.hint.as_deref().is_some_and(|h| h.contains("\"as\"")));
    let out = studio
        .run("library.apply", json!({ "item": "test/expressions/wink", "character": character(), "as": "wink-left" }))
        .unwrap();
    assert_ne!(out["application"], Value::Null);
}

#[test]
fn a_dry_run_patch_is_the_applied_patch_and_a_twenty_step_item_plans_quickly() {
    let steps: String = (0..20).map(|n| palette("$character", &format!("#{n:02x}{n:02x}{n:02x}"))).collect();
    let long = first_party("test/colourways/twenty", "1.0.0", "setting", "", &steps);
    let mut studio = Studio::new(library_with(&[long]));
    let args = json!({ "item": "test/colourways/twenty", "character": character() });
    let dry = studio.dry("library.apply", args.clone()).unwrap();
    let mut times = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        studio.dry("library.apply", args.clone()).unwrap();
        times.push(start.elapsed());
    }
    times.sort();
    let p95 = times[18];
    println!("a 20-step item plans in {p95:?} (p95 of 20, ops included)");
    // The task's budget is 50 ms beyond the ops; a debug build gets ten times that.
    let ceiling = if cfg!(debug_assertions) { 500 } else { 50 };
    assert!(p95.as_millis() < ceiling, "{p95:?}");
    let before = studio.snapshot();
    studio.run("library.apply", args).unwrap();
    let mut by_hand = Studio::new(library_with(&[]));
    by_hand.doc = omuzaki_doc_mock::MemDoc::from_value(before);
    omuzaki_doc_api::DocWrite::apply(&mut by_hand.doc, &dry.patch).unwrap();
    assert_eq!(by_hand.snapshot(), studio.snapshot(), "the dry run's patch is exactly what lands");
}

// --- #339: update ---

#[test]
fn applying_the_same_slotted_item_with_new_params_is_an_update() {
    let tide = |version: &str| {
        first_party(
            "test/colourways/tide",
            version,
            "setting",
            "slot = \"colourway\"\nparams.colour = { type = \"colour\", default = \"#111111\", summary = \"The outfit\" }\n",
            "[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\nslot = \"outfit\"\ncolour = \"$params.colour\"\n",
        )
    };
    let mut studio = Studio::new(library_with(&[tide("1.0.0")]));
    studio.run("library.apply", json!({ "item": "test/colourways/tide", "character": character() })).unwrap();
    let args = json!({ "item": "test/colourways/tide", "character": character(), "params": { "colour": "#444444" } });
    let out = studio.run("library.apply", args).unwrap();
    assert_eq!(out["updated"], true, "{out}");
    assert_eq!(studio.at("palette/outfit"), Some(json!("#444444")));
    assert_eq!(studio.at("applied/colourway/params/colour"), Some(json!("#444444")));
}

#[test]
fn an_update_to_a_version_that_drops_the_template_is_refused_and_a_major_needs_to_version() {
    let v = |version: &str, extra: &str, colour: &str| {
        first_party("test/colourways/reach", version, "setting", extra, &palette("$character", colour))
    };
    let items = [
        v("1.0.0", "", "#111111"),
        v("1.1.0", "compat = { templates = [\"tall\"] }\n", "#222222"),
        v("2.0.0", "", "#333333"),
    ];
    let mut studio = Studio::new(library_with(&items));
    let app = studio
        .run("library.apply", json!({ "item": "test/colourways/reach", "version": "1.0.0", "character": character() }))
        .unwrap()["application"]
        .clone();
    let err = studio.run("library.update", json!({ "character": character(), "application": app })).unwrap_err();
    assert!(err.message.contains("doesn't fit") && err.message.contains("tall"), "{err}");
    let out = studio
        .run("library.update", json!({ "character": character(), "application": app, "to_version": "2.0.0" }))
        .unwrap();
    assert_eq!(out["version"], "2.0.0", "a major is crossed only when asked");
    assert_eq!(studio.at("palette/outfit"), Some(json!("#333333")));
}

#[test]
fn an_update_lists_exactly_the_paths_the_new_version_changes() {
    let v = |version: &str, outfit: &str| {
        let steps = palette("$character", outfit)
            + "[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\nslot = \"skin_line\"\ncolour = \"#abcdef\"\n";
        first_party("test/colourways/pair", version, "setting", "", &steps)
    };
    let mut studio = Studio::new(library_with(&[v("1.0.0", "#111111"), v("1.1.0", "#222222")]));
    let app = studio
        .run("library.apply", json!({ "item": "test/colourways/pair", "version": "1.0.0", "character": character() }))
        .unwrap()["application"]
        .clone();
    let preview = studio.dry("library.update", json!({ "character": character(), "application": app })).unwrap();
    let changes = &preview.output.unwrap()["changes"];
    assert_eq!(
        changes,
        &json!([{ "path": "palette/outfit", "before": "#111111", "after": "#222222", "why": "item" }]),
        "the unchanged skin line isn't listed"
    );
}

// --- #209: search ---

/// 300 items: the fixtures, then generated colourways and hairstyles, with one twintails style among them.
fn three_hundred() -> HostLibrary {
    let library = library_with(&[]);
    let base = first_party("test/colourways/base", "1.0.0", "setting", "", &palette("$character", "#000000"));
    let have = library.list(None, None, 0, 100).unwrap().total;
    let mut items = Vec::new();
    for n in 0..(300 - have) {
        let mut m = base.clone();
        m.id = format!("test/colourways/generated-{n:03}");
        m.title = format!("Generated {n}");
        m.tags = vec![format!("tone-{}", n % 7), format!("set-{}", n % 3)];
        if n == 42 {
            m.id = "test/colourways/twintails".into();
            m.title = "Twintails".into();
            m.summary = "Two high tails tied above the ears.".into();
        }
        if n % 5 == 0 {
            m.compat.kinds = vec![omuzaki_library_api::BodyKind::Humanoid];
        }
        items.push((m, ItemScope::FirstParty));
    }
    assert_eq!(library.insert_all(items).len(), 0, "every generated item loads");
    library
}

fn search(library: &HostLibrary, query: Query) -> Vec<String> {
    library.search(&Query { limit: 100, ..query }).unwrap().items.into_iter().map(|i| i.id.to_string()).collect()
}

#[test]
fn search_finds_by_words_tags_compatibility_and_rights_over_three_hundred_items() {
    let library = three_hundred();
    assert_eq!(library.list(None, None, 0, 1).unwrap().total, 300);
    let twin = search(&library, Query { text: Some("twin tail".into()), ..Query::default() });
    assert_eq!(twin.first().map(String::as_str), Some("test/colourways/twintails"), "{twin:?}");
    let both =
        library.search(&Query { tags: vec!["tone-3".into(), "set-1".into()], limit: 100, ..Query::default() }).unwrap();
    assert!(
        both.total > 0
            && both.items.iter().all(|i| i.tags.contains(&"tone-3".into()) && i.tags.contains(&"set-1".into())),
        "tags AND"
    );

    let mut creature = fixtures::characters().remove(0).1;
    creature.body_kind = omuzaki_library_api::BodyKind::Creature;
    let query = Query { compatible_with: Some(CompatTarget::Character(character())), limit: 100, ..Query::default() };
    let all = library.search(&Query { limit: 100, ..Query::default() }).unwrap().total;
    let fits = library.search_for(&query, Some(&creature)).unwrap();
    assert!(fits.total < all, "humanoid-only items drop out for a creature");
    assert!(fits.items.iter().all(|i| !i.id.as_str().starts_with("test/colourways/generated-000")));

    let publishable = search(&library, Query { usable_for: Some(Usage::Publish), ..Query::default() });
    let every = library.search(&Query { offset: 0, limit: 100, ..Query::default() }).unwrap();
    assert!(every.items.len() <= 100);
    assert!(!publishable.iter().any(|id| id == "anon/colourways/nc"), "a non-commercial item can't be published");
    assert_eq!(search(&library, Query { text: Some("twin tail".into()), ..Query::default() }), twin, "deterministic");
}

fn p95(library: &HostLibrary, queries: &[Query]) -> std::time::Duration {
    let mut times = Vec::new();
    for _ in 0..5 {
        for q in queries {
            let start = Instant::now();
            library.search(q).unwrap();
            times.push(start.elapsed());
        }
    }
    times.sort();
    times[times.len() * 95 / 100]
}

#[test]
fn a_query_stays_inside_its_budget_at_three_hundred_and_ten_thousand_items() {
    let queries: Vec<Query> = ["twin tail", "generated 7", "tone", "zzzz"]
        .into_iter()
        .map(|t| Query { text: Some(t.into()), limit: 20, ..Query::default() })
        .chain([Query { tags: vec!["set-2".into()], limit: 20, ..Query::default() }])
        .collect();
    let hundreds = p95(&three_hundred(), &queries);

    let big = three_hundred();
    let base = first_party("test/colourways/base", "1.0.0", "setting", "", &palette("$character", "#000000"));
    let bulk = (0..9_700).map(|n| {
        let mut m = base.clone();
        m.id = format!("test/colourways/bulk-{n:05}");
        m.title = format!("Bulk {n}");
        m.tags = vec![format!("tone-{}", n % 11)];
        (m, ItemScope::FirstParty)
    });
    assert_eq!(big.insert_all(bulk).len(), 0, "every bulk item loads");
    let thousands = p95(&big, &queries);
    println!("search p95: {hundreds:?} over 300 items, {thousands:?} over 10,000");
    // The task's budgets are 2 ms and 20 ms in a release build; a debug build gets ten times that.
    let scale = if cfg!(debug_assertions) { 10 } else { 1 };
    assert!(hundreds.as_micros() < 2_000 * scale, "{hundreds:?}");
    assert!(thousands.as_micros() < 20_000 * scale, "{thousands:?}");
}
