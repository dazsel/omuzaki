//! The starter characters (`io.starter.list`, `io.starter.fetch`): the shipped catalogue holds only licences that let
//! anyone perform as, edit and export a starter; a fetch checks the model against its pin and stores it under the
//! catalogue's record; and the avatar's source row it gives can be recorded on an imported avatar. No third-party
//! model: the archive here is made by the test.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::path::PathBuf;

use omuzaki_ai_api::{FetchError, FileSource, NoProgress, PinnedFile, Progress, Sha256};
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{CommercialUsage, CreditNotation, Modification, UseTerms};
use omuzaki_io_ops::ops::starter::{Starter, catalogue, fetch, parse_catalogue};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, OpKind, Plan, Scope, Undo, find};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};
use sha2::Digest as _;

fn sha(bytes: &[u8]) -> Sha256 {
    Sha256(sha2::Sha256::digest(bytes).into())
}

/// A "download" that is already on disk: hands back the file whose hash the pin names, as the verified cache would.
struct OnDisk(Vec<PathBuf>);

impl FileSource for OnDisk {
    fn get(&self, pin: &PinnedFile, _progress: &dyn Progress) -> Result<PathBuf, FetchError> {
        let found = self.0.iter().find(|p| sha(&std::fs::read(p).unwrap()) == pin.sha256);
        found.cloned().ok_or_else(|| FetchError::AllFailed(Vec::new()))
    }
}

fn zip_of(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap().into_inner()
}

/// The catalogue entry of the shipped list with its download replaced by `archive` holding `member` = `model`.
fn starter_for(archive: &[u8], member: &str, model: &[u8]) -> Starter {
    let mut starter = catalogue().unwrap().iter().find(|s| s.id == "vroid-vita").unwrap().clone();
    let d = &mut starter.download;
    d.urls = vec!["https://example.invalid/starter.zip".into()];
    d.sha256 = sha(archive);
    d.size = archive.len() as u64;
    d.member = Some(member.into());
    d.member_sha256 = Some(sha(model));
    d.member_size = Some(model.len() as u64);
    starter
}

