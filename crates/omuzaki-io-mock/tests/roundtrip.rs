//! Import and export end to end through the contract's entry points: a hand-built GLB in, a VRM out, bytes stable,
//! limits and malformed files handled, registration and sniffing working.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, Ordering};

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    Confidence, ExportCtx, FormatId, ImportCtx, ImportInput, ImportOptions, ImportOutcome, Importer, IoError, IoScene,
    LicenceEvidence, Limits, NodeIx, Progress, Report, Severity, VRM_LICENSE_REF, find_exporter, find_importer,
    formats, run_export, run_import, sniff,
};
use omuzaki_io_mock::fixtures::{self, glb_from_json, glb_parts, options_without_face_channels, tiny_avatar, tiny_glb};
use omuzaki_io_mock::{MockGlbExporter, MockGlbImporter, MockVrm1Exporter, MockVrm1Importer};
use omuzaki_store_api::Commercial;
use serde_json::json;

fn import_with(importer: &dyn Importer, bytes: &[u8], options: &ImportOptions) -> Result<ImportOutcome, IoError> {
    run_import(importer, &ImportInput::new(bytes).named("test.glb"), &ImportCtx::new(options))
}

fn import(importer: &dyn Importer, bytes: &[u8]) -> ImportOutcome {
    import_with(importer, bytes, &ImportOptions::default()).expect("import")
}

fn export_bytes(exporter: &dyn omuzaki_io_api::Exporter, scene: &IoScene) -> Vec<u8> {
    let options = options_without_face_channels();
    let mut out = run_export(exporter, scene, &ExportCtx::new(&options)).expect("export");
    assert_eq!(out.files.len(), 1);
    out.files.remove(0).bytes
}

