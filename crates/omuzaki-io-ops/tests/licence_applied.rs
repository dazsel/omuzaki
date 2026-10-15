//! The library's application records (`characters/<id>/applied/<app>`) in `licence.derive`, `licence.describe` and
//! `io.check_export`: the rights an applied item carries are joined like a source row's, an item that is not cleared
//! blocks `publish` at its record.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_char_mock::mock_character;
use omuzaki_core::time::FakeClock;
use std::collections::BTreeMap;

use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::UseTerms;
use omuzaki_io_api::{Finding, Severity};
use omuzaki_io_mock::{MockGlbExporter, MockVrm1Exporter};
use omuzaki_io_ops::gather::avatar::LicenceRecord;
use omuzaki_io_ops::ops::check_export::ExportCheck;
use omuzaki_io_ops::ops::licence::derive::DerivedLicence;
use omuzaki_io_ops::ops::licence::describe::LicenceDescription;
use omuzaki_io_ops::ops::licence::source::{Ownership, PartSource};
use omuzaki_library_api::{ApplicationRecord, ItemId, ItemRights, RECORD_SCHEMA, RecordBindings, Resolved, Version};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, find};
use omuzaki_store_api::{Commercial, ContentHash};
use serde_json::{Value, json};

const PARTS: [&str; 5] = ["body", "hair", "head", "shirt", "vest"];

fn application(item: &str, rights: ItemRights) -> ApplicationRecord {
    ApplicationRecord {
        schema: RECORD_SCHEMA,
        item: ItemId::new(item).unwrap(),
        version: Version::new(1, 0, 0),
        hash: ContentHash::of(item.as_bytes()),
        publisher: None,
        from: Resolved::FirstParty,
        slot: None,
        bindings: RecordBindings::default(),
        params: BTreeMap::new(),
        deps: BTreeMap::new(),
        rights,
        created: Vec::new(),
        wrote: BTreeMap::new(),
    }
}

/// Rights of a fixture item that is not cleared for commercial use.
fn non_commercial() -> ItemRights {
    ItemRights { licence: "CC-BY-NC-4.0".into(), commercial: Commercial::No, owned: false, ..ItemRights::first_party() }
}

/// A part row as the library writes it: the item's rights, owned when the item says so.
fn row_of(rights: &ItemRights) -> PartSource {
    let id = ItemId::new("omuzaki/kit/part").unwrap();
    let mut row = PartSource::new(rights.to_provenance(&id, &Version::new(1, 0, 0)));
    row.owned = rights.owned.then(|| Ownership { asserted_by: "Omuzaki first-party library item".into() });
    row
}

/// The mock character with a row from `rights` on every part and `applied` records under their keys.
fn doc(rights: &ItemRights, applied: &[(&str, Value)]) -> MemDoc {
    let mut root = json!({ "characters": { "mira": serde_json::to_value(mock_character().unwrap()).unwrap() } });
    for part in PARTS {
        root["characters"]["mira"]["sources"][part] = serde_json::to_value(row_of(rights)).unwrap();
    }
    for (app, node) in applied {
        root["characters"]["mira"]["applied"][app] = node.clone();
    }
    MemDoc::from_value(root)
}

fn node(item: &str, rights: ItemRights) -> Value {
    application(item, rights).to_node().unwrap()
}

fn plan(doc: &MemDoc, op: &str, args: Value) -> Result<Plan, OpError> {
    let decoded = (find(op).unwrap_or_else(|| panic!("{op} is not registered")).decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    decoded.plan(&OpCtx::new(doc, &actor, &clock).with_dry_run(true))
}

fn answer<T: serde::de::DeserializeOwned>(plan: Plan) -> T {
    serde_json::from_value(plan.output.expect("an answer")).unwrap()
}

fn derive(doc: &MemDoc) -> DerivedLicence {
    answer(plan(doc, "licence.derive", json!({ "character": "mira" })).unwrap())
}

fn describe(doc: &MemDoc, purpose: &str) -> LicenceDescription {
    answer(plan(doc, "licence.describe", json!({ "character": "mira", "purpose": purpose })).unwrap())
}

fn check(doc: &MemDoc, purpose: &str) -> ExportCheck {
    let _ = (MockGlbExporter, MockVrm1Exporter);
    answer(plan(doc, "io.check_export", json!({ "character": "mira", "format": "glb", "purpose": purpose })).unwrap())
}

/// Stores what `licence.derive` says, as the person would with `licence.set`.
fn with_derived_record(mut doc: MemDoc) -> MemDoc {
    let mut record: LicenceRecord = derive(&doc).record;
    record.meta.authors = vec!["Mira".into()];
    let args = json!({ "character": "mira", "record": serde_json::to_value(&record).unwrap() });
    let planned = plan(&doc, "licence.set", args).unwrap_or_else(|e| panic!("{e:?}"));
    doc.apply(&planned.patch).unwrap();
    doc
}

fn rule<'a>(findings: &'a [Finding], rule: &str) -> Vec<&'a Finding> {
    findings.iter().filter(|f| f.rule == rule).collect()
}

