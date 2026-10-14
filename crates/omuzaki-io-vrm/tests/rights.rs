//! The rights rules, row by row: what an imported file's terms become in Omuzaki's provenance record (the tables in
//! `docs/design/io-api.md`, D5), and what an export is allowed to write. The expected values below are written from the
//! design tables, not read back from the code under test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::field_reassign_with_default, clippy::too_many_lines)]

mod common;

use omuzaki_io_api::{CommercialUsage, ExportOptions, IoError, Purpose, Severity};
use omuzaki_io_vrm::{Vrm0Exporter, Vrm0Importer, Vrm1Exporter, Vrm1Importer};
use omuzaki_store_api::{Commercial, Consent};
use serde_json::{Value, json};

use common::{avatar, glb, import, minimal0, minimal1, options, provenance};

/// A change to a provenance record.
type Change = Box<dyn Fn(&mut omuzaki_store_api::Provenance)>;

const VRM_URL: &str = "https://vrm.dev/licenses/1.0/";
const VRM_URL_EN: &str = "https://vrm.dev/licenses/1.0/en/";

fn with_meta(patch: &Value) -> Vec<u8> {
    let (mut doc, bin) = minimal1();
    let meta = doc["extensions"]["VRMC_vrm"]["meta"].as_object_mut().unwrap();
    for (k, v) in patch.as_object().unwrap() {
        meta.insert(k.clone(), v.clone());
    }
    glb(&doc, &bin)
}

// ---------------------------------------------------------------------------------------------------------------
// Import, VRM 1.0 (design doc D5, "Import: VRM 1.0 meta -> Provenance")

#[test]
fn vrm1_licence_and_commercial_status_rows() {
    // (licenseUrl, commercialUsage, expected licence, expected commercial, expected warning rule)
    let rows: [(&str, &str, &str, Commercial, Option<&str>); 6] = [
        (VRM_URL, "personalNonProfit", "LicenseRef-VRM-Public-License-1.0", Commercial::No, None),
        (VRM_URL, "personalProfit", "LicenseRef-VRM-Public-License-1.0", Commercial::Ok, None),
        (VRM_URL_EN, "corporation", "LicenseRef-VRM-Public-License-1.0", Commercial::Ok, None),
        (
            "https://example.org/terms",
            "personalProfit",
            "LicenseRef-VRM-Other",
            Commercial::Unknown,
            Some("rights.unrecognised_license_url"),
        ),
        (
            "https://example.org/terms",
            "personalNonProfit",
            "LicenseRef-VRM-Other",
            Commercial::Unknown,
            Some("rights.unrecognised_license_url"),
        ),
        ("", "corporation", "LicenseRef-Unknown", Commercial::Unknown, None),
    ];
    for (url, usage, licence, commercial, rule) in rows {
        let out = import(&Vrm1Importer, &with_meta(&json!({"licenseUrl": url, "commercialUsage": usage})));
        let p = out.scene.provenance.as_ref().unwrap();
        assert_eq!((p.licence.as_str(), p.commercial), (licence, commercial), "{url:?} / {usage}");
        match rule {
            Some(rule) => {
                assert!(
                    out.report.findings.iter().any(|f| f.rule == rule && f.severity == Severity::Warning),
                    "{rule}"
                );
            }
            None => assert!(!out.report.has_rule("rights.unrecognised_license_url"), "{url:?}"),
        }
        // Nothing in a VRM says the model's owner consented or that the material is private test material.
        assert_eq!((p.consent.is_none(), p.private_test), (true, false), "{url:?} / {usage}");
    }
}

