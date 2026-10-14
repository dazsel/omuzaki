//! The registered `glb` format: what makes plain glTF different from the library underneath it. Reading and writing
//! themselves are tested in `omuzaki-gltf` (round trips, hostile input, every accessor shape); this checks the
//! registration and the policy `omuzaki-io-gltf` adds: VRM data is not carried, and export says what plain glTF can't hold.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_io_api::{
    Confidence, ExportCtx, ExportOptions, ImportCtx, ImportInput, ImportOptions, IoError, IoScene, LicenceEvidence,
    Material, PerfectSync, Purpose, Severity, Shading, VRM_LICENSE_URLS, VrmMeta, find_exporter, find_importer,
    formats, run_export, run_import, sniff,
};
use omuzaki_io_gltf::{GltfExporter, GltfImporter};
use omuzaki_io_mock::MockVrm1Exporter;
use omuzaki_io_mock::fixtures::{
    authored_provenance, glb_from_json, options_without_face_channels, tiny_avatar, tiny_glb,
};
use serde_json::json;

fn import(bytes: &[u8]) -> omuzaki_io_api::ImportOutcome {
    let options = ImportOptions::default();
    run_import(&GltfImporter, &ImportInput::new(bytes), &ImportCtx::new(&options)).expect("import")
}

/// A one-triangle plain scene with every kind of VRM data the policy cares about added to the JSON.
fn file_with_vrm_data() -> Vec<u8> {
    let positions: Vec<u8> =
        [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let doc = json!({
        "asset": { "version": "2.0" },
        "extensionsUsed": ["VRM", "VRMC_vrm", "VRMC_node_constraint", "VRMC_materials_mtoon", "EXT_studio_tag"],
        "extensions": {
            "VRM": { "meta": { "title": "x", "licenseName": "Other" } },
            "VRMC_vrm": { "specVersion": "1.0" },
            "EXT_studio_tag": { "shot": 7 }
        },
        "scene": 0, "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0, "extensions": { "VRMC_node_constraint": { "specVersion": "1.0" } } }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "materials": [{ "extensions": { "VRMC_materials_mtoon": { "specVersion": "1.0" } } }],
        "accessors": [{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0, 0, 0], "max": [1, 1, 0] }],
        "bufferViews": [{ "buffer": 0, "byteLength": 36 }],
        "buffers": [{ "byteLength": 36 }]
    });
    glb_from_json(&doc, &positions)
}

#[test]
fn the_format_is_registered_as_glb_for_both_directions() {
    let glb = tiny_glb();
    let found: Vec<_> = sniff(&ImportInput::new(&glb)).into_iter().map(|d| (d.format, d.confidence)).collect();
    assert!(found.contains(&("glb", Confidence::Possible)));
    let text = br#"{"asset":{"version":"2.0"}}"#;
    assert_eq!(omuzaki_io_api::Importer::sniff(&GltfImporter, &ImportInput::new(text)), Confidence::Possible);
    for junk in [&b"not a model"[..], b"{}", b"", b"glT"] {
        assert_eq!(omuzaki_io_api::Importer::sniff(&GltfImporter, &ImportInput::new(junk)), Confidence::No);
    }
    let all = formats();
    let glb_format = all.iter().find(|f| f.info.id == "glb").expect("registered");
    assert!(glb_format.import && glb_format.export);
    assert_eq!(glb_format.info.extensions, ["glb", "gltf"]);
    assert!(glb_format.info.id.parse::<omuzaki_io_api::FormatId>().is_ok());
    assert!(find_importer("glb").is_some() && find_exporter("glb").is_some());
    // The mock's VRM file is a GLB too: ranked below the VRM importer, above nothing else.
    let options = options_without_face_channels();
    let vrm = run_export(&MockVrm1Exporter, &tiny_avatar(), &ExportCtx::new(&options)).unwrap();
    let ranked: Vec<_> = sniff(&ImportInput::new(&vrm.files[0].bytes)).into_iter().map(|d| d.format).collect();
    assert_eq!(ranked.first(), Some(&"vrm1"));
    assert!(ranked.contains(&"glb"));
}