fn plan(doc: &MemDoc, op: &str, args: Value) -> Result<Plan, OpError> {
    let decoded = (find(op).unwrap_or_else(|| panic!("{op} is not registered")).decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    decoded.plan(&OpCtx::new(doc, &actor, &clock).with_dry_run(true))
}

#[test]
fn the_shipped_catalogue_lists_only_starters_anyone_may_perform_edit_and_export() {
    let all = catalogue().expect("the shipped catalogue parses and passes its checks");
    assert_eq!(all.len(), 7, "six CC0 VRoid characters and Seed-san");
    for starter in all {
        let t = &starter.terms;
        assert_eq!(t.commercial_usage, CommercialUsage::Corporation, "{}", starter.id);
        assert_eq!(t.modification, Modification::AllowModificationRedistribution, "{}", starter.id);
        assert!(t.allow_redistribution, "{}", starter.id);
        assert_eq!(starter.provenance().commercial, Commercial::Ok);
        assert!(starter.download.urls.iter().all(|u| u.starts_with("https://")));
        assert!(starter.file_name().ends_with(".vrm"), "{}: {}", starter.id, starter.file_name());
    }
    // Credit travels with the one starter whose licence asks for it, and only that one.
    let credited: Vec<&str> = all.iter().filter(|s| s.attribution.is_some()).map(|s| s.id.as_str()).collect();
    assert_eq!(credited, ["seed-san"]);
    let cc0 = all.iter().filter(|s| s.licence == "CC0-1.0").count();
    assert_eq!(cc0, 6);
}

#[test]
fn a_catalogue_that_would_ship_something_it_may_not_is_refused() {
    let shipped = include_str!("../data/starter-characters.toml");
    let first_entry_end = shipped.find("\n[[starter]]\nid = \"vroid-bibi\"").unwrap();
    let one = &shipped[..first_entry_end];
    parse_catalogue(one).expect("one entry parses");
    let refused = |text: String, needle: &str| {
        let error = parse_catalogue(&text).expect_err("refused");
        assert!(error.contains(needle), "{error}");
    };
    refused(format!("{one}\n{}", &one[one.find("[[starter]]").unwrap()..]), "listed twice");
    refused(one.replace("https://opengameart", "http://opengameart"), "https");
    refused(one.replace("modification = \"allowModificationRedistribution\"", "modification = \"prohibited\""), "must allow");
    refused(one.replace("allowRedistribution = true", "allowRedistribution = false"), "must allow");
    refused(one.replace("commercialUsage = \"corporation\"", "commercialUsage = \"personalNonProfit\""), "must allow");
    refused(one.replace("creditNotation = \"unnecessary\"", "creditNotation = \"required\""), "attribution");
    refused(one.replace(", member_size = 16851352", ""), "member");
    refused(one.replace("id = \"vroid-shibu\"", "id = \"Vroid Shibu\""), "lowercase");
}

#[test]
fn a_fetch_stores_the_pinned_member_under_the_catalogue_record_and_says_what_comes_next() {
    let dir = tempfile::tempdir().unwrap();
    let model = b"glTF a made-up model, long enough to be compressed a little: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec();
    let archive = zip_of("Vita.vrm", &model);
    let path = dir.path().join("starter.zip");
    std::fs::write(&path, &archive).unwrap();
    let starter = starter_for(&archive, "Vita.vrm", &model);
    let store = MemStore::new();

    let fetched = fetch(&starter, "vita", &OnDisk(vec![path]), &store, &NoProgress).expect("fetches");
    assert_eq!(fetched.asset, ContentHash::of(&model));
    assert_eq!(store.get(&fetched.asset).unwrap(), model, "the member's bytes, not the archive's");
    let record = store.provenance(&fetched.asset).unwrap();
    assert_eq!((record.licence.as_str(), record.commercial), ("CC0-1.0", Commercial::Ok));
    assert!(!fetched.record_kept);
    assert_eq!(fetched.credit, None);

    let ops: Vec<&str> = fetched.next.iter().map(|step| step["op"].as_str().unwrap()).collect();
    assert_eq!(ops, ["io.inspect", "io.import", "licence.set_source"]);
    let row = &fetched.next[2]["args"];
    assert_eq!((row["character"].as_str(), row["part"].as_str()), (Some("vita"), Some("avatar")));
    assert_eq!(row["source"]["terms"]["modification"], "allowModificationRedistribution");
    assert_eq!(fetched.next[1]["args"]["asset"], json!(fetched.asset));
}

#[test]
fn a_model_already_stored_from_a_file_keeps_its_first_record_and_the_fetch_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let model = b"glTF the same bytes the person imported earlier".to_vec();
    let archive = zip_of("Vita.vrm", &model);
    let path = dir.path().join("starter.zip");
    std::fs::write(&path, &archive).unwrap();
    let store = MemStore::new();
    let earlier = Provenance {
        source: Source::Imported { file: "Vita.vrm".into(), origin: None },
        author: None,
        licence: "LicenseRef-VRM-Other".into(),
        attribution: None,
        commercial: Commercial::Unknown,
        consent: None,
        private_test: false,
    };
    store.put(&model, earlier.clone()).unwrap();
    let fetched =
        fetch(&starter_for(&archive, "Vita.vrm", &model), "vita", &OnDisk(vec![path]), &store, &NoProgress).unwrap();
    assert!(fetched.record_kept);
    assert_eq!(fetched.provenance, earlier);
}