#[test]
fn both_importers_report_their_licence_as_imported() {
    use omuzaki_io_api::LicenceEvidence;
    assert_eq!(import(&Vrm1Importer, &with_meta(&json!({}))).licence, LicenceEvidence::Imported);
    assert_eq!(
        import(&Vrm0Importer, &vrm0_with(Some("CC_BY_NC"), Some("Disallow"))).licence,
        LicenceEvidence::Imported
    );
    // Imported evidence leaves the terms to describe themselves: a 0.x file under CC BY-NC is neither "no licence" nor
    // "not imported".
    let out = import(&Vrm0Importer, &vrm0_with(Some("CC_BY_NC"), Some("Disallow")));
    assert!(!out.report.has_rule("rights.no_license_info") && !out.report.has_rule("rights.license_not_imported"));
}

#[test]
fn vrm1_author_and_attribution_rows() {
    let out = import(
        &Vrm1Importer,
        &with_meta(&json!({"authors": ["A", "B"], "name": "Model", "creditNotation": "required"})),
    );
    let p = out.scene.provenance.unwrap();
    assert_eq!(p.author.as_deref(), Some("A, B"), "authors joined with a comma");
    assert_eq!(p.attribution.as_deref(), Some("Model - A, B"), "no copyright holder: the authors");
    let out = import(
        &Vrm1Importer,
        &with_meta(
            &json!({"authors": ["A"], "name": "Model", "creditNotation": "required", "copyrightInformation": "ACME"}),
        ),
    );
    assert_eq!(out.scene.provenance.unwrap().attribution.as_deref(), Some("Model - ACME"), "the copyright holder wins");
    let out = import(&Vrm1Importer, &with_meta(&json!({"creditNotation": "unnecessary"})));
    assert_eq!(out.scene.provenance.unwrap().attribution, None);
    // The spec default for creditNotation is "required".
    let out = import(&Vrm1Importer, &with_meta(&json!({})));
    assert!(out.scene.provenance.unwrap().attribution.is_some());
}

#[test]
fn vrm1_restriction_notes() {
    let free = json!({"allowRedistribution": true, "modification": "allowModificationRedistribution"});
    assert!(!import(&Vrm1Importer, &with_meta(&free)).report.has_rule("rights.import_restricted"));
    let no_redistribution = json!({"allowRedistribution": false, "modification": "allowModificationRedistribution"});
    assert!(import(&Vrm1Importer, &with_meta(&no_redistribution)).report.has_rule("rights.import_restricted"));
    let no_modification = json!({"allowRedistribution": true, "modification": "prohibited"});
    assert!(import(&Vrm1Importer, &with_meta(&no_modification)).report.has_rule("rights.import_restricted"));
    let other = json!({"otherLicenseUrl": "https://example.org/more"});
    assert!(import(&Vrm1Importer, &with_meta(&other)).report.has_rule("rights.other_license"));
    assert!(import(&Vrm1Importer, &with_meta(&json!({"licenseUrl": ""}))).report.has_rule("rights.no_license_info"));
}

// ---------------------------------------------------------------------------------------------------------------
// Import, VRM 0.x (design doc D5, "Import: VRM 0.x meta -> Provenance")

fn vrm0_with(license: Option<&str>, commercial: Option<&str>) -> Vec<u8> {
    let mut meta = json!({"title": "Hand Made", "author": "Someone"});
    if let Some(l) = license {
        meta["licenseName"] = json!(l);
    }
    if let Some(c) = commercial {
        meta["commercialUssageName"] = json!(c);
    }
    let (mut doc, bin) = minimal0(&json!({}));
    doc["extensions"]["VRM"]["meta"] = meta;
    glb(&doc, &bin)
}

