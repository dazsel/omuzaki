//! `licence.derive`, `licence.describe` and `licence.set`, against the mock character.
//!
//! The ops are decoded from JSON and planned exactly as an executor would (`omuzaki-ops` is an implementation crate, so it
//! is not a dependency here); a plan's patch is applied to a `MemDoc` the way the executor applies it, and undone with
//! `Patch::inverse`. `licence_ops_answer_for_the_mock_character_asserted_and_printed` prints the answers the PR quotes
//! (run it with `--nocapture`).

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_char_api::{BuildCtx, BuildError, KindProvenance, PartKind, PartMeta, PartOutput};
use omuzaki_char_mock::mock_character;
use omuzaki_char_mock::{MockBody, MockGarment, MockHair, MockHead};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    AvatarPermission, CommercialUsage, CreditNotation, Modification, Severity, UseTerms, VRM_LICENSE_URLS,
};
use omuzaki_io_mock::{MockGlbExporter, MockVrm1Exporter};
use omuzaki_io_ops::gather::avatar::{LicenceRecord, OwnedBasis, record_path};
use omuzaki_io_ops::ops::check_export::ExportCheck;
use omuzaki_io_ops::ops::licence::derive::{DerivedLicence, KIND_OWNER};
use omuzaki_io_ops::ops::licence::describe::LicenceDescription;
use omuzaki_io_ops::ops::licence::source::{Ownership, PartSource, source_path};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, OpKind, Plan, Scope, Undo, find};
use omuzaki_store_api::{Commercial, Consent, Provenance, Source};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------------------------

/// Raises one flag of a record's terms.
type Raise = fn(&mut UseTerms);

/// The mock character's parts, in id order: body, hair, head, shirt, vest.
const PARTS: [&str; 5] = ["body", "hair", "head", "shirt", "vest"];

fn terms(change: impl FnOnce(&mut UseTerms)) -> UseTerms {
    let mut terms = UseTerms::default();
    change(&mut terms);
    terms
}

/// Terms that grant everything the file can say.
fn everything() -> UseTerms {
    terms(|t| {
        t.avatar_permission = AvatarPermission::Everyone;
        t.allow_excessively_violent_usage = true;
        t.allow_excessively_sexual_usage = true;
        t.commercial_usage = CommercialUsage::Corporation;
        t.allow_political_or_religious_usage = true;
        t.allow_antisocial_or_hate_usage = true;
        t.credit_notation = CreditNotation::Unnecessary;
        t.allow_redistribution = true;
        t.modification = Modification::AllowModificationRedistribution;
    })
}

fn source(licence: &str, commercial: Commercial, author: &str, terms: UseTerms) -> PartSource {
    let provenance = Provenance {
        source: Source::Library { item: format!("{licence}@1") },
        author: Some(author.into()),
        licence: licence.into(),
        attribution: None,
        commercial,
        consent: None,
        private_test: false,
    };
    PartSource { terms: Some(terms), ..PartSource::new(provenance) }
}

fn owned(mut source: PartSource, by: &str) -> PartSource {
    source.owned = Some(Ownership { asserted_by: by.into() });
    source
}

/// Sources for the five parts: four CC0 parts by Ann and Cy that grant everything except that the hair may not be
/// redistributed, and a CC BY head by Bo that wants credit and grants only some.
fn mock_sources() -> Vec<(&'static str, PartSource)> {
    let cc0 = |author: &str, terms| source("CC0-1.0", Commercial::Ok, author, terms);
    let mut head = source(
        "CC-BY-4.0",
        Commercial::Ok,
        "Bo",
        terms(|t| {
            t.avatar_permission = AvatarPermission::Everyone;
            t.commercial_usage = CommercialUsage::PersonalProfit;
            t.allow_redistribution = true;
            t.modification = Modification::AllowModification;
        }),
    );
    head.provenance.attribution = Some("Head by Bo, CC BY 4.0".into());
    vec![
        ("body", cc0("Ann", everything())),
        ("hair", cc0("Ann", terms(|t| *t = UseTerms { allow_redistribution: false, ..everything() }))),
        ("head", head),
        ("shirt", cc0("Ann", everything())),
        ("vest", cc0("Cy", everything())),
    ]
}

fn doc_with(sources: &[(&str, PartSource)], record: Option<&LicenceRecord>) -> MemDoc {
    let mut root = json!({ "characters": { "mira": serde_json::to_value(mock_character().unwrap()).unwrap() } });
    for (part, source) in sources {
        root["characters"]["mira"]["sources"][part] = serde_json::to_value(source).unwrap();
    }
    if let Some(record) = record {
        root["characters"]["mira"]["licence"] = serde_json::to_value(record).unwrap();
    }
    MemDoc::from_value(root)
}