#[test]
fn imports_a_hand_built_glb() {
    let out = import(&MockGlbImporter, &tiny_glb());
    let s = &out.scene;
    assert_eq!((s.nodes.len(), s.roots.as_slice()), (1, &[NodeIx(0)][..]));
    assert_eq!(s.nodes[0].transform.translation, Vec3::Y);
    let prim = &s.meshes[0].primitives[0];
    assert_eq!(prim.mesh.positions[1], Vec3::X);
    assert_eq!(prim.mesh.indices, [0, 1, 2], "u16 indices widen to u32");
    // The file has no normals; the shared mesh type requires them, so the importer makes flat ones and says so.
    assert_eq!(prim.mesh.normals, [Vec3::Z; 3]);
    assert!(out.report.has_rule("gltf.normals_generated"));
    assert_eq!(s.materials[0].base_color.to_array(), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(s.meta.copyright_information.as_deref(), Some("Test Author"));
    assert_eq!(s.source_generator.as_deref(), Some("omuzaki-io-mock fixture"));
    // A plain glTF says nothing about rights, so the record is "unknown", which the gate treats as no.
    let p = s.provenance.as_ref().expect("run_import fills the provenance");
    assert_eq!(p.commercial, Commercial::Unknown);
    assert!(out.report.has_rule("rights.no_license_info"));
    assert_eq!(out.report.count(Severity::Error), 0);
}

#[test]
fn glb_round_trips() {
    let first = import(&MockGlbImporter, &tiny_glb()).scene;
    let again = import(&MockGlbImporter, &export_bytes(&MockGlbExporter, &first)).scene;
    let mut expected = first;
    expected.source_generator = None; // the exporter writes its own generator, not the source's
    assert_eq!(again, expected);
}

#[test]
fn vrm_round_trips_and_derives_a_conservative_provenance() {
    let original = tiny_avatar();
    let file = export_bytes(&MockVrm1Exporter, &original);
    let out = import(&MockVrm1Importer, &file);
    let back = out.scene;
    assert_eq!(back.nodes, original.nodes);
    assert_eq!(back.meshes, original.meshes);
    assert_eq!(back.skins, original.skins);
    assert_eq!(back.materials, original.materials);
    assert_eq!(back.roots, original.roots);
    assert_eq!(back.avatar, original.avatar);
    assert_eq!(back.meta, original.meta);
    // The file carries the VRM terms, not our record, so the record is re-derived from them.
    let p = back.provenance.expect("provenance");
    assert_eq!((p.licence.as_str(), p.commercial), (VRM_LICENSE_REF, Commercial::Ok));
    assert_eq!(p.author.as_deref(), Some("Test Author"));
    assert!(out.report.has_rule("rights.import_restricted"), "redistribution is off in the fixture's terms");
}

#[test]
fn the_importers_say_what_they_found_about_the_licence() {
    let vrm = export_bytes(&MockVrm1Exporter, &tiny_avatar());
    assert_eq!(import(&MockVrm1Importer, &vrm).licence, LicenceEvidence::Imported);
    // A plain glTF with no licence data has none to report; the same VRM read as plain glTF left its licence behind,
    // and must not be reported as having none.
    let plain = import(&MockGlbImporter, &tiny_glb());
    assert_eq!(plain.licence, LicenceEvidence::Absent);
    let left_behind = import(&MockGlbImporter, &vrm);
    assert!(matches!(&left_behind.licence, LicenceEvidence::PresentNotImported { hint_format: "vrm1", .. }));
    assert!(left_behind.report.has_rule("rights.license_not_imported"));
    assert!(!left_behind.report.has_rule("rights.no_license_info"));
}

#[test]
fn exports_are_deterministic_and_canonical() {
    let scene = tiny_avatar();
    let a = export_bytes(&MockVrm1Exporter, &scene);
    assert_eq!(a, export_bytes(&MockVrm1Exporter, &scene), "same scene, same bytes");
    let reimported = import(&MockVrm1Importer, &a).scene;
    assert_eq!(a, export_bytes(&MockVrm1Exporter, &reimported), "export(import(export(x))) == export(x)");
}

#[test]
fn the_vrm_file_has_the_spec_shape() {
    let file = export_bytes(&MockVrm1Exporter, &tiny_avatar());
    // The GLB header, checked by hand rather than through the mock's own reader.
    assert_eq!(&file[0..4], b"glTF");
    assert_eq!(u32::from_le_bytes(file[4..8].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(file[8..12].try_into().unwrap()) as usize, file.len());
    assert_eq!(file.len() % 4, 0);
    let (doc, bin) = glb_parts(&file).expect("parts");
    assert_ne!(bin, [] as [u8; 0]);
    assert_eq!(doc["asset"]["version"], "2.0");
    assert_eq!(doc["extensionsUsed"], json!(["VRMC_vrm"]));
    let vrm = &doc["extensions"]["VRMC_vrm"];
    assert_eq!(vrm["specVersion"], "1.0");
    assert_eq!(vrm["meta"]["licenseUrl"], "https://vrm.dev/licenses/1.0/");
    assert_eq!(vrm["meta"]["commercialUsage"], "personalProfit");
    assert_eq!(vrm["meta"]["allowRedistribution"], false, "defaults are written explicitly");
    assert_eq!(vrm["humanoid"]["humanBones"].as_object().unwrap().len(), 15);
    assert_eq!(vrm["expressions"]["preset"]["happy"]["overrideBlink"], "block");
    assert_eq!(vrm["expressions"]["preset"]["happy"]["morphTargetBinds"][0]["weight"], 0.5);
    assert_eq!(vrm["expressions"]["custom"]["jawOpen"]["morphTargetBinds"][0]["index"], 0);
    assert_eq!(doc["meshes"][0]["extras"]["targetNames"], json!(["jawOpen"]));
    // glTF 2.0: POSITION accessors, including morph target ones, must carry min and max.
    let primitive = &doc["meshes"][0]["primitives"][0];
    let position = usize::try_from(primitive["attributes"]["POSITION"].as_u64().unwrap()).unwrap();
    let target_position = usize::try_from(primitive["targets"][0]["POSITION"].as_u64().unwrap()).unwrap();
    for ix in [position, target_position] {
        assert!(doc["accessors"][ix]["min"].is_array() && doc["accessors"][ix]["max"].is_array(), "accessor {ix}");
    }
}

#[test]
fn sniffing_ranks_the_specific_format_first() {
    let vrm = export_bytes(&MockVrm1Exporter, &tiny_avatar());
    let ranked: Vec<_> = sniff(&ImportInput::new(&vrm)).into_iter().map(|d| (d.format, d.confidence)).collect();
    assert_eq!(ranked, [("vrm1", Confidence::Certain), ("glb", Confidence::Possible)]);
    let plain: Vec<_> = sniff(&ImportInput::new(&tiny_glb())).into_iter().map(|d| d.format).collect();
    assert_eq!(plain, ["glb"]);
    assert_eq!(sniff(&ImportInput::new(b"not a model")), [] as [omuzaki_io_api::Detection; 0]);
}

#[test]
fn the_registry_lists_every_format_once_with_valid_ids() {
    let all = formats();
    let ids: Vec<_> = all.iter().map(|f| f.info.id).collect();
    assert_eq!(ids, ["glb", "vrm1"], "sorted by id, each once");
    assert!(all.iter().all(|f| f.import && f.export));
    for f in &all {
        assert!(f.info.id.parse::<FormatId>().is_ok(), "{} must be a valid slug", f.info.id);
        assert!(!f.info.extensions.is_empty() && !f.info.media_type.is_empty());
    }
    assert!(find_importer("vrm1").is_some() && find_exporter("glb").is_some());
    assert!(find_importer("fbx").is_none());
}

#[test]
fn limits_are_enforced_before_and_after_import() {
    let file = tiny_glb();
    let too_big = ImportOptions::default().with_limits(Limits { max_input_bytes: 10, ..Limits::default() });
    assert!(matches!(
        import_with(&MockGlbImporter, &file, &too_big),
        Err(IoError::Limit { what: "input size in bytes", .. })
    ));
    let few_vertices = ImportOptions::default().with_limits(Limits { max_vertices: 2, ..Limits::default() });
    assert!(
        matches!(import_with(&MockGlbImporter, &file, &few_vertices), Err(IoError::Limit { .. })),
        "the importer checks accessor counts first"
    );
    let few_nodes = ImportOptions::default().with_limits(Limits { max_nodes: 0, ..Limits::default() });
    assert!(matches!(import_with(&MockGlbImporter, &file, &few_nodes), Err(IoError::Limit { what: "node count", .. })));
}

#[test]
fn malformed_and_unsupported_files_fail_with_the_right_error() {
    let (doc, bin) = glb_parts(&tiny_glb()).unwrap();
    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut d = doc.clone();
        edit(&mut d);
        import_with(&MockGlbImporter, &glb_from_json(&d, &bin), &ImportOptions::default())
    };
    assert!(matches!(
        import_with(&MockGlbImporter, b"glTF", &ImportOptions::default()),
        Err(IoError::Malformed { .. })
    ));
    assert!(matches!(
        with(&|d| d["extensionsRequired"] = json!(["KHR_draco_mesh_compression"])),
        Err(IoError::Unsupported { .. })
    ));
    assert!(
        matches!(with(&|d| d["accessors"][0]["count"] = json!(1_000)), Err(IoError::Malformed { .. })),
        "data past the buffer"
    );
    assert!(matches!(with(&|d| d["accessors"][0]["sparse"] = json!({})), Err(IoError::Unsupported { .. })));
    assert!(matches!(with(&|d| d["nodes"][0]["matrix"] = json!(vec![0.0; 16])), Err(IoError::Unsupported { .. })));
    assert!(matches!(with(&|d| d["meshes"][0]["primitives"][0]["mode"] = json!(1)), Err(IoError::Unsupported { .. })));
    // A dangling index in the file becomes a scene-invariant rejection with a rule id and a location.
    let Err(IoError::Rejected(report)) = with(&|d| d["scenes"][0]["nodes"] = json!([7])) else {
        panic!("expected a rejection")
    };
    assert!(report.has_rule("scene.index_out_of_range"));
    // Unknown optional extensions are dropped, with a note.
    let ok = with(&|d| d["extensionsUsed"] = json!(["KHR_materials_sheen"])).expect("optional extensions are fine");
    assert!(ok.report.has_rule("gltf.extension_dropped"));
}

#[test]
fn unindexed_primitives_get_sequential_indices() {
    let (mut doc, bin) = glb_parts(&tiny_glb()).unwrap();
    doc["meshes"][0]["primitives"][0].as_object_mut().unwrap().remove("indices");
    let out = import_with(&MockGlbImporter, &glb_from_json(&doc, &bin), &ImportOptions::default()).expect("import");
    assert_eq!(out.scene.meshes[0].primitives[0].mesh.indices, [0, 1, 2]);
}

#[test]
fn unknown_extensions_and_extras_pass_through_a_round_trip() {
    let (mut doc, bin) = glb_parts(&tiny_glb()).unwrap();
    doc["extensionsUsed"] = json!(["EXT_studio_tag", "KHR_materials_sheen", "EXT_on_a_mesh"]);
    doc["extensions"] = json!({ "EXT_studio_tag": { "shot": 7, "refs": [0] } });
    doc["extras"] = json!({ "pipeline": "v2" });
    doc["nodes"][0]["extras"] = json!({ "rig": "hand" });
    doc["materials"][0]["extensions"] = json!({ "KHR_materials_sheen": { "sheenRoughnessFactor": 0.8 } });
    doc["meshes"][0]["extensions"] = json!({ "EXT_on_a_mesh": {} }); // a place the scene has no bag for
    let first = import(&MockGlbImporter, &glb_from_json(&doc, &bin));
    let s = &first.scene;
    assert_eq!(s.passthrough.extensions["EXT_studio_tag"], json!({ "shot": 7, "refs": [0] }));
    assert_eq!(s.passthrough.extras, Some(json!({ "pipeline": "v2" })));
    assert_eq!(s.nodes[0].passthrough.extras, Some(json!({ "rig": "hand" })));
    assert!(s.materials[0].passthrough.extensions.contains_key("KHR_materials_sheen"));
    // One info finding lists what is carried; an extension on a mesh has no bag, so that one is reported as dropped.
    let note = first.report.findings.iter().find(|f| f.rule == "gltf.passthrough").expect("a passthrough note");
    assert_eq!(note.severity, Severity::Info);
    for what in
        ["scene: extensions EXT_studio_tag; extras", "node 0: extras", "material 0: extensions KHR_materials_sheen"]
    {
        assert!(note.message.contains(what), "{what} in {}", note.message);
    }
    let dropped: Vec<_> = first.report.findings.iter().filter(|f| f.rule == "gltf.extension_dropped").collect();
    assert_eq!(dropped.len(), 1);
    assert!(dropped[0].message.contains("EXT_on_a_mesh"));

    // Written back verbatim, listed as used, and unchanged by a second round trip.
    let file = export_bytes(&MockGlbExporter, &first.scene);
    let (out, _) = glb_parts(&file).unwrap();
    assert_eq!(out["extensions"]["EXT_studio_tag"], doc["extensions"]["EXT_studio_tag"]);
    assert_eq!(out["extras"], doc["extras"]);
    assert_eq!(out["nodes"][0]["extras"], doc["nodes"][0]["extras"]);
    assert_eq!(out["materials"][0]["extensions"], doc["materials"][0]["extensions"]);
    assert_eq!(out["extensionsUsed"], json!(["EXT_studio_tag", "KHR_materials_sheen"]));
    let again = import(&MockGlbImporter, &file).scene;
    assert_eq!(again.passthrough, first.scene.passthrough);
    assert_eq!(again.nodes[0].passthrough, first.scene.nodes[0].passthrough);
    assert_eq!(again.materials[0].passthrough, first.scene.materials[0].passthrough);
    assert_eq!(file, export_bytes(&MockGlbExporter, &again), "pass-through keeps the output canonical");
    // The export preflight mentions it too.
    let options = options_without_face_channels();
    let out = run_export(&MockGlbExporter, &first.scene, &ExportCtx::new(&options)).expect("export");
    assert!(out.report.has_rule("gltf.passthrough"));
}

/// An importer with a bug: it emits a scene with a dangling mesh index. The boundary must catch it.
#[derive(Default)]
struct BrokenImporter;

impl Importer for BrokenImporter {
    fn info(&self) -> omuzaki_io_api::FormatInfo {
        omuzaki_io_api::FormatInfo {
            id: "broken",
            name: "broken",
            extensions: &["bad"],
            media_type: "application/octet-stream",
        }
    }
    fn sniff(&self, _: &ImportInput<'_>) -> Confidence {
        Confidence::No
    }
    fn import(&self, _: &ImportInput<'_>, _: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        let mut scene = IoScene::new();
        let mut node = omuzaki_io_api::Node::new("n");
        node.mesh = Some(omuzaki_io_api::MeshIx(5));
        scene.add_node(node);
        Ok(ImportOutcome::new(scene, LicenceEvidence::Absent))
    }
}

#[test]
fn run_import_rejects_an_importer_that_breaks_the_scene_invariants() {
    let Err(IoError::Rejected(report)) = import_with(&BrokenImporter, b"x", &ImportOptions::default()) else {
        panic!("expected a rejection")
    };
    let finding = report.errors().next().expect("an error");
    assert_eq!(
        (finding.rule.as_str(), finding.location.as_deref()),
        ("scene.index_out_of_range", Some("nodes/0/mesh"))
    );
}

#[test]
fn strict_mode_turns_warnings_into_a_rejection() {
    let file = export_bytes(&MockVrm1Exporter, &tiny_avatar());
    let (mut doc, bin) = glb_parts(&file).unwrap();
    doc["extensions"]["VRMC_vrm"]["humanoid"]["humanBones"]["tail"] = json!({ "node": 0 });
    let edited = glb_from_json(&doc, &bin);
    let lenient = import_with(&MockVrm1Importer, &edited, &ImportOptions::default()).expect("lenient import");
    assert!(lenient.report.has_rule("vrm.humanoid_unknown_bone") && lenient.report.count(Severity::Warning) > 0);
    let Err(IoError::Rejected(report)) = import_with(&MockVrm1Importer, &edited, &ImportOptions::default().strict())
    else {
        panic!("expected a rejection")
    };
    assert!(report.errors().any(|f| f.rule == "vrm.humanoid_unknown_bone"));
}

struct Cancelled(AtomicBool);

impl Progress for Cancelled {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[test]
fn cancellation_stops_import_and_export() {
    let cancelled = Cancelled(AtomicBool::new(true));
    let options = ImportOptions::default();
    let ctx = ImportCtx::new(&options).with_progress(&cancelled);
    assert_eq!(run_import(&MockGlbImporter, &ImportInput::new(&tiny_glb()), &ctx).unwrap_err(), IoError::Cancelled);
    let export_options = options_without_face_channels();
    let ctx = ExportCtx::new(&export_options).with_progress(&cancelled);
    assert_eq!(run_export(&MockVrm1Exporter, &tiny_avatar(), &ctx).unwrap_err(), IoError::Cancelled);
}

#[test]
fn a_rejected_export_reports_every_problem_at_once() {
    let mut scene = tiny_avatar();
    scene.provenance = None;
    scene.meta.name.clear();
    let options = options_without_face_channels();
    let Err(IoError::Rejected(report)) = run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&options)) else {
        panic!("expected a rejection")
    };
    let rules: Vec<_> = report.errors().map(|f| f.rule.as_str()).collect();
    assert_eq!(
        rules,
        ["rights.provenance_missing", "vrm.meta_name_missing"],
        "preflight first, then the exporter's own checks"
    );
    let _: Report = report;
}

#[test]
fn layouts_the_mock_cannot_write_are_refused_up_front() {
    let options = options_without_face_channels().with_layout(omuzaki_io_api::Layout::Separate);
    let Err(IoError::Rejected(report)) =
        run_export(&MockGlbExporter, &fixtures::tiny_avatar(), &ExportCtx::new(&options))
    else {
        panic!("expected a rejection")
    };
    assert!(report.has_rule("gltf.layout_unsupported"));
}
