//! A creator's licence terms through an export: never lost, never widened.
//!
//! 1. The 0.x `licenseName` keeps its identity (`CC_BY_SA` stays `CC_BY_SA`); where 1.0 terms cannot be written in 0.x
//!    exactly, the most restrictive faithful name is used and the finding names the field.
//! 2. Every rights field of both versions is either carried or reported, and unknown meta keys survive a round trip.
//! 3. Publishing someone else's model is refused when its licence forbids it (spec VF-08).
//!
//! The expected values are written from the VRM specs (`VRMC_vrm-1.0/meta.md`, `0.0/README.md`) and the design table in
//! `docs/design/io-api.md`, D5, not read back from the code under test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines, clippy::type_complexity)]

mod common;

use omuzaki_io_api::{
    CreditNotation, ExportOptions, IoError, IoScene, Modification, Purpose, Report, Severity, VrmMeta,
};
use omuzaki_io_vrm::{Vrm0Exporter, Vrm0Importer, Vrm1Exporter, Vrm1Importer};
use serde_json::{Value, json};

use common::{
    avatar, export_with, import, options, parts, vrm0_friendly, vrm0_with_meta, vrm1_with_meta, written_meta,
};

const PL: &str = "https://vrm.dev/licenses/1.0/";

fn to0(scene: &IoScene) -> (Vec<u8>, Report) {
    export_with(&Vrm0Exporter, scene, &options()).expect("export to 0.x")
}

fn to1(scene: &IoScene) -> (Vec<u8>, Report) {
    export_with(&Vrm1Exporter, scene, &options()).expect("export to 1.0")
}

fn from1(patch: &Value) -> IoScene {
    import(&Vrm1Importer, &vrm1_with_meta(patch)).scene
}

fn from0(meta: &Value) -> IoScene {
    import(&Vrm0Importer, &vrm0_with_meta(meta)).scene
}

/// The locations of the findings with `rule`, sorted.
fn locations(report: &Report, rule: &str) -> Vec<String> {
    let mut out: Vec<String> =
        report.findings.iter().filter(|f| f.rule == rule).filter_map(|f| f.location.clone()).collect();
    out.sort();
    out
}

/// True if some finding names `needle` in its message or location.
fn mentions(report: &Report, needle: &str) -> bool {
    report.findings.iter().any(|f| {
        f.severity != Severity::Info
            && (f.message.contains(needle) || f.location.as_deref().is_some_and(|l| l.contains(needle)))
    })
}

fn modification_rank(m: Modification) -> u8 {
    match m {
        Modification::Prohibited => 0,
        Modification::AllowModification => 1,
        Modification::AllowModificationRedistribution => 2,
    }
}

