//! `licence.set_source` and the part source rows, against the mock character.
//!
//! The ops are decoded from JSON and planned exactly as an executor would (`omuzaki-ops` is an implementation crate, so it
//! is not a dependency here); a plan's patch is applied to a `MemDoc` and undone with `Patch::inverse`. The part ops
//! that move a row (`part.duplicate`, `part.remove`, `part.add`) are in `omuzaki-char`, which this crate may not depend on,
//! so their tests (`omuzaki-char/tests/ops_character.rs`) check the rows they leave and this file checks what
//! `licence.derive` makes of rows in that shape.
//!
//! The harness is small and local rather than shared with `licence.rs`, which stays untouched here.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_char_mock::mock_character;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{AvatarPermission, CommercialUsage, Modification, UseTerms};
use omuzaki_io_mock::{MockGlbExporter, MockVrm1Exporter};
use omuzaki_io_ops::ops::check_export::ExportCheck;
use omuzaki_io_ops::ops::licence::derive::DerivedLicence;
use omuzaki_io_ops::ops::licence::describe::LicenceDescription;
use omuzaki_io_ops::ops::licence::source::{Ownership, PartSource};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, OpKind, Plan, Scope, Undo, find};
use omuzaki_store_api::{Commercial, Provenance, Source};
use serde_json::{Value, json};

/// The mock character's parts, in id order.
const PARTS: [&str; 5] = ["body", "hair", "head", "shirt", "vest"];

// ---------------------------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------------------------

/// A row for a CC0 part by `author` that grants what `terms` says.
fn row(commercial: Commercial, author: &str, terms: UseTerms) -> PartSource {
    let provenance = Provenance {
        source: Source::Library { item: format!("{author}-item@1") },
        author: Some(author.into()),
        licence: "CC0-1.0".into(),
        attribution: None,
        commercial,
        consent: None,
        private_test: false,
    };
    PartSource { terms: Some(terms), ..PartSource::new(provenance) }
}

fn grants_profit() -> UseTerms {
    UseTerms {
        avatar_permission: AvatarPermission::Everyone,
        commercial_usage: CommercialUsage::PersonalProfit,
        allow_redistribution: true,
        modification: Modification::AllowModification,
        ..UseTerms::default()
    }
}

/// A row for every part, as a person would record them.
fn all_rows() -> Vec<(&'static str, PartSource)> {
    PARTS.iter().map(|part| (*part, row(Commercial::Ok, "Ann", grants_profit()))).collect()
}