#[test]
fn an_archive_whose_member_is_not_the_pinned_model_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let model = b"glTF the model the catalogue reviewed".to_vec();
    let swapped = b"glTF something else under the same name".to_vec();
    let archive = zip_of("Vita.vrm", &swapped);
    let path = dir.path().join("starter.zip");
    std::fs::write(&path, &archive).unwrap();
    let mut starter = starter_for(&archive, "Vita.vrm", &model);
    starter.download.member_size = Some(swapped.len() as u64);
    let store = MemStore::new();

    let error = fetch(&starter, "vita", &OnDisk(vec![path.clone()]), &store, &NoProgress).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("id")), "{error:?}");
    assert!(error.message.contains("SHA-256"), "{}", error.message);
    assert!(!store.contains(&ContentHash::of(&swapped)));

    starter.download.member = Some("Other.vrm".into());
    let error = fetch(&starter, "vita", &OnDisk(vec![path]), &store, &NoProgress).unwrap_err();
    assert!(error.message.contains("has no `Other.vrm`"), "{}", error.message);
}

#[test]
fn a_failed_download_is_unavailable_and_says_what_to_do() {
    let store = MemStore::new();
    let starter = catalogue().unwrap()[0].clone();
    let error = fetch(&starter, "shibu", &OnDisk(Vec::new()), &store, &NoProgress).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(error.hint.as_deref().unwrap().contains("import a VRM file of your own"), "{error:?}");
}

#[test]
fn the_ops_are_a_read_query_and_a_job_that_changes_no_document() {
    let list = (find("io.starter.list").unwrap().meta)();
    assert_eq!((list.kind, list.scope), (OpKind::Query, Scope::Read));
    let fetch = (find("io.starter.fetch").unwrap().meta)();
    assert_eq!((fetch.kind, fetch.undo), (OpKind::Job, Undo::None));

    let doc = MemDoc::from_value(json!({}));
    let listed = plan(&doc, "io.starter.list", json!({})).unwrap().output.unwrap();
    let rows = listed["starters"].as_array().unwrap();
    assert_eq!(rows.len(), 7);
    assert_eq!(rows.iter().find(|r| r["id"] == "seed-san").unwrap()["credit"].as_str().map(|c| c.contains("VirtualCast")), Some(true));

    let error = plan(&doc, "io.starter.fetch", json!({ "id": "nobody" })).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::NotFound, Some("id")));
    assert!(error.hint.as_deref().unwrap().contains("vroid-vita"), "{error:?}");
    // Without the download cache and the store, the plan says so rather than failing in the job.
    let error = plan(&doc, "io.starter.fetch", json!({ "id": "vroid-vita" })).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable);
}

#[test]
fn the_row_a_fetch_gives_is_recorded_on_an_imported_avatar() {
    let def = AvatarDef::new(AvatarSource::new(ContentHash::of(b"model"), SourceFormat::Vrm0, "Vita.vrm"));
    let meta_terms = UseTerms::default();
    let doc = MemDoc::from_value(json!({ "characters": { "vita": {
        "name": "Vita",
        "avatar": serde_json::to_value(&def).unwrap(),
        "sources": { "avatar": { "schema": 1, "provenance": {
            "source": { "kind": "imported", "file": "Vita.vrm", "origin": null },
            "author": null, "licence": "LicenseRef-VRM-Other", "attribution": null,
            "commercial": "unknown", "consent": null, "private_test": false
        }, "terms": serde_json::to_value(&meta_terms).unwrap() } }
    } } }));
    let starter = catalogue().unwrap().iter().find(|s| s.id == "vroid-vita").unwrap();
    let args = json!({ "character": "vita", "part": "avatar", "source": serde_json::to_value(starter.source_row()).unwrap() });
    let planned = plan(&doc, "licence.set_source", args).expect("an imported avatar's row can be corrected");
    assert!(!planned.patch.is_empty());
    let derived = plan(&doc, "licence.derive", json!({ "character": "vita" })).unwrap();
    assert!(derived.output.is_some());
    // A character that is not an avatar still has no `avatar` part.
    let kit = MemDoc::from_value(json!({ "characters": { "kit": { "name": "Kit", "parts": {} } } }));
    let args = json!({ "character": "kit", "part": "avatar", "source": serde_json::to_value(starter.source_row()).unwrap() });
    assert_eq!(plan(&kit, "licence.set_source", args).unwrap_err().code, ErrorCode::NotFound);
    let _ = CreditNotation::Unnecessary;
}