/// The mock character with a source for every part.
fn mock_doc() -> MemDoc {
    doc_with(&mock_sources(), None)
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

fn set(doc: &MemDoc, record: &LicenceRecord) -> Result<Plan, OpError> {
    plan(doc, "licence.set", json!({ "character": "mira", "record": serde_json::to_value(record).unwrap() }))
}

/// Plans `licence.set` and applies the patch, as the executor does.
fn store(doc: &mut MemDoc, record: &LicenceRecord) -> Plan {
    let planned = set(doc, record).unwrap_or_else(|e| panic!("{e:?}"));
    doc.apply(&planned.patch).unwrap();
    planned
}

fn stored(doc: &MemDoc) -> LicenceRecord {
    LicenceRecord::from_node(doc.root().pointer("/characters/mira/licence").expect("a stored record")).unwrap()
}

fn check_export(doc: &MemDoc, format: &str, purpose: &str) -> ExportCheck {
    let args = json!({ "character": "mira", "format": format, "purpose": purpose });
    answer(plan(doc, "io.check_export", args).unwrap())
}

fn rules(report: &omuzaki_io_api::Report, severity: Severity) -> Vec<&str> {
    report.findings.iter().filter(|f| f.severity == severity).map(|f| f.rule.as_str()).collect()
}

fn refused(doc: &MemDoc, record: &LicenceRecord) -> OpError {
    let error = set(doc, record).expect_err("the record should be refused");
    assert_eq!(error.code, ErrorCode::Invalid, "{error:?}");
    assert!(error.hint.is_some(), "every refusal says how to fix it: {error:?}");
    error
}

/// A record that grants as much as it can, on top of what the parts allow.
fn greedy(derived: &LicenceRecord) -> LicenceRecord {
    let mut record = derived.clone();
    record.meta.terms = everything();
    record
}

/// The mock kinds again as the project's own generators, under other ids: a kind declares whose rights it carries.
macro_rules! owned_kind {
    ($name:ident, $base:ty, $id:literal, $requires:expr) => {
        struct $name;
        impl PartKind for $name {
            type Params = <$base as PartKind>::Params;
            fn meta() -> PartMeta {
                PartMeta { id: $id, requires: $requires, ..<$base>::meta() }
            }
            fn provenance() -> KindProvenance {
                KindProvenance::ProjectOwned
            }
            fn build(params: &Self::Params, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
                <$base>::build(params, ctx)
            }
        }
        omuzaki_char_api::register_part_kind!($name);
    };
}
owned_kind!(OwnedBody, MockBody, "owned-body", &[]);
owned_kind!(OwnedHead, MockHead, "owned-head", &[]);
owned_kind!(OwnedHair, MockHair, "owned-hair", &["owned-head"]);
owned_kind!(OwnedGarment, MockGarment, "owned-garment", &["owned-body"]);

/// The mock character with every part made by a project-owned kind and no source rows at all.
fn procedural_doc() -> MemDoc {
    let mut root = doc_with(&[], None).root().clone();
    for (part, kind) in [
        ("body", "owned-body"),
        ("head", "owned-head"),
        ("hair", "owned-hair"),
        ("shirt", "owned-garment"),
        ("vest", "owned-garment"),
    ] {
        root["characters"]["mira"]["parts"][part]["kind"] = json!(kind);
    }
    MemDoc::from_value(root)
}

// ---------------------------------------------------------------------------------------------------------------
// Kind-declared provenance, the source of a derived record, and private test parts at export
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn a_fully_procedural_character_derives_sets_and_publishes() {
    let mut doc = procedural_doc();
    let derived = derive(&doc);
    assert!(derived.complete && derived.project_owned, "{:#?}", derived.report);
    assert_eq!(derived.owners, [KIND_OWNER]);
    assert!(derived.parts.iter().all(|p| p.kind_owned && p.recorded));
    let record = &derived.record;
    assert_eq!(record.provenance.commercial, Commercial::Ok);
    assert_eq!(record.meta.terms, UseTerms::default(), "a kind says whose rights, never what is granted");
    // The authors are still the person's to add.
    let mut record = record.clone();
    record.meta.authors = vec!["Mira".into()];
    store(&mut doc, &record);
    for format in ["glb", "vrm1"] {
        let check = check_export(&doc, format, "publish");
        assert!(check.ok, "{format}: {:#?}", check.report);
    }
    assert!(describe(&doc, "publish").ok);
}

#[test]
fn an_external_part_without_a_row_still_blocks_and_a_row_beats_the_kind() {
    let mut root = procedural_doc().root().clone();
    root["characters"]["mira"]["parts"]["hair"]["kind"] = json!("mock-hair");
    let external = MemDoc::from_value(root);
    let derived = derive(&external);
    assert!(!derived.complete && derived.record.provenance.commercial == Commercial::Unknown);
    assert_eq!(derived.parts.iter().filter(|p| !p.recorded).count(), 1);
    assert!(!describe(&external, "publish").ok);

    // A row about a project-kind part is the record of that part: it wins.
    let mut with_row = procedural_doc();
    let row = source("CC-BY-NC-4.0", Commercial::No, "Dee", UseTerms::default());
    let patch = omuzaki_doc_api::Patch::new().with(
        source_path(&"mira".parse().unwrap(), &"hair".parse().unwrap()),
        None,
        Some(serde_json::to_value(&row).unwrap()),
    );
    let mut root = with_row.root().clone();
    root["characters"]["mira"]["sources"] = json!({});
    with_row = MemDoc::from_value(root);
    with_row.apply(&patch).unwrap();
    let derived = derive(&with_row);
    assert_eq!(derived.record.provenance.commercial, Commercial::No);
    let hair = derived.parts.iter().find(|p| p.part == "hair").unwrap();
    assert!(!hair.kind_owned && hair.licence == "CC-BY-NC-4.0");
}

#[test]
fn an_imported_part_that_forbids_redistribution_makes_publish_fail() {
    let mut sources = mock_sources();
    sources[1].1.provenance.source = Source::Imported { file: "hair.vrm".into(), origin: None };
    let mut doc = doc_with(&sources, None);
    let derived = derive(&doc);
    assert!(matches!(derived.record.provenance.source, Source::Imported { .. }), "any imported part wins");
    store(&mut doc, &derived.record);
    let check = check_export(&doc, "vrm1", "publish");
    assert!(!check.ok && check.report.has_rule("rights.licence_forbids_redistribution"), "{:#?}", check.report);
    assert!(check_export(&doc, "vrm1", "private").ok);

    // A record that calls the character authored is looser than its parts and is refused.
    let mut authored = derived.record.clone();
    authored.provenance.source = Source::Authored;
    let error = refused(&doc, &authored);
    assert_eq!(error.field.as_deref(), Some("record/provenance/source"));
}

#[test]
fn a_record_that_lacks_the_private_flag_cannot_publish_when_a_part_is_private_test() {
    let mut sources = mock_sources();
    sources[2].1.provenance.private_test = true;
    let mut forgetful = derive(&doc_with(&sources, None)).record;
    forgetful.provenance.private_test = false;
    forgetful.meta.terms = UseTerms::default();
    let doc = doc_with(&sources, Some(&forgetful));
    for format in ["glb", "vrm1"] {
        let check = check_export(&doc, format, "publish");
        assert!(!check.ok && check.report.has_rule("rights.gate_private_test"), "{format}: {:#?}", check.report);
        assert!(check.report.errors().any(|f| f.rule == "rights.gate_private_test"));
    }
    // A record that forgot the flag is wrong even for a file on your machine, as `describe` says: the stored
    // record is more permissive than the part it covers.
    let private = check_export(&doc, "glb", "private");
    assert!(!private.ok, "{:#?}", private.report);
    let flag = private.report.errors().find(|f| f.rule == "licence.exceeds_parts").expect("the flag is missing");
    assert_eq!(flag.location.as_deref(), Some("characters/mira/licence/provenance/private_test"));
    // With the flag stored, private test material may be exported for the user's own machine.
    let mut with_flag = forgetful.clone();
    with_flag.provenance.private_test = true;
    assert!(check_export(&doc_with(&sources, Some(&with_flag)), "glb", "private").ok);
}

// ---------------------------------------------------------------------------------------------------------------
// The ops
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn the_three_ops_register_with_the_scopes_the_decision_gives_them() {
    let meta = |name| (find(name).unwrap_or_else(|| panic!("{name} is not registered")).meta)();
    let set = meta("licence.set");
    assert_eq!((set.kind, set.scope, set.undo), (OpKind::Edit, Scope::EditCharacter, Undo::Auto));
    for query in ["licence.describe", "licence.derive"] {
        let meta = meta(query);
        assert_eq!((meta.kind, meta.scope, meta.undo), (OpKind::Query, Scope::Read, Undo::None), "{query}");
    }
}

#[test]
fn the_argument_schemas_are_flat_and_closed() {
    let schema = |name: &str| serde_json::to_value((find(name).unwrap().schema)()).unwrap();
    for (name, required, properties) in [
        ("licence.set", json!(["character", "record"]), vec!["character", "record"]),
        ("licence.describe", json!(["character"]), vec!["character", "purpose"]),
        ("licence.derive", json!(["character"]), vec!["character"]),
    ] {
        let schema = schema(name);
        assert_eq!(schema["required"], required, "{name}");
        let keys: Vec<&str> = schema["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, properties, "{name}");
        assert_eq!(schema["additionalProperties"], json!(false), "{name}");
        assert!(!schema.to_string().contains("$ref"), "{name}: MCP tool schemas are inlined");
    }
    // The record is documented down to the permission flags an agent has to set.
    let record = schema("licence.set")["properties"]["record"].clone();
    assert!(record["properties"]["meta"]["properties"]["commercialUsage"].is_object(), "{record}");
}

#[test]
fn arguments_that_do_not_fit_are_invalid_and_name_the_field() {
    let derived = derive(&mock_doc()).record;
    let mut bad_status = serde_json::to_value(&derived).unwrap();
    bad_status["provenance"]["commercial"] = json!("maybe");
    let mut bad_flag = serde_json::to_value(&derived).unwrap();
    bad_flag["meta"]["commercialUsage"] = json!("everyone");
    for (op, args, field) in [
        ("licence.set", json!({ "character": "mira" }), "record"),
        ("licence.set", json!({ "character": "mira", "record": bad_status }), "record/provenance/commercial"),
        // The flags are flattened into `meta`, where the decoder can name the table but not the key.
        ("licence.set", json!({ "character": "mira", "record": bad_flag }), "record/meta"),
        ("licence.describe", json!({ "character": "mira", "purpose": "everyone" }), "purpose"),
        ("licence.derive", json!({ "character": "mira", "format": "vrm1" }), "format"),
    ] {
        let error = (find(op).unwrap().decode)(args.clone()).err().expect("refused");
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some(field)), "{op} {args}");
        assert!(error.hint.is_some());
        if field == "record/meta" {
            assert!(error.message.contains("personalNonProfit"), "the message lists the values: {}", error.message);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// licence.derive
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn derive_on_the_mock_character_is_the_strictest_join_of_its_parts() {
    let derived = derive(&mock_doc());
    assert!(derived.complete && !derived.private_test && !derived.project_owned);
    assert_eq!(derived.character, "mira");
    let (p, m) = (&derived.record.provenance, &derived.record.meta);
    assert_eq!(derived.record.schema, 1);
    // Permissions: the strictest of the five.
    assert_eq!(p.commercial, Commercial::Ok);
    assert_eq!(m.terms.avatar_permission, AvatarPermission::Everyone);
    assert_eq!(m.terms.commercial_usage, CommercialUsage::PersonalProfit, "the head grants profit, not corporate use");
    assert!(!m.terms.allow_redistribution, "the hair holds redistribution back");
    assert_eq!(m.terms.modification, Modification::AllowModification, "the head allows editing, not passing it on");
    assert_eq!(m.terms.credit_notation, CreditNotation::Required, "the head asks for credit");
    assert!(
        !m.terms.allow_excessively_violent_usage && !m.terms.allow_excessively_sexual_usage,
        "the head grants no content flag"
    );
    // Credit and licences: the union, nothing dropped, nothing made up.
    assert_eq!(p.licence, "CC-BY-4.0 AND CC0-1.0");
    assert_eq!(p.author.as_deref(), Some("Ann, Bo, Cy"));
    assert_eq!(m.authors, ["Ann", "Bo", "Cy"]);
    assert_eq!(p.attribution.as_deref(), Some("Head by Bo, CC BY 4.0"));
    assert_eq!(derived.attributions, ["Head by Bo, CC BY 4.0"]);
    assert_eq!((m.name.as_str(), m.license_url.as_str()), ("Mock", VRM_LICENSE_URLS[0]));
    assert!(matches!(p.source, Source::Library { .. }), "the strictest source of the parts, not a guess");
    assert!(!p.private_test && p.consent.is_none());
    // Each part's contribution is listed in id order, and the report has nothing to warn about.
    let listed: Vec<&str> = derived.parts.iter().map(|r| r.part.as_str()).collect();
    assert_eq!(listed, PARTS);
    assert!(derived.parts.iter().all(|r| r.recorded && r.enabled));
    assert!(derived.report.findings.is_empty(), "{:#?}", derived.report);
}

#[test]
fn derive_is_never_more_permissive_than_any_part_on_any_permission() {
    // Every combination of three differing parts out of a small set of terms and statuses, over the mock's five parts.
    let flavours: Vec<(Commercial, UseTerms)> = vec![
        (Commercial::Ok, everything()),
        (Commercial::Ok, UseTerms::default()),
        (Commercial::Ok, terms(|t| t.commercial_usage = CommercialUsage::PersonalProfit)),
        (Commercial::Unknown, everything()),
        (Commercial::No, terms(|t| t.avatar_permission = AvatarPermission::OnlySeparatelyLicensedPerson)),
        (Commercial::Ok, terms(|t| t.modification = Modification::AllowModification)),
    ];
    let status_rank = |c: Commercial| match c {
        Commercial::No => 0,
        Commercial::Unknown => 1,
        Commercial::Ok => 2,
    };
    for a in &flavours {
        for b in &flavours {
            for c in &flavours {
                let picks = [a, b, c, a, b];
                let sources: Vec<(&str, PartSource)> = PARTS
                    .iter()
                    .zip(picks)
                    .map(|(part, (commercial, terms))| (*part, source("CC0-1.0", *commercial, "Ann", terms.clone())))
                    .collect();
                let derived = derive(&doc_with(&sources, None));
                let joined = &derived.record;
                for (_, (commercial, part_terms)) in PARTS.iter().zip(picks) {
                    assert!(status_rank(joined.provenance.commercial) <= status_rank(*commercial));
                    assert!(joined.meta.terms.is_within(part_terms), "{:?} exceeds {part_terms:?}", joined.meta.terms);
                }
                // Not cleared means nothing is granted at all, the exporters' rule.
                if joined.provenance.commercial != Commercial::Ok {
                    assert_eq!(joined.meta.terms, UseTerms::default());
                }
            }
        }
    }
}

#[test]
fn a_part_without_a_source_counts_as_the_most_restrictive_part_possible() {
    let mut sources = mock_sources();
    sources.retain(|(part, _)| *part != "hair");
    let derived = derive(&doc_with(&sources, None));
    assert!(!derived.complete);
    let p = &derived.record.provenance;
    assert_eq!(p.commercial, Commercial::Unknown, "unestablished, which the gate treats as no");
    assert_eq!(derived.record.meta.terms, UseTerms::default(), "a status that is not ok grants nothing");
    assert_eq!(p.licence, "CC-BY-4.0 AND CC0-1.0 AND LicenseRef-Unknown");
    let hair = derived.parts.iter().find(|r| r.part == "hair").unwrap();
    assert!(!hair.recorded && hair.commercial == Commercial::Unknown && hair.licence == "LicenseRef-Unknown");
    // The report names the part, where its row goes and how to write it.
    let missing: Vec<_> = derived.report.findings.iter().filter(|f| f.rule == "licence.part_source_missing").collect();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].severity, Severity::Warning);
    assert_eq!(missing[0].location.as_deref(), Some("characters/mira/sources/hair"));
    assert!(missing[0].hint.as_deref().unwrap().contains("provenance"));
}

#[test]
fn a_character_with_no_sources_at_all_derives_the_floor_and_a_character_with_no_parts_too() {
    let none = derive(&doc_with(&[], None));
    assert!(!none.complete);
    assert_eq!(none.report.findings.iter().filter(|f| f.rule == "licence.part_source_missing").count(), 5);
    assert_eq!(none.record.provenance.commercial, Commercial::Unknown);
    assert_eq!(none.record.provenance.licence, "LicenseRef-Unknown");
    assert_eq!(none.record.provenance.author, None);
    assert_eq!(none.record.meta.terms, UseTerms::default());

    let mut root = doc_with(&[], None).root().clone();
    root["characters"]["mira"]["parts"] = json!({});
    let empty = derive(&MemDoc::from_value(root));
    assert!(!empty.complete && !empty.project_owned, "nothing to join is not a clearance");
    assert_eq!(empty.record.provenance.commercial, Commercial::Unknown);
    assert_eq!(empty.record.meta.terms, UseTerms::default());
    assert!(empty.report.has_rule("licence.no_parts"));
}

#[test]
fn disabled_parts_are_joined_too_because_a_variant_can_switch_them_on() {
    let mut sources = mock_sources();
    let sketchy = source("CC-BY-NC-4.0", Commercial::No, "Dee", UseTerms::default());
    sources.retain(|(part, _)| *part != "vest");
    sources.push(("vest", sketchy));
    let mut root = doc_with(&sources, None).root().clone();
    root["characters"]["mira"]["parts"]["vest"]["enabled"] = json!(false);
    let derived = derive(&MemDoc::from_value(root));
    assert_eq!(derived.record.provenance.commercial, Commercial::No);
    let vest = derived.parts.iter().find(|r| r.part == "vest").unwrap();
    assert!(!vest.enabled && vest.recorded);
}

#[test]
fn private_test_material_propagates_and_grants_nothing() {
    let mut sources = mock_sources();
    sources.retain(|(part, _)| *part != "head");
    let mut fan_art = source("LicenseRef-Fan-Art", Commercial::Ok, "Eve", everything());
    fan_art.provenance.private_test = true;
    sources.push(("head", fan_art));
    let derived = derive(&doc_with(&sources, None));
    assert!(derived.private_test && derived.record.provenance.private_test);
    assert_eq!(derived.record.meta.terms, UseTerms::default(), "private test material carries the strictest terms");
    assert!(!derived.project_owned);
}

#[test]
fn consents_are_kept_when_the_parts_agree_and_dropped_with_a_warning_when_they_do_not() {
    let consent = |person: &str| Consent { person: person.into(), scope: "likeness".into(), document: None };
    let mut sources = mock_sources();
    sources[0].1.provenance.consent = Some(consent("Dana"));
    sources[1].1.provenance.consent = Some(consent("Dana"));
    let agreed = derive(&doc_with(&sources, None));
    assert_eq!(agreed.record.provenance.consent, Some(consent("Dana")));
    assert!(!agreed.report.has_rule("licence.consent_ambiguous"));

    sources[2].1.provenance.consent = Some(consent("Eli"));
    let differing = derive(&doc_with(&sources, None));
    assert_eq!(differing.record.provenance.consent, None, "a record holds one consent");
    let warning = differing.report.findings.iter().find(|f| f.rule == "licence.consent_ambiguous").unwrap();
    assert!(warning.message.contains("Dana") && warning.message.contains("Eli"), "{warning:?}");
}

#[test]
fn a_source_row_that_does_not_read_is_an_error_naming_the_part_and_a_stale_row_is_a_note() {
    let mut root = mock_doc().root().clone();
    root["characters"]["mira"]["sources"]["hair"]["schema"] = json!(2);
    let error = plan(&MemDoc::from_value(root), "licence.derive", json!({ "character": "mira" })).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("sources/hair")));
    assert!(error.message.contains("characters/mira/sources/hair"), "{}", error.message);

    let mut root = mock_doc().root().clone();
    root["characters"]["mira"]["sources"]["tail"] = root["characters"]["mira"]["sources"]["hair"].clone();
    let derived = derive(&MemDoc::from_value(root));
    let note = derived.report.findings.iter().find(|f| f.rule == "licence.source_orphaned").unwrap();
    assert_eq!(note.severity, Severity::Info);
    assert!(note.message.contains("`tail`"));
    assert!(derived.complete, "an orphan changes nothing about the join");
}