/// The mock character with the given rows written by hand, as a project file holds them.
fn doc_with(rows: &[(&str, PartSource)]) -> MemDoc {
    let mut root = json!({ "characters": { "mira": serde_json::to_value(mock_character().unwrap()).unwrap() } });
    for (part, source) in rows {
        root["characters"]["mira"]["sources"][part] = serde_json::to_value(source).unwrap();
    }
    MemDoc::from_value(root)
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

fn args(part: &str, source: &PartSource) -> Value {
    json!({ "character": "mira", "part": part, "source": serde_json::to_value(source).unwrap() })
}

fn set_source(doc: &MemDoc, part: &str, source: &PartSource) -> Result<Plan, OpError> {
    plan(doc, "licence.set_source", args(part, source))
}

/// Plans `licence.set_source` and applies the patch, as the executor does.
fn record(doc: &mut MemDoc, part: &str, source: &PartSource) -> Plan {
    let planned = set_source(doc, part, source).unwrap_or_else(|e| panic!("{e:?}"));
    doc.apply(&planned.patch).unwrap();
    planned
}

/// The refusal for `part` and `source`, with the class and the field it names checked and a hint present.
#[track_caller]
fn refusal(doc: &MemDoc, part: &str, source: &PartSource, code: ErrorCode, field: &str) -> OpError {
    let error = set_source(doc, part, source).expect_err("the row should be refused");
    assert_eq!((error.code, error.field.as_deref()), (code, Some(field)), "{error:?}");
    assert!(error.hint.is_some(), "every refusal says how to fix it: {error:?}");
    error
}

fn notes(planned: &Plan) -> Vec<String> {
    planned.output.as_ref().unwrap()["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note.as_str().unwrap().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// The op
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn it_is_an_undoable_character_edit_with_a_flat_closed_schema() {
    let meta = (find("licence.set_source").expect("registered").meta)();
    assert_eq!((meta.kind, meta.scope, meta.undo), (OpKind::Edit, Scope::EditCharacter, Undo::Auto));
    let schema = serde_json::to_value((find("licence.set_source").unwrap().schema)()).unwrap();
    assert_eq!(schema["required"], json!(["character", "part", "source"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    assert!(!schema.to_string().contains("$ref"), "MCP tool schemas are inlined");
    // The row is documented down to the fields an agent has to set.
    assert!(
        schema["properties"]["source"]["properties"]["provenance"]["properties"]["licence"].is_object(),
        "{schema}"
    );
}

#[test]
fn it_plans_a_patch_applies_and_undoes_exactly_and_creates_the_table_it_needs() {
    let mut doc = doc_with(&[]);
    let original = doc.root().clone();
    assert!(original["characters"]["mira"].get("sources").is_none(), "the fixture starts without a table");
    let source = row(Commercial::Ok, "Ann", grants_profit());

    // A dry run changes nothing; the table and the row are one patch, so undo removes both.
    let planned = set_source(&doc, "hair", &source).unwrap();
    assert_eq!(*doc.root(), original);
    assert_eq!(planned.patch.changes.len(), 2, "the sources table and the row");
    assert_eq!(planned.patch.changes[1].path.to_string(), "characters/mira/sources/hair");

    doc.apply(&planned.patch).unwrap();
    let applied = doc.root().clone();
    let node = applied.pointer("/characters/mira/sources/hair").unwrap();
    assert_eq!(node["schema"], json!(1));
    assert!(!node.to_string().contains("null"), "the project files cannot hold null: {node}");
    assert_eq!(PartSource::from_node(node).unwrap(), source, "it reads back as what was recorded");
    let mut without = applied.clone();
    without["characters"]["mira"].as_object_mut().unwrap().remove("sources");
    assert_eq!(without, original, "only the row changed");

    doc.apply(&planned.patch.inverse()).unwrap();
    assert_eq!(*doc.root(), original, "undo must restore the document exactly");
    doc.apply(&planned.patch).unwrap();
    assert_eq!(*doc.root(), applied, "redo must give the same document as the first apply");

    let summary = planned.output.unwrap();
    assert_eq!(summary["path"], json!("characters/mira/sources/hair"));
    assert_eq!((summary["changed"].clone(), summary["replaced"].clone()), (json!(true), json!(false)));
    assert_eq!(summary["source"], *node);
}

#[test]
fn recording_the_same_row_again_changes_nothing_and_a_new_row_replaces_and_undoes_to_the_old() {
    let mut doc = doc_with(&[]);
    let source = row(Commercial::Ok, "Ann", grants_profit());
    record(&mut doc, "hair", &source);
    let again = set_source(&doc, "hair", &source).unwrap();
    assert!(again.patch.is_empty() && again.patch.changes.is_empty(), "no empty table either");
    let summary = again.output.unwrap();
    assert_eq!((summary["changed"].clone(), summary["replaced"].clone()), (json!(false), json!(true)));

    let before = doc.root().clone();
    let stricter = row(Commercial::No, "Ann", UseTerms::default());
    let replacing = record(&mut doc, "hair", &stricter);
    assert_eq!(replacing.output.as_ref().unwrap()["replaced"], json!(true));
    assert_eq!(derive(&doc).parts[1].commercial, Commercial::No);
    doc.apply(&replacing.patch.inverse()).unwrap();
    assert_eq!(*doc.root(), before, "undo brings the old row back");
}

#[test]
fn a_row_that_does_not_read_can_be_repaired_by_recording_it_again() {
    let mut rows = all_rows();
    rows[1].1.schema = 2;
    let mut doc = doc_with(&rows);
    let broken = plan(&doc, "licence.derive", json!({ "character": "mira" })).unwrap_err();
    assert_eq!(broken.field.as_deref(), Some("sources/hair"));
    record(&mut doc, "hair", &row(Commercial::Ok, "Ann", grants_profit()));
    assert!(derive(&doc).complete, "recording the row replaced the one that did not read");
}

// ---------------------------------------------------------------------------------------------------------------
// Refusals name the exact field
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn a_row_the_file_format_would_refuse_is_refused_naming_the_field() {
    let doc = doc_with(&[]);
    let mut blank_licence = row(Commercial::Ok, "Ann", grants_profit());
    blank_licence.provenance.licence = "  ".into();
    let error = refusal(&doc, "hair", &blank_licence, ErrorCode::Invalid, "source/provenance/licence");
    assert!(error.message.contains("LicenseRef-Unknown"), "{}", error.message);

    let mut blank_owner = row(Commercial::Ok, "Ann", grants_profit());
    blank_owner.owned = Some(Ownership { asserted_by: " ".into() });
    let error = refusal(&doc, "hair", &blank_owner, ErrorCode::Invalid, "source/owned/asserted_by");
    assert!(error.message.contains("owned.asserted_by"), "{}", error.message);

    let mut wrong_schema = row(Commercial::Ok, "Ann", grants_profit());
    wrong_schema.schema = 2;
    let error = refusal(&doc, "hair", &wrong_schema, ErrorCode::Invalid, "source/schema");
    assert!(error.message.contains("schema 1"), "{}", error.message);

    // Nothing was written by any of the refusals.
    assert!(doc.root()["characters"]["mira"].get("sources").is_none());
}

#[test]
fn a_row_with_a_key_the_format_does_not_have_is_refused_at_the_arguments() {
    let mut arguments = args("hair", &row(Commercial::Ok, "Ann", grants_profit()));
    arguments["source"]["surprise"] = json!(true);
    let error = (find("licence.set_source").unwrap().decode)(arguments).err().expect("refused");
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("source/surprise")));
    assert!(error.hint.is_some());
}

#[test]
fn an_unknown_part_or_character_is_not_found_naming_the_argument() {
    let doc = doc_with(&[]);
    let source = row(Commercial::Ok, "Ann", grants_profit());
    let error = refusal(&doc, "wig", &source, ErrorCode::NotFound, "part");
    assert!(error.message.contains("no part `wig`"), "{}", error.message);
    assert!(error.hint.as_deref().unwrap().contains("body, hair, head, shirt, vest"), "{error:?}");
    // A row is about a part the character has: a refused row leaves no orphan behind.
    assert!(doc.root()["characters"]["mira"].get("sources").is_none());

    let arguments = json!({ "character": "ghost", "part": "hair", "source": serde_json::to_value(&source).unwrap() });
    let error = plan(&doc, "licence.set_source", arguments).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::NotFound, Some("character")));
    assert!(error.hint.is_some());
}

// ---------------------------------------------------------------------------------------------------------------
// Nothing widens by itself
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn derive_widens_only_as_rows_are_written_and_is_complete_after_the_last() {
    let mut doc = doc_with(&[]);
    let floor = derive(&doc);
    assert!(!floor.complete);
    assert_eq!(floor.record.provenance.commercial, Commercial::Unknown);
    assert_eq!(floor.record.meta.terms, UseTerms::default());
    assert!(!describe(&doc, "publish").ok);

    let rows = all_rows();
    for (n, (part, source)) in rows.iter().enumerate() {
        record(&mut doc, part, source);
        let derived = derive(&doc);
        let unrecorded: Vec<&str> = derived.parts.iter().filter(|p| !p.recorded).map(|p| p.part.as_str()).collect();
        assert_eq!(unrecorded, PARTS[n + 1..], "after the row for {part}");
        assert_eq!(derived.complete, n + 1 == PARTS.len());
        if !derived.complete {
            // One part nobody has recorded keeps the whole character at the floor, however much the others grant.
            assert_eq!(derived.record.provenance.commercial, Commercial::Unknown);
            assert_eq!(derived.record.meta.terms, UseTerms::default());
        }
    }
    let derived = derive(&doc);
    assert_eq!(derived.record.provenance.commercial, Commercial::Ok);
    assert_eq!(derived.record.meta.terms.commercial_usage, CommercialUsage::PersonalProfit);
    assert_eq!(derived, derive(&doc_with(&rows)), "the ops write what a hand-written project holds");
}

#[test]
fn a_row_that_says_nothing_about_terms_grants_nothing() {
    let mut doc = doc_with(&[]);
    for part in PARTS {
        let mut source = row(Commercial::Ok, "Ann", UseTerms::default());
        source.terms = None;
        record(&mut doc, part, &source);
    }
    let derived = derive(&doc);
    assert!(derived.complete);
    assert_eq!(derived.record.provenance.commercial, Commercial::Ok);
    assert_eq!(derived.record.meta.terms, UseTerms::default(), "a licence that says nothing about profit grants none");
}

#[test]
fn terms_on_a_row_that_is_not_cleared_are_ignored_and_the_note_says_so() {
    let mut doc = doc_with(&[]);
    for part in PARTS {
        record(&mut doc, part, &row(Commercial::Ok, "Ann", grants_profit()));
    }
    let planned = record(&mut doc, "hair", &row(Commercial::Unknown, "Ann", grants_profit()));
    let said = notes(&planned);
    assert!(said.iter().any(|n| n.contains("`unknown`") && n.contains("ignored")), "{said:?}");
    let derived = derive(&doc);
    assert_eq!(derived.record.provenance.commercial, Commercial::Unknown);
    assert_eq!(derived.record.meta.terms, UseTerms::default());
    assert!(!describe(&doc, "publish").ok, "unknown stays blocked until someone has read the licence");

    // A row that grants nothing has nothing to ignore.
    let quiet = set_source(&doc, "hair", &row(Commercial::Unknown, "Ann", UseTerms::default())).unwrap();
    assert!(notes(&quiet).is_empty(), "{:?}", notes(&quiet));
}

#[test]
fn private_test_material_recorded_by_hand_is_never_waived_by_ownership() {
    let mut doc = doc_with(&[]);
    let mut fan_art = row(Commercial::Ok, "Ann", grants_profit());
    fan_art.provenance.private_test = true;
    fan_art.owned = Some(Ownership { asserted_by: "Ann".into() });
    let planned = record(&mut doc, "head", &fan_art);
    let said = notes(&planned);
    assert!(said.iter().any(|n| n.contains("private test") && n.contains("ignored")), "{said:?}");
    assert!(said.iter().any(|n| n.contains("`owned` does not waive")), "{said:?}");
    for part in PARTS.iter().filter(|p| **p != "head") {
        let mut source = row(Commercial::Ok, "Ann", grants_profit());
        source.owned = Some(Ownership { asserted_by: "Ann".into() });
        record(&mut doc, part, &source);
    }
    let derived = derive(&doc);
    assert!(derived.complete && derived.private_test, "the flag propagates");
    assert!(!derived.project_owned, "every part is owned, and it still is not waived");
    assert_eq!(derived.record.meta.terms, UseTerms::default());
    assert!(!describe(&doc, "publish").ok && describe(&doc, "publish").report.has_rule("rights.gate_private_test"));
}

#[test]
fn a_stored_licence_record_is_flagged_when_a_row_may_change_what_it_may_say() {
    let mut doc = doc_with(&all_rows());
    let quiet = set_source(&doc, "hair", &row(Commercial::No, "Ann", UseTerms::default())).unwrap();
    assert!(notes(&quiet).is_empty(), "no record, no note: {:?}", notes(&quiet));

    let record_node = serde_json::to_value(derive(&doc).record).unwrap();
    let mut root = doc.root().clone();
    root["characters"]["mira"]["licence"] = record_node;
    doc = MemDoc::from_value(root);
    let planned = record(&mut doc, "hair", &row(Commercial::No, "Ann", UseTerms::default()));
    assert!(notes(&planned).iter().any(|n| n.contains("licence.describe")), "{:?}", notes(&planned));
    // And describe does find the record stale: the row now allows less than the record says.
    assert!(!describe(&doc, "publish").ok);
}

// ---------------------------------------------------------------------------------------------------------------
// Rows in the shape the part ops leave
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn a_copied_row_keeps_the_character_complete_and_a_removed_part_takes_its_row() {
    // `part.duplicate` copies the row verbatim under the new id; `part.remove` deletes it (omuzaki-char's tests check
    // the ops). What `licence.derive` makes of a document in that shape is checked here.
    let mut root = doc_with(&all_rows()).root().clone();
    let (parts, sources) = (root["characters"]["mira"]["parts"].clone(), root["characters"]["mira"]["sources"].clone());
    let (mut hair_b, hair_row) = (parts["hair"].clone(), sources["hair"].clone());
    hair_b["enabled"] = json!(false);
    root["characters"]["mira"]["parts"]["hair-b"] = hair_b;
    root["characters"]["mira"]["sources"]["hair-b"] = hair_row;
    let duplicated = MemDoc::from_value(root.clone());
    let derived = derive(&duplicated);
    assert!(derived.complete, "the copy has its original's row: {:#?}", derived.report);
    assert_eq!(derived.parts.len(), 6);
    assert_eq!(
        derived.record,
        derive(&doc_with(&all_rows())).record,
        "a copy of an asset adds no rights and no limits"
    );

    // Without the copied row the character would have been incomplete and back at the floor.
    root["characters"]["mira"]["sources"].as_object_mut().unwrap().remove("hair-b");
    let without_row = derive(&MemDoc::from_value(root.clone()));
    assert!(!without_row.complete && without_row.record.provenance.commercial == Commercial::Unknown);

    // A removed part with its row gone leaves nothing to ignore; a row left behind is only a note.
    root["characters"]["mira"]["parts"].as_object_mut().unwrap().remove("hair-b");
    let removed = derive(&MemDoc::from_value(root.clone()));
    assert!(removed.complete && !removed.report.has_rule("licence.source_orphaned"));
    root["characters"]["mira"]["sources"]["hair-b"] =
        doc_with(&all_rows()).root()["characters"]["mira"]["sources"]["hair"].clone();
    assert!(derive(&MemDoc::from_value(root)).report.has_rule("licence.source_orphaned"));
}

#[test]
fn a_recorded_character_publishes_once_its_derived_record_is_stored() {
    let mut doc = doc_with(&[]);
    for (part, source) in all_rows() {
        record(&mut doc, part, &source);
    }
    let derived = derive(&doc).record;
    let stored =
        plan(&doc, "licence.set", json!({ "character": "mira", "record": serde_json::to_value(&derived).unwrap() }))
            .unwrap_or_else(|e| panic!("{e:?}"));
    doc.apply(&stored.patch).unwrap();
    let check: ExportCheck = answer(
        plan(&doc, "io.check_export", json!({ "character": "mira", "format": "glb", "purpose": "publish" })).unwrap(),
    );
    assert!(check.ok, "{:#?}", check.report);
    // Referenced so the linker keeps the mock formats `io.check_export` looks up by id.
    let _ = (MockGlbExporter, MockVrm1Exporter);
}

/// What a person sees: `licence.derive` before any row is written and after the last one. Run with `--nocapture` to
/// print it (the PR quotes it).
#[test]
fn licence_derive_before_and_after_the_rows_are_written_printed() {
    let summary = |doc: &MemDoc| {
        let derived = derive(doc);
        json!({
            "complete": derived.complete,
            "commercial": derived.record.provenance.commercial,
            "licence": derived.record.provenance.licence,
            "authors": derived.record.meta.authors,
            "meta.commercialUsage": derived.record.meta.terms.commercial_usage,
            "parts_without_a_row": derived.parts.iter().filter(|p| !p.recorded).map(|p| p.part.as_str()).collect::<Vec<_>>(),
        })
    };
    let mut doc = doc_with(&[]);
    let before = summary(&doc);
    println!("--- licence.derive, no rows ---\n{}", serde_json::to_string_pretty(&before).unwrap());
    for (part, source) in all_rows() {
        let planned = record(&mut doc, part, &source);
        if part == "body" {
            println!("--- licence.set_source body ---\n{}", serde_json::to_string_pretty(&planned.output).unwrap());
        }
    }
    let after = summary(&doc);
    println!(
        "--- licence.derive, a row for each of the five parts ---\n{}",
        serde_json::to_string_pretty(&after).unwrap()
    );
    // The rows complete the derivation; storing its record with `licence.set` is what lets the character publish.
    assert!(!describe(&doc, "publish").ok, "no record yet");
    let record_args = json!({ "character": "mira", "record": serde_json::to_value(derive(&doc).record).unwrap() });
    let stored = plan(&doc, "licence.set", record_args).unwrap_or_else(|e| panic!("{e:?}"));
    doc.apply(&stored.patch).unwrap();
    println!(
        "--- licence.describe publish, after licence.set stores the derived record: ok = {} ---",
        describe(&doc, "publish").ok
    );
    assert!(describe(&doc, "publish").ok);
    assert_eq!(before["complete"], json!(false));
    assert_eq!(before["commercial"], json!("unknown"));
    assert_eq!(before["meta.commercialUsage"], json!("personalNonProfit"));
    assert_eq!(before["parts_without_a_row"], json!(PARTS));
    assert_eq!(after["complete"], json!(true));
    assert_eq!(after["commercial"], json!("ok"));
    assert_eq!(after["meta.commercialUsage"], json!("personalProfit"));
    assert_eq!(after["parts_without_a_row"], json!([]));
}