#[test]
fn vrm0_licence_name_rows() {
    // (licenseName, SPDX / LicenseRef, commercial when the avatar flag says Allow)
    let rows: [(&str, &str, Commercial); 9] = [
        ("Redistribution_Prohibited", "LicenseRef-VRM-Redistribution-Prohibited", Commercial::Ok),
        ("CC0", "CC0-1.0", Commercial::Ok),
        ("CC_BY", "CC-BY-4.0", Commercial::Ok),
        ("CC_BY_SA", "CC-BY-SA-4.0", Commercial::Ok),
        ("CC_BY_ND", "CC-BY-ND-4.0", Commercial::Ok),
        // Policy: nothing non-commercial, whatever the avatar flag says.
        ("CC_BY_NC", "CC-BY-NC-4.0", Commercial::No),
        ("CC_BY_NC_SA", "CC-BY-NC-SA-4.0", Commercial::No),
        ("CC_BY_NC_ND", "CC-BY-NC-ND-4.0", Commercial::No),
        // "Other" points at a document we can't evaluate.
        ("Other", "LicenseRef-VRM-Other", Commercial::Unknown),
    ];
    for (name, licence, allowed) in rows {
        let out = import(&Vrm0Importer, &vrm0_with(Some(name), Some("Allow")));
        let p = out.scene.provenance.as_ref().unwrap();
        assert_eq!((p.licence.as_str(), p.commercial), (licence, allowed), "{name} with Allow");
        // "Disallow" or a missing flag is No for every licence.
        for flag in [Some("Disallow"), None] {
            let out = import(&Vrm0Importer, &vrm0_with(Some(name), flag));
            assert_eq!(out.scene.provenance.unwrap().commercial, Commercial::No, "{name} with {flag:?}");
        }
    }
    // No licence name at all: unknown licence, unknown rights when commercial use is allowed.
    let out = import(&Vrm0Importer, &vrm0_with(None, Some("Allow")));
    let p = out.scene.provenance.unwrap();
    assert_eq!((p.licence.as_str(), p.commercial), ("LicenseRef-Unknown", Commercial::Unknown));
}

#[test]
fn vrm0_attribution_author_and_redistribution_rows() {
    // (licenseName, credit required, redistribution allowed, modification allowed) from the design table.
    let rows = [
        ("Redistribution_Prohibited", true, false, false),
        ("CC0", false, true, true),
        ("CC_BY", true, true, true),
        ("CC_BY_SA", true, true, true),
        ("CC_BY_ND", true, true, false),
        ("CC_BY_NC", true, true, true),
        ("CC_BY_NC_ND", true, true, false),
    ];
    for (name, credit, redistribution, modification) in rows {
        let out = import(&Vrm0Importer, &vrm0_with(Some(name), Some("Allow")));
        let s = &out.scene;
        assert_eq!(s.provenance.as_ref().unwrap().attribution.is_some(), credit, "{name} credit");
        assert_eq!(s.meta.terms.allow_redistribution, redistribution, "{name} redistribution");
        let free = s.meta.terms.modification != omuzaki_io_api::Modification::Prohibited;
        assert_eq!(free, modification, "{name} modification");
        assert_eq!(s.provenance.as_ref().unwrap().author.as_deref(), Some("Someone"));
    }
}

#[test]
fn vrm0_flags_map_to_the_one_zero_terms() {
    let (mut doc, bin) = minimal0(&json!({}));
    doc["extensions"]["VRM"]["meta"] = json!({
        "title": "T", "author": "A", "allowedUserName": "ExplicitlyLicensedPerson", "violentUssageName": "Allow",
        "sexualUssageName": "Disallow", "commercialUssageName": "Allow", "licenseName": "CC_BY",
        "otherPermissionUrl": "https://example.org/permissions",
    });
    let t = import(&Vrm0Importer, &glb(&doc, &bin)).scene.meta;
    assert_eq!(t.terms.avatar_permission, omuzaki_io_api::AvatarPermission::OnlySeparatelyLicensedPerson);
    assert!(t.terms.allow_excessively_violent_usage && !t.terms.allow_excessively_sexual_usage);
    assert_eq!(t.terms.commercial_usage, CommercialUsage::PersonalProfit, "0.x can't say corporation");
    assert!(!t.terms.allow_political_or_religious_usage && !t.terms.allow_antisocial_or_hate_usage);
    assert_eq!(t.other_license_url.as_deref(), Some("https://example.org/permissions"));
}

// ---------------------------------------------------------------------------------------------------------------
// Export (design doc D5, "Export: what the scene may write")