#[test]
fn an_unknown_character_is_not_found_naming_the_argument() {
    let doc = mock_doc();
    for (op, args) in [
        ("licence.derive", json!({ "character": "nobody" })),
        ("licence.describe", json!({ "character": "nobody" })),
        ("licence.set", json!({ "character": "nobody", "record": serde_json::to_value(derive(&doc).record).unwrap() })),
    ] {
        let error = plan(&doc, op, args).unwrap_err();
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::NotFound, Some("character")), "{op}");
    }
}

// ---------------------------------------------------------------------------------------------------------------
// licence.set
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn set_plans_a_patch_applies_and_undoes_exactly() {
    let mut doc = mock_doc();
    let original = doc.root().clone();
    let derived = derive(&doc).record;

    // Dry-run: planning changes nothing.
    let planned = set(&doc, &derived).unwrap();
    assert_eq!(*doc.root(), original, "planning (a dry-run) must not change the document");
    assert_eq!(planned.patch.changes.len(), 1);
    assert_eq!(planned.patch.changes[0].path, record_path(&"mira".parse().unwrap()));
    assert_eq!(planned.patch.changes[0].before, None);

    // Apply: the record is there, carries its schema, holds no nulls, and reads back as what was set.
    doc.apply(&planned.patch).unwrap();
    let applied = doc.root().clone();
    assert_ne!(applied, original);
    let node = applied.pointer("/characters/mira/licence").unwrap();
    assert_eq!(node["schema"], json!(1));
    assert!(!node.to_string().contains("null"), "the project files cannot hold null: {node}");
    assert_eq!(stored(&doc), derived);
    // Only the record changed: the character, its parts and its sources are as they were.
    let mut without = applied.clone();
    without["characters"]["mira"].as_object_mut().unwrap().remove("licence");
    assert_eq!(without, original);

    // Undo restores the document exactly, and applying again gives the same document.
    doc.apply(&planned.patch.inverse()).unwrap();
    assert_eq!(*doc.root(), original, "undo must restore the document exactly");
    doc.apply(&planned.patch).unwrap();
    assert_eq!(*doc.root(), applied, "redo must give the same document as the first apply");

    // The summary says what was stored and that the parts allowed all of it.
    let summary = planned.output.unwrap();
    assert_eq!((summary["changed"].clone(), summary["path"].clone()), (json!(true), json!("characters/mira/licence")));
    assert_eq!(summary["granted_beyond_the_parts"], json!([]));
}