/// The written 0.x file grants no more than the scene's terms did: read it back and compare the licence-level terms.
fn assert_not_widened(written_0x: &[u8], scene: &IoScene, what: &str) {
    let back = import(&Vrm0Importer, written_0x).scene.meta.terms;
    let own = &scene.meta.terms;
    assert!(!back.allow_redistribution || own.allow_redistribution, "{what}: redistribution was widened");
    assert!(
        modification_rank(back.modification) <= modification_rank(own.modification),
        "{what}: modification widened"
    );
    assert!(
        back.credit_notation == CreditNotation::Required || own.credit_notation == CreditNotation::Unnecessary,
        "{what}: the credit requirement was dropped"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// 1. Licence identity on a 0.x export

const NAMES: [&str; 8] =
    ["Redistribution_Prohibited", "CC0", "CC_BY", "CC_BY_SA", "CC_BY_ND", "CC_BY_NC", "CC_BY_NC_SA", "CC_BY_NC_ND"];

#[test]
fn every_known_0x_licence_name_survives_a_round_trip() {
    for name in NAMES {
        let scene =
            from0(&json!({"title": "T", "author": "A", "licenseName": name, "commercialUssageName": "Disallow"}));
        let (out, report) = to0(&scene);
        assert_eq!(written_meta(&out, "VRM")["licenseName"], name, "{name} was rewritten");
        assert!(!report.has_rule("vrm0.meta_licence"), "{name}: nothing is lost, so nothing is warned: {report:?}");
        assert_not_widened(&out, &scene, name);
    }
}

#[test]
fn other_keeps_its_licence_document() {
    let meta = json!({"title": "T", "author": "A", "licenseName": "Other", "commercialUssageName": "Disallow",
                      "otherLicenseUrl": "https://example.org/terms"});
    let (out, report) = to0(&from0(&meta));
    let written = written_meta(&out, "VRM");
    assert_eq!(
        (written["licenseName"].as_str(), written["otherLicenseUrl"].as_str()),
        (Some("Other"), Some("https://example.org/terms"))
    );
    assert!(!report.has_rule("vrm0.meta_licence"), "{report:?}");
    // "Other" without any document has nothing to point at: it falls to the strictest name, and says so.
    let (out, report) = to0(&from0(&json!({"title": "T", "author": "A", "licenseName": "Other"})));
    assert_eq!(written_meta(&out, "VRM")["licenseName"], "Redistribution_Prohibited");
    assert_eq!(locations(&report, "vrm0.meta_licence"), ["meta/licenseUrl"], "the change of licence is reported");
}

#[test]
fn one_zero_terms_map_to_the_most_restrictive_faithful_licence() {
    // (meta patch of a VRM Public License 1.0 file, licenseName, otherLicenseUrl, fields the 0.x file cannot say)
    let custom = "https://example.org/terms";
    let rows: [(Value, &str, Option<&str>, &[&str]); 7] = [
        // The spec defaults are the strictest terms and match Redistribution_Prohibited exactly.
        (json!({}), "Redistribution_Prohibited", None, &[]),
        (json!({"modification": "allowModification"}), "Redistribution_Prohibited", None, &["meta/modification"]),
        (json!({"creditNotation": "unnecessary"}), "Redistribution_Prohibited", None, &["meta/creditNotation"]),
        // Redistribution has no 0.x name outside the CC set, and CC would change the licence: Other + the document.
        (
            json!({"allowRedistribution": true, "modification": "allowModificationRedistribution"}),
            "Other",
            Some(PL),
            &["meta/allowRedistribution", "meta/modification"],
        ),
        (json!({"allowRedistribution": true}), "Other", Some(PL), &["meta/allowRedistribution"]),
        // A licence document that is not the VRM Public License is never replaced by a name.
        (json!({"licenseUrl": custom}), "Other", Some(custom), &[]),
        (
            json!({"licenseUrl": custom, "allowRedistribution": true}),
            "Other",
            Some(custom),
            &["meta/allowRedistribution"],
        ),
    ];
    for (patch, name, document, fields) in rows {
        let scene = from1(&patch);
        let (out, report) = to0(&scene);
        let written = written_meta(&out, "VRM");
        assert_eq!(written["licenseName"], name, "{patch}");
        assert_eq!(written["otherLicenseUrl"].as_str(), document, "{patch}");
        assert_eq!(locations(&report, "vrm0.meta_licence"), fields, "{patch}");
        assert_not_widened(&out, &scene, &patch.to_string());
    }
}

#[test]
fn a_finding_names_the_field_its_value_and_why() {
    let (_, report) = to0(&from1(&json!({"modification": "allowModification"})));
    let f = report.findings.iter().find(|f| f.rule == "vrm0.meta_licence").expect("a finding");
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.location.as_deref(), Some("meta/modification"));
    assert!(
        f.message.contains("modification = \"allowModification\"") && f.message.contains("strictest 0.x licence"),
        "{}",
        f.message
    );
}

#[test]
fn terms_narrowed_below_the_source_licence_never_keep_the_wider_name() {
    // CC_BY allows modification; the terms were since narrowed to "prohibited". Writing CC_BY would widen them.
    let mut scene =
        from0(&json!({"title": "T", "author": "A", "licenseName": "CC_BY", "commercialUssageName": "Disallow"}));
    scene.meta.terms.modification = Modification::Prohibited;
    let (out, report) = to0(&scene);
    assert_eq!(written_meta(&out, "VRM")["licenseName"], "Redistribution_Prohibited");
    assert_not_widened(&out, &scene, "narrowed CC_BY");
    // The other way: terms wider than the licence name keep the name (the name is the stricter one) and say so.
    let mut scene =
        from0(&json!({"title": "T", "author": "A", "licenseName": "CC_BY_ND", "commercialUssageName": "Disallow"}));
    scene.meta.terms.modification = Modification::AllowModificationRedistribution;
    let (out, report_wide) = to0(&scene);
    assert_eq!(written_meta(&out, "VRM")["licenseName"], "CC_BY_ND");
    assert_eq!(locations(&report_wide, "vrm0.meta_licence"), ["meta/modification"]);
    assert_eq!(locations(&report, "vrm0.meta_licence"), ["meta/allowRedistribution"]);
}

#[test]
fn share_alike_is_reported_when_a_0x_model_is_written_as_1_0() {
    for name in ["CC_BY_SA", "CC_BY_NC_SA"] {
        let scene =
            from0(&json!({"title": "T", "author": "A", "licenseName": name, "commercialUssageName": "Disallow"}));
        let (_, report) = to1(&scene);
        assert!(locations(&report, "vrm.meta_licence_condition") == ["meta/licenseUrl"], "{name}");
    }
    let plain =
        from0(&json!({"title": "T", "author": "A", "licenseName": "CC_BY", "commercialUssageName": "Disallow"}));
    assert!(!to1(&plain).1.has_rule("vrm.meta_licence_condition"));
}

// ---------------------------------------------------------------------------------------------------------------
// 2. No silent drops of rights data

#[test]
fn both_0x_urls_are_kept_and_come_back() {
    let base = json!({"title": "T", "author": "A", "licenseName": "Other", "commercialUssageName": "Disallow"});
    let with = |license: Option<&str>, permission: Option<&str>| {
        let mut m = base.clone();
        if let Some(l) = license {
            m["otherLicenseUrl"] = json!(l);
        }
        if let Some(p) = permission {
            m["otherPermissionUrl"] = json!(p);
        }
        from0(&m)
    };
    // Different URLs: both are in the 1.0 field, joined as UniVRM's migrator joins them.
    let both = with(Some("https://a.example/licence"), Some("https://b.example/permission"));
    assert_eq!(
        both.meta.other_license_url.as_deref(),
        Some("'https://a.example/licence', 'https://b.example/permission'")
    );
    let written = written_meta(&to0(&both).0, "VRM");
    assert_eq!(written["otherLicenseUrl"], "https://a.example/licence");
    assert_eq!(written["otherPermissionUrl"], "https://b.example/permission");
    assert_eq!(
        written_meta(&to1(&both).0, "VRMC_vrm")["otherLicenseUrl"],
        "'https://a.example/licence', 'https://b.example/permission'"
    );
    // The same URL twice, or only one of them, is just that URL.
    let same = with(Some("https://a.example/x"), Some("https://a.example/x"));
    assert_eq!(same.meta.other_license_url.as_deref(), Some("https://a.example/x"));
    let only_permission = with(None, Some("https://b.example/permission"));
    assert_eq!(only_permission.meta.other_license_url.as_deref(), Some("https://b.example/permission"));
    let written = written_meta(&to0(&only_permission).0, "VRM");
    assert_eq!(written["otherPermissionUrl"], "https://b.example/permission");
}

#[test]
fn unknown_1_0_meta_keys_come_back_verbatim() {
    let extra = json!({
        "extras": {"studio": "ACME", "ai": false},
        "extensions": {"EXT_terms": {"noAiTraining": true}},
        "vendorTerms": "no resale",
    });
    let imported = import(&Vrm1Importer, &vrm1_with_meta(&extra));
    let note = imported.report.findings.iter().find(|f| f.rule == "vrm.meta_unknown_keys").expect("an import note");
    assert_eq!(note.severity, Severity::Info);
    for key in ["extras", "extensions", "vendorTerms"] {
        assert!(note.message.contains(key), "{key}: {}", note.message);
    }
    // Back to 1.0: every key, value for value, and no trace of the scene's private carrier in the file.
    let (out, report) = to1(&imported.scene);
    let written = written_meta(&out, "VRMC_vrm");
    for (key, value) in extra.as_object().unwrap() {
        assert_eq!(&written[key], value, "{key}");
    }
    assert!(!report.has_rule("vrm.meta_unknown_keys"), "{report:?}");
    // To 0.x: 0.x cannot hold them, so each is listed, and the carrier is not written there either.
    let (out, report) = to0(&imported.scene);
    let f = report.findings.iter().find(|f| f.rule == "vrm.meta_unknown_keys").expect("a warning");
    assert_eq!(f.severity, Severity::Warning);
    for key in ["extras", "extensions", "vendorTerms"] {
        assert!(f.message.contains(key), "{key}: {}", f.message);
    }
    let (doc, _) = parts(&out);
    assert!(doc["extensions"]["VRM"]["meta"].get("extras").is_none(), "0.x has no place for them, so none is written");
}

#[test]
fn unknown_0x_meta_keys_come_back_verbatim_and_are_reported_for_1_0() {
    let meta = json!({"title": "T", "author": "A", "licenseName": "CC_BY", "commercialUssageName": "Disallow",
                      "vendorTerms": {"nested": [1, 2]}});
    let imported = import(&Vrm0Importer, &vrm0_with_meta(&meta));
    assert!(imported.report.has_rule("vrm.meta_unknown_keys"));
    let (out, report) = to0(&imported.scene);
    assert_eq!(written_meta(&out, "VRM")["vendorTerms"], json!({"nested": [1, 2]}));
    assert!(!report.has_rule("vrm.meta_unknown_keys"));
    let (out, report) = to1(&imported.scene);
    assert!(mentions(&report, "vendorTerms"), "{report:?}");
    assert!(written_meta(&out, "VRMC_vrm").get("vendorTerms").is_none());
}

#[test]
fn a_crafted_file_cannot_plant_meta_keys_through_an_extension() {
    // An earlier version carried unknown meta keys in a reserved root extension; the scene's meta carries them now, so
    // that name means nothing and a file that uses it cannot reach the meta.
    let (mut doc, bin) = common::minimal1();
    doc["extensions"]["OMUZAKI_vrm_meta_residue"] = json!({"vrm1": {"allowRedistribution": true, "planted": 1}});
    doc["extensionsUsed"] = json!(["VRMC_vrm", "OMUZAKI_vrm_meta_residue"]);
    let imported = import(&Vrm1Importer, &common::glb(&doc, &bin));
    assert!(imported.scene.meta.extras.is_empty() && !imported.scene.meta.terms.allow_redistribution);
    let written = written_meta(&to1(&imported.scene).0, "VRMC_vrm");
    assert!(written.get("planted").is_none());
    assert_eq!(written["allowRedistribution"], false);
}

#[test]
fn unknown_keys_can_never_override_a_typed_term_on_the_way_out() {
    // A scene edited by hand puts typed keys into the extras: the typed field wins.
    let mut scene = from1(&json!({}));
    scene.meta.extras.insert("allowRedistribution".into(), json!(true));
    scene.meta.extras.insert("licenseUrl".into(), json!("https://forged.example/"));
    let written = written_meta(&to1(&scene).0, "VRMC_vrm");
    assert_eq!(written["allowRedistribution"], false);
    assert_eq!(written["licenseUrl"], PL);
}

#[test]
fn third_party_licences_are_carried_into_the_0x_reference_and_reported() {
    let scene = from1(
        &json!({"thirdPartyLicenses": "Hair texture: CC-BY 4.0 by Jo\nShader: MIT", "references": ["https://example.org/original"]}),
    );
    let (out, report) = to0(&scene);
    let reference = written_meta(&out, "VRM")["reference"].as_str().unwrap().to_owned();
    assert!(reference.contains("https://example.org/original"), "{reference}");
    assert!(reference.contains("Hair texture: CC-BY 4.0 by Jo\nShader: MIT"), "{reference}");
    assert_eq!(locations(&report, "vrm0.meta_third_party_licenses"), ["meta/thirdPartyLicenses"]);
}

#[test]
fn corporation_and_content_flags_name_their_field() {
    let scene = from1(
        &json!({"commercialUsage": "corporation", "allowPoliticalOrReligiousUsage": true, "allowAntisocialOrHateUsage": true}),
    );
    let (_, report) = to0(&scene);
    assert_eq!(locations(&report, "vrm0.meta_commercial"), ["meta/commercialUsage"]);
    assert_eq!(
        locations(&report, "vrm0.meta_content_flags"),
        ["meta/allowAntisocialOrHateUsage", "meta/allowPoliticalOrReligiousUsage"]
    );
}

/// How a field of the other version's file is carried: `Some(check)` says where the value lands in the written meta
/// (and the whole document); a field with `None` can only be reported.
type Carried = Option<fn(&Value, &Value) -> bool>;

#[test]
fn every_1_0_rights_field_is_carried_or_reported() {
    // (field, probe value, where the 0.x file carries it)
    let fields: Vec<(&str, Value, Carried)> = vec![
        ("name", json!("Probe Name"), Some(|m, _| m["title"] == "Probe Name")),
        ("version", json!("7.1"), Some(|m, _| m["version"] == "7.1")),
        ("authors", json!(["Author A", "Author B"]), Some(|m, _| m["author"] == "Author A, Author B")),
        ("copyrightInformation", json!("(c) ACME"), Some(|_, d| d["asset"]["copyright"] == "(c) ACME")),
        ("contactInformation", json!("probe@example.org"), Some(|m, _| m["contactInformation"] == "probe@example.org")),
        ("references", json!(["https://example.org/ref"]), Some(|m, _| m["reference"] == "https://example.org/ref")),
        (
            "thirdPartyLicenses",
            json!("Hair: CC-BY"),
            Some(|m, _| m["reference"].as_str().is_some_and(|r| r.contains("Hair: CC-BY"))),
        ),
        (
            "licenseUrl",
            json!("https://example.org/terms"),
            Some(|m, _| m["otherLicenseUrl"] == "https://example.org/terms"),
        ),
        ("avatarPermission", json!("everyone"), Some(|m, _| m["allowedUserName"] == "Everyone")),
        ("allowExcessivelyViolentUsage", json!(true), Some(|m, _| m["violentUssageName"] == "Allow")),
        ("allowExcessivelySexualUsage", json!(true), Some(|m, _| m["sexualUssageName"] == "Allow")),
        ("commercialUsage", json!("personalProfit"), Some(|m, _| m["commercialUssageName"] == "Allow")),
        ("allowPoliticalOrReligiousUsage", json!(true), None),
        ("allowAntisocialOrHateUsage", json!(true), None),
        ("creditNotation", json!("unnecessary"), None),
        ("allowRedistribution", json!(true), None),
        ("modification", json!("allowModificationRedistribution"), None),
        (
            "otherLicenseUrl",
            json!("https://example.org/more"),
            Some(|m, _| m["otherPermissionUrl"] == "https://example.org/more"),
        ),
        // Keys the typed meta does not name.
        ("extras", json!({"studio": "ACME"}), None),
        ("extensions", json!({"EXT_terms": {"a": 1}}), None),
        ("vendorTerms", json!("no resale"), None),
    ];
    // The table must cover every key `VrmMeta` has, so a new field cannot slip past this test.
    let mut full = VrmMeta::new("n", vec!["a".into()], "u");
    full.version = Some("v".into());
    full.copyright_information = Some("c".into());
    full.contact_information = Some("c".into());
    full.references = vec!["r".into()];
    full.third_party_licenses = Some("t".into());
    full.thumbnail_image = Some(omuzaki_io_api::ImageIx(0));
    full.other_license_url = Some("o".into());
    for key in serde_json::to_value(&full).unwrap().as_object().unwrap().keys() {
        // The thumbnail is an image index, not a right.
        assert!(key == "thumbnailImage" || fields.iter().any(|(f, ..)| f == key), "{key} is not in the table");
    }
    for (key, probe, carried) in fields {
        let scene = from1(&member(key, &probe));
        let (out1, _) = to1(&scene);
        assert_eq!(written_meta(&out1, "VRMC_vrm")[key], probe, "{key}: 1.0 to 1.0 must carry it exactly");
        let (out0, report) = to0(&scene);
        let (doc0, _) = parts(&out0);
        let meta0 = &doc0["extensions"]["VRM"]["meta"];
        let kept = carried.is_some_and(|check| check(meta0, &doc0));
        assert!(kept || mentions(&report, key), "{key} = {probe} was dropped without a finding");
        // A field the 0.x file cannot hold must be reported even if some other field happens to carry its value.
        if carried.is_none() {
            assert!(mentions(&report, key), "{key} cannot be carried and must be reported");
        }
    }
}

#[test]
fn every_0x_rights_field_is_carried_or_reported() {
    // (field, probe, where the 1.0 file carries it)
    let fields: Vec<(&str, Value, Carried)> = vec![
        ("title", json!("Probe"), Some(|m, _| m["name"] == "Probe")),
        ("version", json!("9"), Some(|m, _| m["version"] == "9")),
        ("author", json!("Au"), Some(|m, _| m["authors"] == json!(["Au"]))),
        ("contactInformation", json!("c@example.org"), Some(|m, _| m["contactInformation"] == "c@example.org")),
        ("reference", json!("https://example.org/r"), Some(|m, _| m["references"] == json!(["https://example.org/r"]))),
        (
            "allowedUserName",
            json!("ExplicitlyLicensedPerson"),
            Some(|m, _| m["avatarPermission"] == "onlySeparatelyLicensedPerson"),
        ),
        ("violentUssageName", json!("Allow"), Some(|m, _| m["allowExcessivelyViolentUsage"] == true)),
        ("sexualUssageName", json!("Allow"), Some(|m, _| m["allowExcessivelySexualUsage"] == true)),
        ("commercialUssageName", json!("Allow"), Some(|m, _| m["commercialUsage"] == "personalProfit")),
        (
            "otherPermissionUrl",
            json!("https://example.org/perm"),
            Some(|m, _| m["otherLicenseUrl"] == "https://example.org/perm"),
        ),
        // CC_BY_SA: redistribution, modification and credit are flags in 1.0; share-alike is not, so it is reported.
        ("licenseName", json!("CC_BY_SA"), None),
        (
            "otherLicenseUrl",
            json!("https://example.org/lic"),
            Some(|m, _| m["otherLicenseUrl"] == "https://example.org/lic"),
        ),
        ("vendorTerms", json!("no resale"), None),
    ];
    // The table must cover every key `Vrm0Meta` has, so a new field cannot slip past this test.
    let some = || Some("x".to_owned());
    let full = omuzaki_io_api::Vrm0Meta {
        title: some(),
        version: some(),
        author: some(),
        contact_information: some(),
        reference: some(),
        texture: Some(0),
        allowed_user_name: some(),
        violent_ussage_name: some(),
        sexual_ussage_name: some(),
        commercial_ussage_name: some(),
        other_permission_url: some(),
        license_name: some(),
        other_license_url: some(),
        extras: serde_json::Map::new(),
    };
    for key in serde_json::to_value(&full).unwrap().as_object().unwrap().keys() {
        // The thumbnail is a texture index, not a right.
        assert!(key == "texture" || fields.iter().any(|(f, ..)| f == key), "{key} is not in the table");
    }
    let base = json!({"title": "T", "author": "A", "licenseName": "CC_BY", "commercialUssageName": "Disallow"});
    for (key, probe, carried) in fields {
        let mut meta = base.clone();
        meta[key] = probe.clone();
        let scene = from0(&meta);
        let (out0, report0) = to0(&scene);
        assert_eq!(written_meta(&out0, "VRM")[key], probe, "{key}: 0.x to 0.x must carry it exactly ({report0:?})");
        let (out1, report1) = to1(&scene);
        let (doc1, _) = parts(&out1);
        let meta1 = &doc1["extensions"]["VRMC_vrm"]["meta"];
        let kept = carried.is_some_and(|check| check(meta1, &doc1));
        assert!(kept || mentions(&report1, key), "{key} = {probe} was dropped without a finding");
        if carried.is_none() {
            assert!(mentions(&report1, key), "{key} cannot be carried and must be reported");
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 3. A published export is gated by the model's own licence (spec VF-08)

fn publish() -> ExportOptions {
    options().with_purpose(Purpose::Publish)
}

fn try_export(vrm0: bool, scene: &IoScene, options: &ExportOptions) -> Result<(Vec<u8>, Report), IoError> {
    if vrm0 { export_with(&Vrm0Exporter, scene, options) } else { export_with(&Vrm1Exporter, scene, options) }
}

fn rejected(vrm0: bool, scene: &IoScene) -> Report {
    match try_export(vrm0, scene, &publish()) {
        Err(IoError::Rejected(report)) => report,
        other => panic!("expected a rejection, got {:?}", other.map(|(_, r)| r)),
    }
}

/// `extra` on top of a meta that allows commercial use, so only the file's own sharing terms are in question.
fn patch(extra: &Value) -> Value {
    let mut p = json!({"commercialUsage": "personalProfit"});
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    p
}

/// A one-member JSON object.
fn member(key: &str, value: &Value) -> Value {
    let mut o = json!({});
    o[key] = value.clone();
    o
}

#[test]
fn publishing_a_model_whose_licence_forbids_it_is_refused_with_the_field_value_and_creator() {
    for vrm0 in [false, true] {
        let scene = from1(&patch(&json!({"copyrightInformation": "(c) Studio Fox"})));
        let report = rejected(vrm0, &scene);
        let redistribution =
            report.findings.iter().find(|f| f.rule == "rights.licence_forbids_redistribution").unwrap();
        assert_eq!(redistribution.severity, Severity::Error);
        assert_eq!(redistribution.location.as_deref(), Some("meta/allowRedistribution"));
        let modification = report.findings.iter().find(|f| f.rule == "rights.licence_forbids_modification").unwrap();
        assert_eq!(modification.location.as_deref(), Some("meta/modification"));
        for (f, field, value) in
            [(redistribution, "allowRedistribution", "false"), (modification, "modification", "\"prohibited\"")]
        {
            let msg = &f.message;
            assert!(msg.contains(&format!("{field} is {value}")), "{msg}");
            assert!(
                msg.contains("\"Minimal\" by Someone (c) Studio Fox")
                    || msg.contains("\"Minimal\" by Someone ((c) Studio Fox)"),
                "{msg}"
            );
            assert!(f.hint.as_deref().is_some_and(|h| h.contains("permission")), "{:?}", f.hint);
        }
    }
}

#[test]
fn each_forbidding_term_is_refused_on_its_own_and_a_free_licence_publishes() {
    // (patch, rules that must fire)
    let rows: [(Value, &[&str]); 4] = [
        (json!({"allowRedistribution": true, "modification": "prohibited"}), &["rights.licence_forbids_modification"]),
        (
            json!({"allowRedistribution": true, "modification": "allowModification"}),
            &["rights.licence_forbids_modification"],
        ),
        (
            json!({"allowRedistribution": false, "modification": "allowModificationRedistribution"}),
            &["rights.licence_forbids_redistribution"],
        ),
        (json!({}), &["rights.licence_forbids_redistribution", "rights.licence_forbids_modification"]),
    ];
    for vrm0 in [false, true] {
        for (extra, rules) in &rows {
            let report = rejected(vrm0, &from1(&patch(extra)));
            let mut fired: Vec<&str> = report
                .findings
                .iter()
                .filter(|f| f.rule.starts_with("rights.licence_forbids"))
                .map(|f| f.rule.as_str())
                .collect();
            fired.sort_unstable();
            let mut want = rules.to_vec();
            want.sort_unstable();
            assert_eq!(fired, want, "{extra}");
        }
        let free =
            from1(&patch(&json!({"allowRedistribution": true, "modification": "allowModificationRedistribution"})));
        let (_, report) = try_export(vrm0, &free, &publish()).expect("a free licence may be published");
        assert!(!report.findings.iter().any(|f| f.rule.starts_with("rights.licence_forbids")));
    }
}

#[test]
fn a_private_export_of_a_restricted_model_is_allowed_with_a_finding() {
    for vrm0 in [false, true] {
        let scene = from1(&patch(&json!({})));
        let (_, report) = try_export(vrm0, &scene, &options()).expect("private export");
        let f = report.findings.iter().find(|f| f.rule == "rights.private_export_restricted").expect("a finding");
        assert_eq!(f.severity, Severity::Info);
        assert!(f.message.contains("allowRedistribution") && f.message.contains("private use only"), "{}", f.message);
        // A model that may be shared carries no such note.
        let free =
            from1(&patch(&json!({"allowRedistribution": true, "modification": "allowModificationRedistribution"})));
        let (_, report) = try_export(vrm0, &free, &options()).unwrap();
        assert!(!report.has_rule("rights.private_export_restricted"));
    }
}

#[test]
fn commercial_use_is_still_gated_by_the_store_gate() {
    // personalNonProfit is "no" for the licence gate, whatever the sharing flags say.
    for vrm0 in [false, true] {
        let scene = from1(&json!({"allowRedistribution": true, "modification": "allowModificationRedistribution"}));
        assert!(rejected(vrm0, &scene).has_rule("rights.gate_non_commercial"));
    }
}

#[test]
fn imported_0x_licences_are_gated_by_what_the_name_allows() {
    // (licenseName, may be published)
    let rows = [
        ("Redistribution_Prohibited", false),
        ("CC_BY_ND", false), // redistribution of the unmodified model only; an export may differ
        ("CC0", true),
        ("CC_BY", true),
        ("CC_BY_SA", true),
        ("CC_BY_NC", false), // non-commercial: the store gate
    ];
    for vrm0 in [false, true] {
        for (name, ok) in rows {
            let meta = json!({"title": "T", "author": "A", "licenseName": name, "commercialUssageName": "Allow"});
            let scene = from0(&meta);
            let result = try_export(vrm0, &scene, &publish());
            assert_eq!(result.is_ok(), ok, "{name} (vrm0 = {vrm0})");
        }
    }
}

#[test]
fn a_model_made_in_omuzaki_is_the_users_to_publish() {
    // Authored material has the user as licensor: restrictive terms on it are the user's own choice.
    let (mut scene, _) = vrm0_friendly(avatar());
    scene.meta.terms = omuzaki_io_api::UseTerms::default();
    for vrm0 in [false, true] {
        let (_, report) = try_export(vrm0, &scene, &publish()).expect("own work may be published");
        assert!(!report.findings.iter().any(|f| f.rule.starts_with("rights.licence_forbids")));
        assert!(!report.has_rule("rights.private_export_restricted"));
    }
}