fn export_error(
    scene: &omuzaki_io_api::IoScene,
    options: &ExportOptions,
    vrm0: bool,
) -> Option<omuzaki_io_api::Report> {
    let result = if vrm0 {
        common::export_with(&Vrm0Exporter, scene, options)
    } else {
        common::export_with(&Vrm1Exporter, scene, options)
    };
    match result {
        Err(IoError::Rejected(report)) => Some(report),
        Err(other) => panic!("unexpected error {other:?}"),
        Ok(_) => None,
    }
}

#[test]
fn export_rights_rules_row_by_row() {
    for vrm0 in [false, true] {
        let publish = options().with_purpose(Purpose::Publish);
        let (clean, _) = common::vrm0_friendly(avatar());
        // The baseline: cleared material exports, privately and published.
        assert!(export_error(&clean, &options(), vrm0).is_none());
        assert!(export_error(&clean, &publish, vrm0).is_none());

        let mut s = clean.clone();
        s.provenance = None;
        assert!(export_error(&s, &options(), vrm0).unwrap().has_rule("rights.provenance_missing"));

        // The gate applies to published exports only, with its own rule per reason.
        let cases: [(&str, Change); 3] = [
            ("rights.gate_non_commercial", Box::new(|p| p.commercial = Commercial::No)),
            ("rights.gate_unknown", Box::new(|p| p.commercial = Commercial::Unknown)),
            ("rights.gate_private_test", Box::new(|p| p.private_test = true)),
        ];
        for (rule, change) in cases {
            let mut s = clean.clone();
            change(s.provenance.as_mut().unwrap());
            s.meta.terms = omuzaki_io_api::UseTerms::default(); // keep the terms within what the record allows
            assert!(export_error(&s, &publish, vrm0).unwrap().has_rule(rule), "{rule}");
            if rule != "rights.gate_private_test" {
                assert!(export_error(&s, &options(), vrm0).is_none(), "{rule}: a private export is not gated");
            }
        }
        let mut s = clean.clone();
        s.provenance.as_mut().unwrap().consent = None;
        let real_person = publish.clone().with_real_person();
        assert!(export_error(&s, &real_person, vrm0).unwrap().has_rule("rights.gate_no_consent"));
        s.provenance.as_mut().unwrap().consent =
            Some(Consent { person: "P".into(), scope: "this avatar".into(), document: None });
        assert!(export_error(&s, &real_person, vrm0).is_none());

        // The terms may not say more than the record holds.
        let mut s = clean.clone();
        s.provenance.as_mut().unwrap().commercial = Commercial::No;
        s.meta.terms.commercial_usage = CommercialUsage::PersonalProfit;
        assert!(export_error(&s, &options(), vrm0).unwrap().has_rule("rights.terms_exceed_provenance"));
        // Not cleared means no permission at all, not only no commercial use: the fixture's other open terms are refused too.
        s.meta.terms.commercial_usage = CommercialUsage::PersonalNonProfit;
        assert!(export_error(&s, &options(), vrm0).unwrap().has_rule("rights.terms_exceed_provenance"));
        s.meta.terms = omuzaki_io_api::UseTerms::default();
        assert!(export_error(&s, &options(), vrm0).is_none(), "the most restrictive terms are always allowed");

        // Private test material exports only privately, with the most restrictive terms, and says so.
        let mut s = clean.clone();
        s.provenance.as_mut().unwrap().private_test = true;
        assert!(export_error(&s, &options(), vrm0).unwrap().has_rule("rights.terms_exceed_provenance"));
        s.meta.terms = omuzaki_io_api::UseTerms::default();
        s.meta.other_license_url = None;
        let (_, report) = if vrm0 {
            common::export_with(&Vrm0Exporter, &s, &options()).unwrap()
        } else {
            common::export_with(&Vrm1Exporter, &s, &options()).unwrap()
        };
        assert!(report.has_rule("rights.private_test_export"));
    }
    let _ = provenance();
}