#[test]
fn setting_the_same_record_again_plans_no_change_and_replacing_one_undoes_to_the_old() {
    let mut doc = mock_doc();
    let derived = derive(&doc).record;
    store(&mut doc, &derived);
    let again = set(&doc, &derived).unwrap();
    assert_eq!(again.patch.changes.len(), 0);
    assert_eq!(again.output.unwrap()["changed"], json!(false));

    let before = doc.root().clone();
    let mut edited = derived.clone();
    edited.meta.contact_information = Some("ann@example.com".into());
    let replacing = store(&mut doc, &edited);
    assert_eq!(stored(&doc).meta.contact_information.as_deref(), Some("ann@example.com"));
    doc.apply(&replacing.patch.inverse()).unwrap();
    assert_eq!(*doc.root(), before);
}

#[test]
fn io_check_export_passes_after_set_with_the_derived_record() {
    let mut doc = mock_doc();
    // Before: a character with no record fails the check, and says where the record goes.
    let before = check_export(&doc, "vrm1", "publish");
    assert!(!before.ok && before.report.has_rule("rights.provenance_missing"));
    assert!(before.report.errors().any(|f| f.hint.as_deref().is_some_and(|h| h.contains("characters/mira/licence"))));

    let derived = derive(&doc).record;
    store(&mut doc, &derived);
    for (format, purpose) in [("vrm1", "private"), ("vrm1", "publish"), ("glb", "private"), ("glb", "publish")] {
        let check = check_export(&doc, format, purpose);
        assert!(check.ok && check.errors == 0, "{format} {purpose}: {:#?}", check.report);
    }
    // The file says what the parts allow and nothing more: the credit travels, the hair's redistribution ban stands.
    let record = stored(&doc);
    assert!(!record.meta.terms.allow_redistribution);
    assert_eq!(record.provenance.attribution.as_deref(), Some("Head by Bo, CC BY 4.0"));
    // Referenced so the linker keeps the mock formats `io.check_export` looks up by id.
    let _ = (MockGlbExporter, MockVrm1Exporter);
}

#[test]
fn set_refuses_each_permission_more_permissive_than_derive_and_names_the_field() {
    let doc = mock_doc();
    let derived = derive(&doc).record;
    // The mock's parts hold every flag below the most the file can say, so raising each one is refused. The
    // `commercial` row raises the status, which the parts cap at `ok`: so first drop a part's status to `no`.
    let mut raised: Vec<(&str, LicenceRecord)> = vec![];
    let changes: [(&str, Raise); 8] = [
        ("meta/commercialUsage", |t| t.commercial_usage = CommercialUsage::Corporation),
        ("meta/allowRedistribution", |t| t.allow_redistribution = true),
        ("meta/modification", |t| t.modification = Modification::AllowModificationRedistribution),
        ("meta/creditNotation", |t| t.credit_notation = CreditNotation::Unnecessary),
        ("meta/allowExcessivelyViolentUsage", |t| t.allow_excessively_violent_usage = true),
        ("meta/allowExcessivelySexualUsage", |t| t.allow_excessively_sexual_usage = true),
        ("meta/allowPoliticalOrReligiousUsage", |t| t.allow_political_or_religious_usage = true),
        ("meta/allowAntisocialOrHateUsage", |t| t.allow_antisocial_or_hate_usage = true),
    ];
    for (field, change) in changes {
        let mut record = derived.clone();
        change(&mut record.meta.terms);
        raised.push((field, record));
    }
    for (field, record) in &raised {
        let error = refused(&doc, record);
        assert_eq!(error.field.as_deref(), Some(format!("record/{field}").as_str()), "{field}");
        assert!(error.message.contains(&format!("`{field}`")), "{}", error.message);
    }
    // The avatar permission is at its top for the mock (every part says everyone), so lower it in a part first.
    let mut sources = mock_sources();
    sources[0].1.terms =
        Some(terms(|t| *t = UseTerms { avatar_permission: AvatarPermission::OnlyAuthor, ..everything() }));
    let narrower = doc_with(&sources, None);
    let mut record = derive(&narrower).record;
    record.meta.terms.avatar_permission = AvatarPermission::Everyone;
    let error = refused(&narrower, &record);
    assert_eq!(error.field.as_deref(), Some("record/meta/avatarPermission"));
    assert!(error.message.contains("`body`"), "the message names the part that sets the limit: {}", error.message);

    // And the commercial status itself.
    let mut sources = mock_sources();
    sources[3].1.provenance.commercial = Commercial::No;
    let uncleared = doc_with(&sources, None);
    let mut record = derive(&uncleared).record;
    assert_eq!(record.provenance.commercial, Commercial::No);
    record.provenance.commercial = Commercial::Ok;
    let error = refused(&uncleared, &record);
    assert_eq!(error.field.as_deref(), Some("record/provenance/commercial"));
    assert!(error.message.contains("`shirt`") && error.message.contains("allow only `no`"), "{}", error.message);
    // Raising it to unknown is still more permissive than no.
    record.provenance.commercial = Commercial::Unknown;
    assert_eq!(refused(&uncleared, &record).field.as_deref(), Some("record/provenance/commercial"));
}

#[test]
fn set_lists_every_field_it_refuses_in_one_message() {
    let doc = mock_doc();
    let mut record = derive(&doc).record;
    record.meta.terms.allow_redistribution = true;
    record.meta.terms.commercial_usage = CommercialUsage::Corporation;
    let error = refused(&doc, &record);
    assert_eq!(error.field.as_deref(), Some("record/meta/commercialUsage"), "the first in the file's order");
    assert!(error.message.contains("`meta/allowRedistribution`") && error.message.contains("`meta/commercialUsage`"));
    assert!(error.message.contains("limited by `hair`"), "{}", error.message);
}

#[test]
fn set_refuses_a_record_that_drops_a_required_credit() {
    let doc = mock_doc();
    let derived = derive(&doc).record;
    let mut dropped = derived.clone();
    dropped.provenance.attribution = None;
    let error = refused(&doc, &dropped);
    assert_eq!(error.field.as_deref(), Some("record/provenance/attribution"));
    assert!(error.message.contains("Head by Bo, CC BY 4.0") && error.message.contains("`head`"), "{}", error.message);
    // Rewording around the credit is fine; only losing it is not.
    let mut longer = derived.clone();
    longer.provenance.attribution = Some("Model by Mira Studio. Head by Bo, CC BY 4.0".into());
    assert!(set(&doc, &longer).is_ok());
}

#[test]
fn set_accepts_a_record_stricter_than_derive_and_adds_what_only_a_person_knows() {
    let doc = mock_doc();
    let derived = derive(&doc).record;
    let mut stricter = derived.clone();
    stricter.meta.terms = UseTerms::default();
    stricter.provenance.commercial = Commercial::No;
    stricter.provenance.private_test = true;
    stricter.meta.contact_information = Some("ann@example.com".into());
    stricter.meta.authors.push("Studio".into());
    assert!(set(&doc, &stricter).is_ok(), "less is always allowed");
    let mut richer = derived;
    richer.meta.contact_information = Some("ann@example.com".into());
    richer.meta.version = Some("1.2".into());
    richer.provenance.consent = Some(Consent { person: "Dana".into(), scope: "likeness".into(), document: None });
    assert!(set(&doc, &richer).is_ok(), "contact, version and consent are not permissions");
}

#[test]
fn set_refuses_a_record_that_contradicts_itself_even_for_a_project_owned_character() {
    let sources: Vec<_> = mock_sources().into_iter().map(|(part, s)| (part, owned(s, "Ann"))).collect();
    let doc = doc_with(&sources, None);
    let mut uncleared = derive(&doc).record;
    uncleared.provenance.commercial = Commercial::No;
    uncleared.meta.terms = terms(|t| t.commercial_usage = CommercialUsage::PersonalProfit);
    let error = refused(&doc, &uncleared);
    assert_eq!(error.field.as_deref(), Some("record/meta/commercialUsage"));
    assert!(error.message.contains("contradicts itself"), "{}", error.message);

    let mut private = derive(&doc).record;
    private.provenance.private_test = true;
    private.meta.terms = terms(|t| t.allow_redistribution = true);
    assert_eq!(refused(&doc, &private).field.as_deref(), Some("record/meta/allowRedistribution"));
}

#[test]
fn set_refuses_a_malformed_record_naming_the_field() {
    let doc = mock_doc();
    let derived = derive(&doc).record;
    let mut wrong_schema = derived.clone();
    wrong_schema.schema = 2;
    assert_eq!(refused(&doc, &wrong_schema).field.as_deref(), Some("record/schema"));
    let mut blank = derived.clone();
    blank.provenance.licence = "  ".into();
    assert_eq!(refused(&doc, &blank).field.as_deref(), Some("record/provenance/licence"));
    for (person, scope, field) in [("", "likeness", "person"), ("Dana", " ", "scope")] {
        let mut record = derived.clone();
        record.provenance.consent = Some(Consent { person: person.into(), scope: scope.into(), document: None });
        assert_eq!(
            refused(&doc, &record).field.as_deref(),
            Some(format!("record/provenance/consent/{field}").as_str())
        );
    }
}