#[test]
fn a_non_commercial_face_item_fails_publish_at_its_record_and_private_passes() {
    // Every part is the project's own; only the face was set by a fixture item that forbids commercial use.
    let owned_rights = ItemRights::first_party();
    let doc = with_derived_record(doc(&owned_rights, &[("face", node("test/face-style", non_commercial()))]));

    let derived = derive(&doc);
    assert_eq!(derived.record.provenance.commercial, Commercial::No);
    assert!(!derived.project_owned, "the item does not say the result is the applier's own");
    assert_eq!(derived.applications.len(), 1);
    assert_eq!(derived.applications[0].path, "characters/mira/applied/face");

    let publish = check(&doc, "publish");
    assert!(!publish.ok);
    let found = rule(&publish.report.findings, "rights.item_not_cleared");
    assert_eq!(found.len(), 1, "{:#?}", publish.report);
    assert_eq!(found[0].severity, Severity::Error);
    assert_eq!(found[0].location.as_deref(), Some("characters/mira/applied/face"));
    assert!(found[0].message.contains("test/face-style@1.0.0") && found[0].message.contains("CC-BY-NC-4.0"));
    assert!(found[0].hint.as_deref().is_some_and(|h| h.contains("apply a cleared item in the same slot")));

    let private = check(&doc, "private");
    assert!(private.ok, "{:#?}", private.report);
    assert_eq!(rule(&private.report.findings, "rights.item_not_cleared").len(), 0);
}

#[test]
fn an_unknown_status_and_private_test_material_are_not_cleared_either() {
    for rights in [
        ItemRights { commercial: Commercial::Unknown, owned: false, ..ItemRights::first_party() },
        ItemRights { private_test: true, owned: false, ..ItemRights::first_party() },
    ] {
        let doc = doc(&ItemRights::first_party(), &[("hat", node("test/hat", rights))]);
        let publish = check(&doc, "publish");
        let found = rule(&publish.report.findings, "rights.item_not_cleared");
        assert_eq!(found.len(), 1, "{:#?}", publish.report);
        assert_eq!(found[0].location.as_deref(), Some("characters/mira/applied/hat"));
        assert!(!publish.ok);
    }
}

#[test]
fn check_export_and_describe_agree_about_the_applied_items() {
    let applied = [
        ("face", node("test/face-style", non_commercial())),
        ("sash", node("omuzaki/kit/sash", ItemRights::first_party())),
    ];
    let doc = with_derived_record(doc(&ItemRights::first_party(), &applied));
    for purpose in ["publish", "private"] {
        let (export, described) = (check(&doc, purpose), describe(&doc, purpose));
        let key = |f: &&Finding| (f.severity, f.location.clone(), f.message.clone());
        let mut a: Vec<_> = rule(&export.report.findings, "rights.item_not_cleared").iter().map(key).collect();
        let mut b: Vec<_> = rule(&described.report.findings, "rights.item_not_cleared").iter().map(key).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "{purpose}");
        assert_eq!(export.ok, described.ok, "{purpose}");
    }
}

#[test]
fn a_character_of_first_party_items_publishes_without_findings_about_them() {
    let rights = ItemRights::first_party();
    let applied = [("hair-style", node("omuzaki/hair/twintails", rights.clone()))];
    let doc = with_derived_record(doc(&rights, &applied));
    let derived = derive(&doc);
    assert!(derived.complete && derived.project_owned, "{:#?}", derived.report);
    assert_eq!(derived.record.provenance.licence, "CC0-1.0");
    let publish = check(&doc, "publish");
    assert!(publish.ok, "{:#?}", publish.report);
    assert_eq!(rule(&publish.report.findings, "rights.item_not_cleared").len(), 0);
    assert!(describe(&doc, "publish").ok);
}

#[test]
fn a_whitespace_only_licence_is_an_error_in_derive_describe_and_check_export() {
    let blank = ItemRights { licence: "  \t".into(), ..ItemRights::first_party() };
    let doc = doc(&ItemRights::first_party(), &[("face", node("test/face-style", blank))]);
    let input = json!({ "character": "mira" });
    let derive_error = plan(&doc, "licence.derive", input.clone()).expect_err("blank licence");
    assert_eq!(derive_error.field.as_deref(), Some("applied/face"));
    assert!(derive_error.message.contains("characters/mira/applied/face"), "{}", derive_error.message);
    assert!(derive_error.message.contains("blank"), "{}", derive_error.message);
    let describe_error = plan(&doc, "licence.describe", json!({ "character": "mira", "purpose": "publish" }))
        .expect_err("describe refuses too");
    assert_eq!(describe_error.field.as_deref(), Some("applied/face"));
    let export_error =
        plan(&doc, "io.check_export", json!({ "character": "mira", "format": "glb", "purpose": "publish" }))
            .expect_err("check_export refuses too");
    assert_eq!(export_error.field.as_deref(), Some("applied/face"));
}