#[test]
fn vrm_data_is_dropped_with_a_warning_that_names_the_fix() {
    let out = import(&file_with_vrm_data());
    let s = &out.scene;
    assert_eq!(s.passthrough.extensions.keys().collect::<Vec<_>>(), ["EXT_studio_tag"], "VRM 0.x and 1.0 data both go");
    assert!(s.nodes[0].passthrough.extensions.is_empty());
    assert!(s.materials[0].passthrough.extensions.is_empty());
    let dropped: Vec<_> = out.report.findings.iter().filter(|f| f.rule == "gltf.extension_dropped").collect();
    let names: Vec<_> = ["VRM", "VRMC_vrm", "VRMC_node_constraint", "VRMC_materials_mtoon"]
        .into_iter()
        .filter(|name| dropped.iter().any(|f| f.message.contains(&format!("extension {name} was dropped"))))
        .collect();
    assert_eq!(names.len(), 4, "{dropped:?}");
    for f in dropped {
        assert_eq!(f.severity, Severity::Warning);
        // A 0.x file is imported as `vrm0`; telling its owner `vrm1` would send them to the wrong importer.
        let (right, wrong) = if f.message.contains("extension VRM was") { ("vrm0", "vrm1") } else { ("vrm1", "vrm0") };
        assert!(f.hint.as_deref().is_some_and(|h| h.contains(right) && !h.contains(wrong)), "{f:?}");
        assert!(f.message.contains("belongs to the VRM importer"));
    }
    // The licence terms that rode in the dropped extension must not reach a plain export.
    let options = options_without_face_channels();
    let exported = run_export(&GltfExporter, &import(&file_with_vrm_data()).scene, &ExportCtx::new(&options));
    let text = String::from_utf8_lossy(&exported.expect("export").files[0].bytes).into_owned();
    assert!(!text.contains("licenseName") && !text.contains("VRMC_vrm"));
}