#[test]
fn a_record_may_grant_more_when_every_part_is_the_projects_own_and_says_who_asserted_it() {
    let sources: Vec<_> = mock_sources()
        .into_iter()
        .enumerate()
        .map(|(i, (part, s))| (part, owned(s, if i % 2 == 0 { "Mira" } else { "Ann" })))
        .collect();
    let mut doc = doc_with(&sources, None);
    let derived = derive(&doc);
    assert!(derived.project_owned && !derived.private_test);
    assert_eq!(derived.owners, ["Ann", "Mira"], "sorted, once each");
    assert_eq!(set(&doc, &derived.record).unwrap().output.unwrap()["granted_beyond_the_parts"], json!([]));

    let mut generous = greedy(&derived.record);
    generous.provenance.commercial = Commercial::Ok;
    // A basis the caller made up is replaced by the one the parts back.
    generous.owned = Some(OwnedBasis { asserted_by: vec!["Somebody Else".into()] });
    let planned = store(&mut doc, &generous);
    let record = stored(&doc);
    assert_eq!(record.meta.terms, everything());
    assert_eq!(record.owned, Some(OwnedBasis { asserted_by: vec!["Ann".into(), "Mira".into()] }));
    let beyond = planned.output.unwrap()["granted_beyond_the_parts"].clone();
    assert!(beyond.as_array().unwrap().iter().any(|f| f == "meta/allowRedistribution"), "{beyond}");

    // A record that needs no exception carries no basis, so the field means "this record used it".
    let mut modest = derived.record;
    modest.owned = Some(OwnedBasis { asserted_by: vec!["Somebody Else".into()] });
    store(&mut doc, &modest);
    assert_eq!(stored(&doc).owned, None);
}

#[test]
fn one_part_that_is_not_the_projects_own_keeps_the_refusal() {
    let mut sources: Vec<_> = mock_sources().into_iter().map(|(part, s)| (part, owned(s, "Ann"))).collect();
    sources[2].1.owned = None;
    let doc = doc_with(&sources, None);
    let derived = derive(&doc);
    assert!(!derived.project_owned);
    let error = refused(&doc, &greedy(&derived.record));
    assert_eq!(
        error.field.as_deref(),
        Some("record/meta/allowExcessivelyViolentUsage"),
        "the head grants no content flag"
    );
    assert!(error.message.contains("the parts of `mira` allow"), "{}", error.message);
}

#[test]
fn a_missing_source_is_never_the_projects_own() {
    let mut sources: Vec<_> = mock_sources().into_iter().map(|(part, s)| (part, owned(s, "Ann"))).collect();
    sources.retain(|(part, _)| *part != "vest");
    let doc = doc_with(&sources, None);
    let derived = derive(&doc);
    assert!(!derived.project_owned && !derived.complete);
    let mut record = derived.record.clone();
    record.provenance.commercial = Commercial::Ok;
    let error = refused(&doc, &record);
    assert_eq!(error.field.as_deref(), Some("record/provenance/commercial"));
    assert!(error.message.contains("`vest` (no source recorded)"), "{}", error.message);
}

#[test]
fn private_test_material_is_never_waived_by_ownership() {
    let mut sources: Vec<_> = mock_sources().into_iter().map(|(part, s)| (part, owned(s, "Mira"))).collect();
    sources[2].1.provenance.private_test = true;
    let doc = doc_with(&sources, None);
    let derived = derive(&doc);
    assert!(derived.private_test && !derived.project_owned, "fan art stays private even when its drawer owns it");
    let mut record = derived.record.clone();
    record.provenance.private_test = false;
    let error = refused(&doc, &record);
    assert_eq!(error.field.as_deref(), Some("record/provenance/private_test"));
    assert!(error.message.contains("part `head` is private test material"), "{}", error.message);
    // Everything else is refused too: ownership is not an exception for this character at all.
    let mut generous = greedy(&derived.record);
    generous.provenance.private_test = true;
    assert!(set(&doc, &generous).is_err());
    // The derived record, flag set, is what can be stored.
    assert!(set(&doc, &derived.record).is_ok());
}

// ---------------------------------------------------------------------------------------------------------------
// licence.describe
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn describe_a_character_with_no_record_says_where_it_goes_and_how_to_make_one() {
    for purpose in ["private", "publish"] {
        let described = describe(&mock_doc(), purpose);
        assert!(!described.ok && described.record.is_none(), "{purpose}");
        assert_eq!(described.path, "characters/mira/licence");
        let missing = described.report.findings.iter().find(|f| f.rule == "rights.provenance_missing").unwrap();
        assert_eq!(missing.severity, Severity::Error);
        assert!(missing.hint.as_deref().unwrap().contains("licence.derive"), "{missing:?}");
    }
}

#[test]
fn describe_a_record_that_does_not_read_points_at_licence_set() {
    let mut root = mock_doc().root().clone();
    root["characters"]["mira"]["licence"] = json!({ "schema": 2 });
    let described = describe(&MemDoc::from_value(root), "private");
    let finding = described.report.findings.iter().find(|f| f.rule == "licence.record_invalid").unwrap();
    assert!(finding.message.contains("schema") && finding.hint.as_deref().unwrap().contains("licence.set"));
    assert!(!described.ok && described.record.is_none());
}

#[test]
fn describe_the_derived_record_is_ok_for_every_purpose_and_only_a_vrm_misses_authors_when_there_are_none() {
    let mut doc = mock_doc();
    let derived = derive(&doc).record;
    store(&mut doc, &derived);
    for purpose in ["private", "publish"] {
        let described = describe(&doc, purpose);
        assert!(described.ok && described.ok_for_vrm, "{purpose}: {:#?}", described.report);
        assert_eq!(rules(&described.report, Severity::Error).len(), 0);
        assert!(described.derived.complete && described.record.is_some());
    }
    // A record with no authors anywhere is fine for a glTF and flagged for a VRM.
    let mut anonymous = derive(&doc).record;
    anonymous.provenance.author = None;
    anonymous.meta.authors.clear();
    anonymous.meta.license_url = String::new();
    let doc = doc_with(&mock_sources(), Some(&anonymous));
    let described = describe(&doc, "publish");
    assert!(described.ok && !described.ok_for_vrm, "{:#?}", described.report);
    assert_eq!(
        rules(&described.report, Severity::Warning),
        ["vrm.meta_authors_missing", "vrm.meta_license_url_missing"]
    );
    let locations: Vec<_> = described.report.findings.iter().filter_map(|f| f.location.as_deref()).collect();
    assert!(locations.contains(&"characters/mira/licence/meta/authors"), "{locations:?}");
}

#[test]
fn describe_publish_needs_a_recorded_source_for_every_part_and_private_only_warns() {
    let mut sources = mock_sources();
    sources.retain(|(part, _)| *part != "hair");
    let mut doc = doc_with(&sources, None);
    let derived = derive(&doc).record;
    store(&mut doc, &derived);

    let publish = describe(&doc, "publish");
    assert!(!publish.ok);
    let rules_seen = rules(&publish.report, Severity::Error);
    assert!(
        rules_seen.contains(&"licence.part_source_missing") && rules_seen.contains(&"rights.gate_unknown"),
        "{rules_seen:?}"
    );
    assert_eq!(publish.derived.unresolved, ["hair"]);

    let private = describe(&doc, "private");
    assert!(private.ok, "{:#?}", private.report);
    assert_eq!(rules(&private.report, Severity::Warning), ["licence.part_source_missing"]);
}

#[test]
fn describe_flags_a_record_that_grants_more_than_its_parts_and_a_stale_one_after_a_part_changes() {
    // A record set while the hair allowed redistribution goes stale when the hair's row changes.
    let mut sources = mock_sources();
    sources[1].1.terms = Some(everything());
    let mut doc = doc_with(&sources, None);
    let mut record = derive(&doc).record;
    assert!(record.meta.terms.allow_redistribution);
    store(&mut doc, &record);
    assert!(describe(&doc, "publish").ok);

    let mut root = doc.root().clone();
    root["characters"]["mira"]["sources"]["hair"] = serde_json::to_value(&mock_sources()[1].1).unwrap();
    let stale = MemDoc::from_value(root);
    let publish = describe(&stale, "publish");
    assert!(!publish.ok);
    let finding = publish.report.findings.iter().find(|f| f.rule == "licence.exceeds_parts").unwrap();
    assert_eq!(finding.severity, Severity::Error);
    assert_eq!(finding.location.as_deref(), Some("characters/mira/licence/meta/allowRedistribution"));
    assert!(finding.message.contains("limited by `hair`"), "{}", finding.message);
    // For a file that stays on your machine it is a warning, not a blocker.
    let private = describe(&stale, "private");
    assert!(private.ok);
    assert_eq!(rules(&private.report, Severity::Warning), ["licence.exceeds_parts"]);

    // A record that contradicts its own provenance blocks every purpose, parts or not.
    record.provenance.commercial = Commercial::No;
    let contradictory = doc_with(&mock_sources(), Some(&record));
    for purpose in ["private", "publish"] {
        assert!(describe(&contradictory, purpose).report.has_rule("rights.terms_exceed_provenance"), "{purpose}");
    }
}