#[test]
fn an_owned_first_party_record_does_not_change_what_derive_says() {
    let rights = ItemRights::first_party();
    let without = derive(&doc(&rights, &[]));
    let with = derive(&doc(&rights, &[("hair-style", node("omuzaki/hair/twintails", rights.clone()))]));
    assert_eq!(with.record.provenance.licence, without.record.provenance.licence);
    assert_eq!(with.record.provenance.commercial, without.record.provenance.commercial);
    assert_eq!(with.record.meta.terms, without.record.meta.terms);
    assert_eq!(with.record.meta.authors, without.record.meta.authors);
    assert!(with.project_owned && without.project_owned);
    assert_eq!(with.applications[0].item, "omuzaki/hair/twintails");
    assert_eq!(with.applications[0].version, "1.0.0");
}

#[test]
fn a_record_counts_as_owned_only_when_its_rights_carry_owned() {
    let rights = ItemRights::first_party();
    let not_owned = ItemRights { owned: false, ..rights.clone() };
    let doc_not_owned = doc(&rights, &[("face", node("omuzaki/face/smile", not_owned))]);
    assert!(!derive(&doc_not_owned).project_owned);
    let doc_owned = doc(&rights, &[("face", node("omuzaki/face/smile", rights.clone()))]);
    let derived = derive(&doc_owned);
    assert!(derived.project_owned);
    assert!(derived.owners.iter().any(|o| o.contains("omuzaki/face/smile@1.0.0")), "{:?}", derived.owners);
}

#[test]
fn the_strictest_of_a_record_and_the_parts_wins_and_its_credit_travels() {
    let mut rights =
        ItemRights { attribution: Some("Smile by Eve".into()), author: Some("Eve".into()), ..non_commercial() };
    rights.licence = "CC-BY-NC-4.0".into();
    let derived = derive(&doc(&ItemRights::first_party(), &[("face", node("test/face-style", rights))]));
    assert_eq!(derived.record.provenance.commercial, Commercial::No);
    assert_eq!(derived.record.provenance.licence, "CC-BY-NC-4.0 AND CC0-1.0");
    assert_eq!(derived.attributions, ["Smile by Eve"]);
    assert_eq!(derived.record.meta.authors, ["Omuzaki", "Eve"]);
}

#[test]
fn a_record_that_does_not_read_is_an_error_naming_its_path() {
    let mut broken = node("test/face-style", non_commercial());
    broken["surprise"] = json!(1);
    let doc = doc(&ItemRights::first_party(), &[("face", broken)]);
    for (op, args) in [
        ("licence.derive", json!({ "character": "mira" })),
        ("licence.describe", json!({ "character": "mira", "purpose": "publish" })),
        ("io.check_export", json!({ "character": "mira", "format": "glb", "purpose": "publish" })),
    ] {
        let error = plan(&doc, op, args).err().unwrap_or_else(|| panic!("{op} should refuse"));
        assert_eq!(error.code, ErrorCode::Invalid, "{op}");
        assert_eq!(error.field.as_deref(), Some("applied/face"), "{op}");
        assert!(error.message.contains("characters/mira/applied/face"), "{op}: {}", error.message);
    }
}

#[test]
fn an_applied_node_that_is_not_a_table_is_an_error_not_no_applications() {
    let doc = doc(&ItemRights::first_party(), &[("face", json!("not a record"))]);
    let error = plan(&doc, "licence.derive", json!({ "character": "mira" })).expect_err("a string is not a record");
    assert_eq!(error.field.as_deref(), Some("applied/face"));
    let mut root = doc.root().clone();
    root["characters"]["mira"]["applied"] = json!("oops");
    let error =
        plan(&MemDoc::from_value(root), "licence.derive", json!({ "character": "mira" })).expect_err("not a table");
    assert_eq!(error.field.as_deref(), Some("applied"));
}

#[test]
fn a_character_with_no_parts_keeps_the_most_restrictive_floor_whatever_it_has_applied() {
    let rights = ItemRights::first_party();
    let mut root = json!({ "characters": { "mira": serde_json::to_value(mock_character().unwrap()).unwrap() } });
    root["characters"]["mira"]["parts"] = json!({});
    root["characters"]["mira"]["applied"]["hair-style"] = node("omuzaki/hair/twintails", rights);
    let derived = derive(&MemDoc::from_value(root));
    assert!(derived.parts.is_empty() && derived.applications.len() == 1);
    assert_eq!(derived.record.provenance.commercial, Commercial::Unknown);
    assert!(derived.record.provenance.licence.contains("LicenseRef-Unknown"), "{}", derived.record.provenance.licence);
    assert!(!derived.project_owned && !derived.complete);
    assert_eq!(derived.record.meta.terms, UseTerms::default());
}