/// The VRM-bearing file with its root extensions replaced by `root`.
fn file_with_root_extensions(root: &serde_json::Value) -> Vec<u8> {
    let positions: Vec<u8> =
        [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let doc = json!({
        "asset": { "version": "2.0" },
        "extensions": root,
        "scene": 0, "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "accessors": [{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0, 0, 0], "max": [1, 1, 0] }],
        "bufferViews": [{ "buffer": 0, "byteLength": 36 }],
        "buffers": [{ "byteLength": 36 }]
    });
    glb_from_json(&doc, &positions)
}

/// The finding for the dropped root extension `name`.
fn dropped_root(out: &omuzaki_io_api::ImportOutcome, name: &str) -> omuzaki_io_api::Finding {
    let marker = format!("scene extension {name} was dropped");
    out.report.findings.iter().find(|f| f.message.contains(&marker)).cloned().expect("a drop warning")
}

#[test]
fn a_vrm0_file_imported_as_plain_glb_says_its_licence_terms_were_left_behind() {
    let out = import(&file_with_root_extensions(&json!({ "VRM": { "meta": {
        "title": "Test Avatar", "author": "someone",
        "licenseName": "CC_BY_NC", "allowedUserName": "OnlyAuthor", "violentUssageName": "Disallow",
        "sexualUssageName": "Disallow", "commercialUssageName": "Disallow",
        "otherPermissionUrl": "https://example.com/terms", "otherLicenseUrl": ""
    } } })));
    let f = dropped_root(&out, "VRM");
    assert_eq!(f.severity, Severity::Warning);
    for quoted in [
        "licence data present but not imported (VRM 0.x avatar)",
        "licenseName=CC_BY_NC",
        "allowedUserName=OnlyAuthor",
        "violentUssageName=Disallow",
        "sexualUssageName=Disallow",
        "commercialUssageName=Disallow",
        "otherPermissionUrl=https://example.com/terms",
    ] {
        assert!(f.message.contains(quoted), "{quoted:?} in {:?}", f.message);
    }
    assert!(!f.message.contains("otherLicenseUrl"), "an empty field isn't quoted: {:?}", f.message);
    assert!(!f.message.contains("someone"), "only the licence fields are quoted, not the author");
    assert!(f.hint.as_deref().is_some_and(|h| h.contains("vrm0") && !h.contains("vrm1")), "{f:?}");
    assert_eq!(f.location.as_deref(), Some("/extensions/VRM"));
}

#[test]
fn a_vrm1_file_imported_as_plain_glb_quotes_its_licence_and_names_vrm1() {
    let out = import(&file_with_root_extensions(&json!({ "VRMC_vrm": { "specVersion": "1.0", "meta": {
        "name": "x", "authors": ["a"], "licenseUrl": "https://vrm.dev/licenses/1.0/",
        "commercialUsage": "personalNonProfit", "creditNotation": "required", "allowRedistribution": false,
        "modification": "prohibited"
    } } })));
    let f = dropped_root(&out, "VRMC_vrm");
    for quoted in [
        "licence data present but not imported (VRM 1.0 avatar)",
        "licenseUrl=https://vrm.dev/licenses/1.0/",
        "commercialUsage=personalNonProfit",
        "creditNotation=required",
        "allowRedistribution=false",
        "modification=prohibited",
    ] {
        assert!(f.message.contains(quoted), "{quoted:?} in {:?}", f.message);
    }
    assert!(f.hint.as_deref().is_some_and(|h| h.contains("vrm1") && !h.contains("vrm0")), "{f:?}");
}

#[test]
fn quoted_licence_values_are_cut_short_and_cannot_carry_control_characters() {
    let long = "x".repeat(500);
    let out = import(&file_with_root_extensions(&json!({ "VRM": { "meta": {
        "licenseName": long, "otherPermissionUrl": "https://a.example/\u{1b}[31m\nred", "allowedUserName": 7
    } } })));
    let message = dropped_root(&out, "VRM").message;
    assert!(message.contains(&format!("licenseName={}...", "x".repeat(80))), "{message}");
    assert!(!message.contains(&"x".repeat(81)));
    assert!(message.len() < 500, "{} bytes", message.len());
    assert!(!message.chars().any(char::is_control), "{message:?}");
    assert!(message.contains("allowedUserName=7"), "numbers are quoted too: {message}");
}

#[test]
fn a_vrm_extension_without_licence_fields_says_so_instead_of_inventing_terms() {
    for (name, version) in [("VRM", "VRM 0.x"), ("VRMC_vrm", "VRM 1.0")] {
        let out = import(&file_with_root_extensions(&json!({ name: { "meta": { "title": "t" } } })));
        let f = dropped_root(&out, name);
        assert!(f.message.contains(&format!("it holds no licence fields ({version} avatar)")), "{f:?}");
        assert!(!f.message.contains("licence data present"));
        let bare = import(&file_with_root_extensions(&json!({ name: {} })));
        assert!(dropped_root(&bare, name).message.contains("no licence fields"));
    }
}

/// The finding with `rule`, which the import must have produced exactly once.
fn rights_finding(out: &omuzaki_io_api::ImportOutcome, rule: &str) -> omuzaki_io_api::Finding {
    let all: Vec<_> = out.report.findings.iter().filter(|f| f.rule == rule).collect();
    assert_eq!(all.len(), 1, "{rule}: {:?}", out.report);
    all[0].clone()
}

#[test]
fn a_vrm0_file_with_a_non_commercial_licence_is_never_reported_as_having_no_licence() {
    // The bug this guards: the plain importer dropped the VRM extension, so the report said "no licence
    // information" about a file whose licence is CC BY-NC.
    let out = import(&file_with_root_extensions(&json!({ "VRM": { "meta": {
        "title": "T", "author": "someone", "licenseName": "CC_BY_NC", "commercialUssageName": "Disallow"
    } } })));
    assert!(!out.report.has_rule("rights.no_license_info"), "{:?}", out.report);
    let f = rights_finding(&out, "rights.license_not_imported");
    assert_eq!(f.severity, Severity::Warning);
    for quoted in ["licence data present but not imported", "licenseName=CC_BY_NC", "commercialUssageName=Disallow"] {
        assert!(f.message.contains(quoted), "{quoted:?} in {:?}", f.message);
    }
    assert_eq!(f.hint.as_deref(), Some("import as vrm0 to keep it"));
    assert!(
        matches!(&out.licence, LicenceEvidence::PresentNotImported { hint_format: "vrm0", .. }),
        "{:?}",
        out.licence
    );
    // The record stays "unknown": the importer read none of the terms.
    assert_eq!(out.scene.provenance.as_ref().map(|p| p.licence.as_str()), Some("LicenseRef-Unknown"));
}

#[test]
fn a_vrm1_file_names_vrm1_and_a_file_with_both_extensions_names_both_importers() {
    let one =
        json!({ "VRMC_vrm": { "specVersion": "1.0", "meta": { "licenseUrl": "https://vrm.dev/licenses/1.0/" } } });
    let out = import(&file_with_root_extensions(&one));
    assert_eq!(rights_finding(&out, "rights.license_not_imported").hint.as_deref(), Some("import as vrm1 to keep it"));
    assert!(!out.report.has_rule("rights.no_license_info"));

    let both = json!({ "VRM": { "meta": { "licenseName": "CC_BY_NC" } }, "VRMC_vrm": { "meta": { "licenseUrl": "https://example.org/terms" } } });
    let out = import(&file_with_root_extensions(&both));
    let f = rights_finding(&out, "rights.license_not_imported");
    for quoted in ["VRM 0.x avatar: licenseName=CC_BY_NC", "VRM 1.0 avatar: licenseUrl=https://example.org/terms"] {
        assert!(f.message.contains(quoted), "{quoted:?} in {:?}", f.message);
    }
    assert_eq!(f.hint.as_deref(), Some("import as vrm0 or vrm1 to keep it"));
}

#[test]
fn licence_data_in_a_malformed_or_less_common_field_is_still_licence_data() {
    // An array or an object where a string belongs: quoted as written, named by its field, never "no licence".
    let malformed = [
        json!({ "VRM": { "meta": { "licenseName": ["CC_BY_NC"] } } }),
        json!({ "VRM": { "meta": { "licenseName": { "name": "CC_BY_NC" } } } }),
        json!({ "VRMC_vrm": { "meta": { "licenseUrl": ["https://example.org/terms"] } } }),
    ];
    for root in malformed {
        let out = import(&file_with_root_extensions(&root));
        assert!(!out.report.has_rule("rights.no_license_info"), "{root}: {:?}", out.report);
        let f = rights_finding(&out, "rights.license_not_imported");
        let field = if root.get("VRM").is_some() { "licenseName" } else { "licenseUrl" };
        assert!(
            f.message.contains(&format!("{field}=[")) || f.message.contains(&format!("{field}={{")),
            "{}",
            f.message
        );
    }
    // A 1.0 meta whose only licence data is a content flag or the third-party licences.
    for field in [
        "allowExcessivelyViolentUsage",
        "allowExcessivelySexualUsage",
        "allowPoliticalOrReligiousUsage",
        "allowAntisocialOrHateUsage",
    ] {
        let out = import(&file_with_root_extensions(&json!({ "VRMC_vrm": { "meta": { field: true } } })));
        assert!(!out.report.has_rule("rights.no_license_info"), "{field}");
        assert!(
            rights_finding(&out, "rights.license_not_imported").message.contains(&format!("{field}=true")),
            "{field}"
        );
    }
    let out =
        import(&file_with_root_extensions(&json!({ "VRMC_vrm": { "meta": { "thirdPartyLicenses": "Hair: CC-BY" } } })));
    assert!(rights_finding(&out, "rights.license_not_imported").message.contains("thirdPartyLicenses=Hair: CC-BY"));
    // Nothing in the field is nothing: null, blank text, empty containers.
    for empty in [json!(null), json!(""), json!("  "), json!([]), json!({})] {
        let out = import(&file_with_root_extensions(&json!({ "VRM": { "meta": { "licenseName": empty } } })));
        assert_eq!(out.licence, LicenceEvidence::Absent, "{empty}");
    }
    // A quoted container is cut short and cannot carry control characters either.
    let long = json!({ "VRM": { "meta": { "licenseName": ["x".repeat(500), "\u{1b}[31mred"] } } });
    let message = rights_finding(&import(&file_with_root_extensions(&long)), "rights.license_not_imported").message;
    assert!(message.len() < 400 && !message.chars().any(char::is_control), "{message:?}");
}

#[test]
fn the_plain_exporter_refuses_to_publish_a_model_whose_licence_forbids_sharing_it() {
    // An imported model, cleared for commercial use, whose creator did not allow redistribution (the gate is
    // in `preflight`, so plain glTF gets it too).
    let mut meta = VrmMeta::new("Minimal", vec!["Someone".into()], VRM_LICENSE_URLS[0]);
    meta.terms.commercial_usage = omuzaki_io_api::CommercialUsage::PersonalProfit;
    let mut scene = IoScene::new();
    scene.provenance = Some(meta.imported_provenance(Some("minimal.vrm")));
    scene.meta = meta;
    let publish = ExportOptions::default().with_purpose(Purpose::Publish).with_perfect_sync(PerfectSync::Off);
    let Err(IoError::Rejected(report)) = run_export(&GltfExporter, &scene, &ExportCtx::new(&publish)) else {
        panic!("a published export of a model that forbids redistribution must be refused")
    };
    let f = report.errors().find(|f| f.rule == "rights.licence_forbids_redistribution").expect("the rule");
    assert_eq!(f.location.as_deref(), Some("meta/allowRedistribution"));
    assert!(f.message.contains("allowRedistribution is false") && f.message.contains("\"Minimal\" by Someone"));
    // The same model exports privately, with a note that the file must not be shared.
    let private = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let out = run_export(&GltfExporter, &scene, &ExportCtx::new(&private)).expect("a private export is allowed");
    assert!(out.report.has_rule("rights.private_export_restricted"));
}

#[test]
fn only_a_file_with_no_licence_data_at_all_is_reported_as_having_none() {
    let plain = import(&tiny_glb());
    assert_eq!(plain.licence, LicenceEvidence::Absent);
    assert!(plain.report.has_rule("rights.no_license_info") && !plain.report.has_rule("rights.license_not_imported"));
    // A VRM extension with no licence fields has nothing to leave behind.
    for name in ["VRM", "VRMC_vrm"] {
        let out = import(&file_with_root_extensions(&json!({ name: { "meta": { "title": "t" } } })));
        assert_eq!(out.licence, LicenceEvidence::Absent, "{name}");
        assert!(out.report.has_rule("rights.no_license_info") && !out.report.has_rule("rights.license_not_imported"));
    }
}

#[test]
fn what_the_mock_writes_as_vrm_reads_back_as_plain_glb_without_the_avatar() {
    let original = tiny_avatar();
    let options = options_without_face_channels();
    let mock = run_export(&MockVrm1Exporter, &original, &ExportCtx::new(&options)).unwrap();
    let out = import(&mock.files[0].bytes);
    assert_eq!(out.scene.nodes, original.nodes);
    assert_eq!(out.scene.meshes, original.meshes);
    assert_eq!(out.scene.skins, original.skins);
    assert_eq!(out.scene.materials, original.materials);
    assert!(out.scene.avatar.is_none(), "a plain importer reads no avatar");
    assert!(out.report.has_rule("gltf.extension_dropped"), "and says what it left out");
}

#[test]
fn what_a_gltf_cannot_carry_is_reported_on_export() {
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let outcome = run_export(&GltfExporter, &tiny_avatar(), &ExportCtx::new(&options)).expect("export");
    assert!(outcome.report.has_rule("gltf.avatar_dropped"));

    let mut scene = IoScene::new();
    scene.provenance = Some(authored_provenance());
    let ix = scene.add_material(Material::new("toon", Shading::Toon(Box::default())));
    assert_eq!(ix.index(), 0);
    let report =
        run_export(&GltfExporter, &scene, &ExportCtx::new(&options_without_face_channels())).expect("export").report;
    let toon = report.findings.iter().find(|f| f.rule == "gltf.toon_dropped").expect("a toon note");
    assert_eq!(toon.severity, Severity::Warning);
    assert_eq!(toon.location.as_deref(), Some("materials/0"));
    assert!(toon.hint.as_deref().is_some_and(|h| h.contains("vrm1")));
}