#[test]
fn describe_a_project_owned_record_notes_who_asserted_it_instead_of_failing() {
    let sources: Vec<_> = mock_sources().into_iter().map(|(part, s)| (part, owned(s, "Mira"))).collect();
    let mut doc = doc_with(&sources, None);
    let mut record = greedy(&derive(&doc).record);
    store(&mut doc, &record);
    let described = describe(&doc, "publish");
    assert!(described.ok, "{:#?}", described.report);
    let note = described.report.findings.iter().find(|f| f.rule == "licence.exceeds_parts_owned").unwrap();
    assert_eq!(note.severity, Severity::Info);
    assert!(note.message.contains("on the word of Mira"), "{}", note.message);

    // The same record written by hand, without the basis, is a warning that says how to record it.
    record.owned = None;
    let by_hand = doc_with(&sources, Some(&record));
    let described = describe(&by_hand, "publish");
    let warning = described.report.findings.iter().find(|f| f.rule == "licence.exceeds_parts_owned").unwrap();
    assert_eq!(warning.severity, Severity::Warning);
    assert!(warning.hint.as_deref().unwrap().contains("licence.set"));
    assert!(described.ok);
}

#[test]
fn a_private_test_part_makes_every_purpose_but_private_fail() {
    let mut sources = mock_sources();
    sources[2].1.provenance.private_test = true;
    let mut doc = doc_with(&sources, None);
    // With no record, with the right record and with a record that forgot the flag: the answer is the same.
    let derived = derive(&doc);
    assert!(derived.private_test);
    let mut forgetful = derived.record.clone();
    forgetful.provenance.private_test = false;
    let cases = [("no record", doc.clone()), ("forgetful", doc_with(&sources, Some(&forgetful)))];
    for (label, doc) in &cases {
        let publish = describe(doc, "publish");
        assert!(!publish.ok, "{label}");
        let gate = publish.report.findings.iter().find(|f| f.rule == "rights.gate_private_test").unwrap();
        assert_eq!(gate.severity, Severity::Error, "{label}");
        assert!(gate.message.contains("`head`"), "{label}: {}", gate.message);
        assert_eq!(gate.location.as_deref(), Some("characters/mira/sources/head"), "{label}");
    }
    assert!(!describe(&cases[1].1, "private").ok, "a record that forgot the flag is wrong even for a private file");
    assert!(describe(&cases[1].1, "private").report.has_rule("licence.exceeds_parts"));

    store(&mut doc, &derived.record);
    let publish = describe(&doc, "publish");
    assert!(!publish.ok);
    let gates: Vec<_> = publish.report.findings.iter().filter(|f| f.rule == "rights.gate_private_test").collect();
    assert_eq!(gates.len(), 1, "the gate's own finding and the part's are not both reported");
    let private = describe(&doc, "private");
    assert!(private.ok, "private test material may be exported for the author's own machine: {:#?}", private.report);
    // And `io.check_export` agrees about the stored record.
    assert!(!check_export(&doc, "glb", "publish").ok);
    assert!(check_export(&doc, "glb", "private").ok);
    assert!(derive(&doc).record.provenance.private_test);
}

// ---------------------------------------------------------------------------------------------------------------
// A stored record that goes stale when a part changes
// ---------------------------------------------------------------------------------------------------------------

/// `doc` with one part's source row replaced, as the library or an upload would when the part is swapped.
fn with_source(doc: &MemDoc, part: &str, source: &PartSource) -> MemDoc {
    let mut root = doc.root().clone();
    root["characters"]["mira"]["sources"][part] = serde_json::to_value(source).unwrap();
    MemDoc::from_value(root)
}

/// The mock character with `licence.derive`'s record stored: valid, and it passes every check.
fn stored_doc() -> MemDoc {
    let mut doc = mock_doc();
    let derived = derive(&doc).record;
    store(&mut doc, &derived);
    doc
}

/// The `licence.exceeds_parts*` findings of a check, as (rule, severity, location).
fn stale(check: &ExportCheck) -> Vec<(&str, Severity, &str)> {
    check
        .report
        .findings
        .iter()
        .filter(|f| f.rule.starts_with("licence.exceeds_parts"))
        .map(|f| (f.rule.as_str(), f.severity, f.location.as_deref().unwrap_or_default()))
        .collect()
}

const AT: &str = "characters/mira/licence/";

#[test]
fn a_record_that_was_valid_goes_stale_when_a_part_is_swapped_for_tighter_terms() {
    let doc = stored_doc();
    for (format, purpose) in [("vrm1", "private"), ("vrm1", "publish"), ("glb", "private"), ("glb", "publish")] {
        let check = check_export(&doc, format, purpose);
        assert!(check.ok && stale(&check).is_empty(), "{format} {purpose}: {:#?}", check.report);
    }

    // The body is swapped for one that allows only its author and non-profit use. The record, set a moment ago, still
    // says everyone and profit.
    let tighter = terms(|t| {
        *t = UseTerms {
            avatar_permission: AvatarPermission::OnlyAuthor,
            commercial_usage: CommercialUsage::PersonalNonProfit,
            ..everything()
        }
    });
    let swapped = with_source(&doc, "body", &source("CC0-1.0", Commercial::Ok, "Ann", tighter));
    for format in ["vrm1", "glb"] {
        let check = check_export(&swapped, format, "publish");
        assert!(!check.ok, "{format}: {:#?}", check.report);
        // Every field that exceeds the parts, each a finding of its own, located at that field of the record.
        assert_eq!(
            stale(&check),
            [
                ("licence.exceeds_parts", Severity::Error, &*format!("{AT}meta/avatarPermission")),
                ("licence.exceeds_parts", Severity::Error, &*format!("{AT}meta/commercialUsage")),
            ],
            "{format}"
        );
        let avatar = check.report.findings.iter().find(|f| f.rule == "licence.exceeds_parts").unwrap();
        assert!(
            avatar.message.contains("`meta/avatarPermission` is `everyone`, but the parts allow only `onlyAuthor`")
                && avatar.message.contains("limited by `body`"),
            "names the field, what each says and the part that limits it: {}",
            avatar.message
        );
        assert!(avatar.hint.as_deref().unwrap().contains("run licence.derive and licence.set"), "{avatar:?}");
        assert_eq!(check.errors, check.report.count(Severity::Error), "the counts stay true");
        assert_eq!(check.errors, 2);
    }

    // For a file that stays on your machine it is a warning, and the counts say so.
    let private = check_export(&swapped, "vrm1", "private");
    assert!(private.ok, "{:#?}", private.report);
    assert_eq!(
        stale(&private),
        [
            ("licence.exceeds_parts", Severity::Warning, &*format!("{AT}meta/avatarPermission")),
            ("licence.exceeds_parts", Severity::Warning, &*format!("{AT}meta/commercialUsage")),
        ]
    );
    assert_eq!(private.warnings, check_export(&doc, "vrm1", "private").warnings + 2);
    assert_eq!(private.warnings, private.report.count(Severity::Warning));

    // The hint works: derive and set again, and the check passes.
    let mut fixed = swapped.clone();
    let derived = derive(&fixed).record;
    store(&mut fixed, &derived);
    for (format, purpose) in [("vrm1", "publish"), ("glb", "publish"), ("vrm1", "private")] {
        let check = check_export(&fixed, format, purpose);
        assert!(check.ok && stale(&check).is_empty(), "{format} {purpose}: {:#?}", check.report);
    }
    println!(
        "--- io.check_export vrm1 publish, a part swapped after licence.set ---\n{}",
        serde_json::to_string_pretty(&check_export(&swapped, "vrm1", "publish").report.errors().collect::<Vec<_>>())
            .unwrap()
    );
}

#[test]
fn a_part_that_is_not_cleared_makes_the_status_and_every_flag_the_record_grants_stale() {
    let doc = stored_doc();
    let uncleared = with_source(&doc, "shirt", &source("CC-BY-NC-4.0", Commercial::No, "Dee", UseTerms::default()));
    let check = check_export(&uncleared, "glb", "publish");
    assert!(!check.ok);
    // The record still says `ok`, so the licence gate on the record alone cannot see it: only the parts can.
    assert!(!check.report.has_rule("rights.gate_non_commercial"), "{:#?}", check.report);
    let at: Vec<&str> = stale(&check).iter().map(|(_, _, location)| *location).collect();
    for field in ["provenance/commercial", "meta/avatarPermission", "meta/commercialUsage", "meta/modification"] {
        assert!(at.contains(&&*format!("{AT}{field}")), "{field} in {at:?}");
    }
    let status =
        check.report.findings.iter().find(|f| f.location.as_deref() == Some(&*format!("{AT}provenance/commercial")));
    let message = &status.expect("the status is a finding").message;
    assert!(message.contains("`provenance/commercial` is `ok`, but the parts allow only `no`"), "{message}");
    assert!(message.contains("limited by `shirt`"), "{message}");
}

#[test]
fn a_part_added_after_set_with_no_source_makes_the_record_stale_and_names_it() {
    let doc = stored_doc();
    let mut root = doc.root().clone();
    root["characters"]["mira"]["parts"]["cape"] = root["characters"]["mira"]["parts"]["shirt"].clone();
    let added = MemDoc::from_value(root);
    let check = check_export(&added, "vrm1", "publish");
    assert!(!check.ok, "{:#?}", check.report);
    let status = check
        .report
        .errors()
        .find(|f| f.location.as_deref() == Some(&*format!("{AT}provenance/commercial")))
        .expect("the status is stale");
    assert!(status.message.contains("`cape` (no source recorded)"), "{}", status.message);
    // Recording where it came from, and storing the record again, fixes it.
    let mut recorded = with_source(&added, "cape", &mock_sources()[3].1);
    let derived = derive(&recorded).record;
    store(&mut recorded, &derived);
    assert!(check_export(&recorded, "vrm1", "publish").ok);
}

#[test]
fn the_source_and_the_credits_of_a_changed_part_are_checked_too() {
    let doc = stored_doc();
    let mut imported = mock_sources()[3].1.clone();
    imported.provenance.source = Source::Imported { file: "shirt.vrm".into(), origin: None };
    let check = check_export(&with_source(&doc, "shirt", &imported), "glb", "publish");
    assert!(!check.ok);
    assert!(stale(&check).iter().any(|(_, _, at)| *at == format!("{AT}provenance/source")), "{:#?}", check.report);

    let mut credited = mock_sources()[0].1.clone();
    credited.provenance.attribution = Some("Body by Ann".into());
    let check = check_export(&with_source(&doc, "body", &credited), "glb", "publish");
    assert!(!check.ok);
    let credit = check.report.errors().find(|f| f.location.as_deref() == Some(&*format!("{AT}provenance/attribution")));
    assert!(credit.expect("the credit is stale").message.contains("\"Body by Ann\""));
}

#[test]
fn a_record_stricter_than_its_parts_and_a_missing_record_are_not_stale() {
    let mut doc = mock_doc();
    let mut strict = derive(&doc).record;
    strict.meta.terms.commercial_usage = CommercialUsage::PersonalNonProfit;
    strict.meta.terms.avatar_permission = AvatarPermission::OnlyAuthor;
    store(&mut doc, &strict);
    for (format, purpose) in [("vrm1", "publish"), ("glb", "publish"), ("vrm1", "private")] {
        let check = check_export(&doc, format, purpose);
        assert!(check.ok && stale(&check).is_empty(), "{format} {purpose}: {:#?}", check.report);
    }
    // No record: the absence is reported, and there is nothing to compare with the parts.
    let none = check_export(&mock_doc(), "vrm1", "publish");
    assert!(none.report.has_rule("rights.provenance_missing") && stale(&none).is_empty());
}

#[test]
fn the_project_owned_exception_is_honoured_while_every_part_is_still_owned() {
    let owned_sources: Vec<_> = mock_sources().into_iter().map(|(part, s)| (part, owned(s, "Mira"))).collect();
    let mut doc = doc_with(&owned_sources, None);
    let record = greedy(&derive(&doc).record);
    store(&mut doc, &record);
    assert!(stored(&doc).owned.is_some(), "set wrote who asserted it");
    for (format, purpose) in [("vrm1", "publish"), ("glb", "publish"), ("glb", "private")] {
        let check = check_export(&doc, format, purpose);
        assert!(check.ok && check.errors == 0, "{format} {purpose}: {:#?}", check.report);
        let notes = stale(&check);
        assert!(
            !notes.is_empty()
                && notes
                    .iter()
                    .all(|(rule, severity, _)| *rule == "licence.exceeds_parts_owned" && *severity == Severity::Info),
            "{notes:?}"
        );
    }

    // A part swapped for tighter terms, still the project's own: the exception still holds.
    let tighter = owned(source("CC0-1.0", Commercial::Ok, "Ann", UseTerms::default()), "Mira");
    let still_owned = with_source(&doc, "body", &tighter);
    assert!(check_export(&still_owned, "vrm1", "publish").ok);

    // A part swapped for one nobody owns: the record's own basis no longer covers it, whatever the record says.
    let outside = with_source(&doc, "body", &source("CC0-1.0", Commercial::Ok, "Ann", UseTerms::default()));
    let check = check_export(&outside, "vrm1", "publish");
    assert!(!check.ok, "{:#?}", check.report);
    assert!(check.report.errors().any(|f| f.rule == "licence.exceeds_parts"));
    assert!(check_export(&outside, "vrm1", "private").ok);

    // The same record written by hand, without the basis, is a warning that says how to record it, and does not block.
    let mut by_hand = stored(&doc);
    by_hand.owned = None;
    let check = check_export(&doc_with(&owned_sources, Some(&by_hand)), "vrm1", "publish");
    assert!(check.ok, "{:#?}", check.report);
    assert!(
        stale(&check)
            .iter()
            .all(|(rule, severity, _)| *rule == "licence.exceeds_parts_owned" && *severity == Severity::Warning)
    );
    assert!(
        check
            .report
            .findings
            .iter()
            .any(|f| f.severity == Severity::Warning && f.hint.as_deref().is_some_and(|h| h.contains("licence.set")))
    );
}

#[test]
fn a_private_test_part_is_never_waived_even_when_the_project_owns_it() {
    let mut owned_sources: Vec<_> = mock_sources().into_iter().map(|(part, s)| (part, owned(s, "Mira"))).collect();
    let mut doc = doc_with(&owned_sources, None);
    let record = greedy(&derive(&doc).record);
    store(&mut doc, &record);
    assert!(check_export(&doc, "glb", "publish").ok);
    owned_sources[2].1.provenance.private_test = true;
    let mut root = doc.root().clone();
    root["characters"]["mira"]["sources"]["head"] = serde_json::to_value(&owned_sources[2].1).unwrap();
    let doc = MemDoc::from_value(root);
    for purpose in ["private", "publish"] {
        let check = check_export(&doc, "glb", purpose);
        assert!(!check.ok, "{purpose}: {:#?}", check.report);
        let flag = format!("{AT}provenance/private_test");
        assert!(
            check.report.errors().any(|f| f.rule == "licence.exceeds_parts" && f.location.as_deref() == Some(&*flag)),
            "{purpose}"
        );
    }
}

#[test]
fn check_export_and_describe_say_the_same_about_a_stale_record() {
    let doc = stored_doc();
    let mut private_sources = mock_sources();
    private_sources[2].1.provenance.private_test = true;
    let mut credited = mock_sources();
    credited[0].1.provenance.attribution = Some("Body by Ann".into());
    let stale_docs = [
        with_source(&doc, "body", &source("CC0-1.0", Commercial::Ok, "Ann", UseTerms::default())),
        with_source(&doc, "shirt", &source("CC-BY-NC-4.0", Commercial::No, "Dee", UseTerms::default())),
        with_source(&doc, "head", &private_sources[2].1),
        with_source(&doc, "body", &credited[0].1),
        doc,
    ];
    let words = |report: &omuzaki_io_api::Report| -> Vec<omuzaki_io_api::Finding> {
        let about_parts = |f: &&omuzaki_io_api::Finding| {
            f.rule.starts_with("licence.exceeds_parts") || f.rule == "rights.gate_private_test"
        };
        report.findings.iter().filter(about_parts).cloned().collect()
    };
    for (n, doc) in stale_docs.iter().enumerate() {
        for purpose in ["private", "publish"] {
            let described = words(&describe(doc, purpose).report);
            let checked = words(&check_export(doc, "glb", purpose).report);
            assert_eq!(described, checked, "doc {n}, {purpose}");
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------------------------------------------

/// The answers the PR quotes, asserted and printed.
#[test]
fn licence_ops_answer_for_the_mock_character_asserted_and_printed() {
    let mut doc = mock_doc();
    let derived = derive(&doc);
    println!("--- licence.derive ---\n{}", serde_json::to_string_pretty(&derived).unwrap());

    let greedy_record = greedy(&derived.record);
    let refusal = refused(&doc, &greedy_record);
    println!("--- licence.set, more permissive than the parts ---\n{refusal:#?}");

    println!("--- licence.describe publish, before ---\n{}", describe_json(&doc, "publish"));
    store(&mut doc, &derived.record);
    println!("--- licence.describe publish, after licence.set ---\n{}", describe_json(&doc, "publish"));
    let check = check_export(&doc, "vrm1", "publish");
    println!(
        "--- io.check_export vrm1 publish, after licence.set ---\nok={} errors={} warnings={}",
        check.ok, check.errors, check.warnings
    );
    assert!(check.ok);

    let mut fan_art = mock_sources();
    fan_art[2].1.provenance.private_test = true;
    let private = doc_with(&fan_art, None);
    println!("--- licence.describe publish, a private test part ---\n{}", describe_json(&private, "publish"));
    assert!(!describe(&private, "publish").ok);
}

fn describe_json(doc: &MemDoc, purpose: &str) -> String {
    let out =
        plan(doc, "licence.describe", json!({ "character": "mira", "purpose": purpose })).unwrap().output.unwrap();
    // Keep the transcript short: the record is `licence.derive`'s answer already.
    let mut out = out;
    out.as_object_mut().unwrap().remove("record");
    serde_json::to_string_pretty(&out).unwrap()
}

/// A guard that the fixtures stay what the assertions above assume.
#[test]
fn the_fixtures_are_the_mock_characters_five_parts() {
    let doc = mock_doc();
    let parts: Vec<&String> = doc.root()["characters"]["mira"]["parts"].as_object().unwrap().keys().collect();
    assert_eq!(parts, PARTS);
    assert_eq!(
        source_path(&"mira".parse().unwrap(), &"hair".parse().unwrap()).to_string(),
        "characters/mira/sources/hair"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// provenance.licence against the join of the parts' licences (D10)
// ---------------------------------------------------------------------------------------------------------------

/// `derive`'s record with another SPDX licence expression.
fn with_licence(record: &LicenceRecord, licence: &str) -> LicenceRecord {
    let mut record = record.clone();
    record.provenance.licence = licence.into();
    record
}

#[test]
fn set_refuses_a_licence_that_leaves_out_a_part() {
    let doc = mock_doc();
    let derived = derive(&doc).record;
    assert_eq!(derived.provenance.licence, "CC-BY-4.0 AND CC0-1.0");
    let error = refused(&doc, &with_licence(&derived, "CC0-1.0"));
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));
    assert!(error.message.contains("`head`") && error.message.contains("CC-BY-4.0"), "{}", error.message);
}

#[test]
fn set_accepts_any_spelling_and_any_superset_of_the_joined_licences() {
    let doc = mock_doc();
    let derived = derive(&doc).record;
    for ok in
        ["CC0-1.0 AND CC-BY-4.0", "cc-by-4.0 and cc0-1.0", "(CC-BY-4.0 AND CC0-1.0)", "CC-BY-4.0  AND  CC0-1.0 AND MIT"]
    {
        assert!(set(&doc, &with_licence(&derived, ok)).is_ok(), "{ok}");
    }

    // A part licensed `MIT OR Apache-2.0` is one bracketed term; `MIT` alone is not it.
    let mut sources = mock_sources();
    sources[0].1.provenance.licence = "MIT OR Apache-2.0".into();
    let doc = doc_with(&sources, None);
    let derived = derive(&doc).record;
    assert_eq!(derived.provenance.licence, "CC-BY-4.0 AND CC0-1.0 AND (MIT OR Apache-2.0)");
    assert!(set(&doc, &with_licence(&derived, "(MIT OR Apache-2.0) AND CC0-1.0 AND CC-BY-4.0")).is_ok());
    let error = refused(&doc, &with_licence(&derived, "CC-BY-4.0 AND CC0-1.0 AND MIT"));
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));
    assert!(error.message.contains("`body`"), "{}", error.message);
}

#[test]
fn a_part_with_no_source_row_and_a_character_with_no_parts_need_the_unknown_licence() {
    let mut sources = mock_sources();
    sources.retain(|(part, _)| *part != "vest");
    let doc = doc_with(&sources, None);
    let derived = derive(&doc).record;
    assert_eq!(derived.provenance.licence, "CC-BY-4.0 AND CC0-1.0 AND LicenseRef-Unknown");
    let error = refused(&doc, &with_licence(&derived, "CC-BY-4.0 AND CC0-1.0"));
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));
    assert!(error.message.contains("`vest`") && error.message.contains("LicenseRef-Unknown"), "{}", error.message);

    let mut root = doc_with(&[], None).root().clone();
    root["characters"]["mira"]["parts"] = json!({});
    let empty = MemDoc::from_value(root);
    let derived = derive(&empty).record;
    assert_eq!(derived.provenance.licence, "LicenseRef-Unknown");
    assert!(set(&empty, &derived).is_ok());
    let error = refused(&empty, &with_licence(&derived, "CC0-1.0"));
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));
    assert!(error.message.contains("no parts"), "{}", error.message);
}

#[test]
fn a_stale_licence_after_a_part_changes_is_reported_and_re_deriving_clears_it() {
    let doc = stored_doc();
    let swapped = with_source(&doc, "body", &source("GPL-3.0-only", Commercial::Ok, "Ann", everything()));
    let at = format!("{AT}provenance/licence");
    for (purpose, rule_severity) in [("publish", Severity::Error), ("private", Severity::Warning)] {
        let check = check_export(&swapped, "glb", purpose);
        assert!(
            stale(&check).contains(&("licence.exceeds_parts", rule_severity, at.as_str())),
            "{purpose}: {:#?}",
            check.report
        );
        assert_eq!(check.ok, rule_severity != Severity::Error, "{purpose}");
    }
    let described = describe(&swapped, "publish");
    assert!(described.report.errors().any(|f| f.rule == "licence.exceeds_parts"), "{:#?}", described.report);

    let mut fixed = swapped;
    let derived = derive(&fixed).record;
    assert!(derived.provenance.licence.contains("GPL-3.0-only"));
    store(&mut fixed, &derived);
    let check = check_export(&fixed, "glb", "publish");
    assert!(check.ok && stale(&check).is_empty(), "{:#?}", check.report);
}

#[test]
fn an_owned_character_may_keep_a_simpler_licence_and_a_non_owned_part_refuses_it() {
    let owned_sources: Vec<_> = mock_sources().into_iter().map(|(part, s)| (part, owned(s, "Mira"))).collect();
    let mut doc = doc_with(&owned_sources, None);
    let simple = with_licence(&derive(&doc).record, "LicenseRef-Mira-Own");
    let planned = store(&mut doc, &simple);
    let beyond = planned.output.unwrap()["granted_beyond_the_parts"].clone();
    assert!(beyond.as_array().unwrap().iter().any(|f| f == "provenance/licence"), "{beyond}");
    let check = check_export(&doc, "glb", "publish");
    let at = format!("{AT}provenance/licence");
    assert!(check.ok && stale(&check).contains(&("licence.exceeds_parts_owned", Severity::Info, at.as_str())));

    let mut sources = owned_sources;
    sources[2].1.owned = None;
    let error = refused(&doc_with(&sources, None), &simple);
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));
}

#[test]
fn credits_match_by_token_not_by_substring() {
    let doc = mock_doc();
    let derived = derive(&doc).record;
    let mut inside = derived.clone();
    inside.provenance.attribution = Some("Model by Mira. xHead by Bo, CC BY 4.0y".into());
    let error = refused(&doc, &inside);
    assert_eq!(error.field.as_deref(), Some("record/provenance/attribution"));
    let mut apart = derived;
    apart.provenance.attribution = Some("Model by Mira.\nHead by Bo, CC BY 4.0!".into());
    assert!(set(&doc, &apart).is_ok());
}

#[test]
fn a_top_level_or_keeps_the_licence_atomic_and_non_ascii_text_does_not_panic() {
    let doc = mock_doc();
    let derived = derive(&doc).record;
    // `A OR B AND C` is `A OR (B AND C)`: the MIT alternative carries neither required licence.
    let error = refused(&doc, &with_licence(&derived, "MIT OR Apache-2.0 AND CC-BY-4.0 AND CC0-1.0"));
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));
    let error = refused(&doc, &with_licence(&derived, "MIT OR 日本語"));
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));
    let error = refused(&doc, &with_licence(&derived, "日本語 AND CC0-1.0"));
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));

    // Malformed input is a refusal, never a panic: the doubled `AND` is a term nothing satisfies.
    let error = refused(&doc, &with_licence(&derived, "MIT AND AND Apache-2.0"));
    assert_eq!(error.field.as_deref(), Some("record/provenance/licence"));
    let broken = doc_with(&mock_sources(), Some(&with_licence(&derived, "MIT AND AND Apache-2.0")));
    assert!(!check_export(&broken, "glb", "publish").ok);
    assert!(describe(&broken, "publish").report.errors().any(|f| f.rule == "licence.exceeds_parts"));

    // A record written by hand with the loose expression is flagged by the export check too.
    let loose = doc_with(&mock_sources(), Some(&with_licence(&derived, "MIT OR Apache-2.0 AND CC-BY-4.0 AND CC0-1.0")));
    let check = check_export(&loose, "glb", "publish");
    let at = format!("{AT}provenance/licence");
    assert!(!check.ok && stale(&check).contains(&("licence.exceeds_parts", Severity::Error, at.as_str())));
}

#[test]
fn overlapping_credits_and_unicode_spacing_never_make_a_derived_record_refuse_itself() {
    // Credits `JoAnn` and `Ann; Ann` derive "JoAnn; Ann; Ann": "Ann; Ann" is kept, but only the second candidate counts.
    let mut sources = mock_sources();
    sources[0].1.provenance.attribution = Some("JoAnn".into());
    sources[1].1.provenance.attribution = Some("Ann; Ann".into());
    let mut doc = doc_with(&sources, None);
    let derived = derive(&doc).record;
    assert!(derived.provenance.attribution.as_deref().unwrap().contains("JoAnn; Ann; Ann"), "{derived:#?}");
    store(&mut doc, &derived);
    let check = check_export(&doc, "glb", "publish");
    assert!(check.ok && stale(&check).is_empty(), "{:#?}", check.report);

    // Non-breaking spaces around `AND` are whitespace like any other.
    let spaced = with_licence(&derived, "CC0-1.0\u{a0}AND\u{a0}CC-BY-4.0");
    assert!(set(&doc, &spaced).is_ok());
    let stored_spaced = doc_with(&sources, Some(&spaced));
    let check = check_export(&stored_spaced, "glb", "publish");
    assert!(check.ok && stale(&check).is_empty(), "{:#?}", check.report);
}
