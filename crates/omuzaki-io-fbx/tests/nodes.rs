//! `omuzaki-io-fbx` end to end: sniffing, units and axes, the hierarchy, the failure paths and the registry. The
//! documents are written by [`common::FbxBuilder`]; there is no third-party file.

mod common;

use common::{Axis, FbxBuilder, SCENE_ROOT};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{
    Confidence, ImportCtx, ImportInput, ImportOptions, ImportOutcome, Importer, IoError, Limits, NodeIx, Progress,
    Severity, run_import,
};
use omuzaki_io_fbx::FbxImporter;

const EPS: f32 = 1e-5;

fn import(bytes: &[u8]) -> Result<ImportOutcome, IoError> {
    let options = ImportOptions::default();
    run_import(&FbxImporter, &ImportInput::new(bytes), &ImportCtx::new(&options))
}

fn rules(outcome: &ImportOutcome) -> Vec<&str> {
    outcome.report.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).abs().max_element() < EPS
}

/// A Maya-style file: Y up, front +Z, one unit is one centimetre.
fn maya() -> FbxBuilder {
    FbxBuilder::new(Axis::Y, Axis::Z, Axis::X, 1.0)
}

/// A 3ds Max-style file: Z up, the character faces -Y (`front` -Y), one unit is one centimetre.
fn max() -> FbxBuilder {
    FbxBuilder::new(Axis::Z, Axis::NEG_Y, Axis::X, 1.0)
}

#[test]
fn sniffs_binary_and_ascii_magic() {
    let sniff = |bytes: &[u8]| FbxImporter.sniff(&ImportInput::new(bytes));
    assert_eq!(sniff(b"Kaydara FBX Binary  \0\x1a\0\xe8\x1c\0\0"), Confidence::Certain);
    assert_eq!(sniff(&maya().bytes()), Confidence::Certain);
    assert_eq!(sniff(b"\xEF\xBB\xBF\n\n; FBX 7.4.0 project file\n"), Confidence::Certain);
    assert_eq!(sniff(b"{\"asset\":{\"version\":\"2.0\"}}"), Confidence::No);
    assert_eq!(sniff(b"glTF\x02\0\0\0\x14\0\0\0"), Confidence::No);
    assert_eq!(sniff(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n"), Confidence::No);
    assert_eq!(sniff(b""), Confidence::No);
    // The binary magic needs its NUL; the same text without it is not a header.
    assert_eq!(sniff(b"Kaydara FBX Binary  "), Confidence::No);
}

#[test]
fn a_y_up_file_in_centimetres_lands_in_metres() {
    let mut file = maya();
    file.model("Head", SCENE_ROOT, [0.0, 170.0, 0.0], [0.0; 3]);
    file.model("Nose", SCENE_ROOT, [0.0, 0.0, 50.0], [0.0; 3]);
    let out = import(&file.bytes()).expect("a valid file");
    let t = |i: usize| out.scene.nodes[i].transform.translation;
    assert!(close(t(0), Vec3::new(0.0, 1.70, 0.0)), "{:?}", t(0));
    // 50 cm toward the front the file declares (+Z) is z = +0.50.
    assert!(close(t(1), Vec3::new(0.0, 0.0, 0.50)), "{:?}", t(1));
    assert!(rules(&out).contains(&"fbx.units_converted"), "{:?}", rules(&out));
    assert!(!rules(&out).contains(&"fbx.axes_converted"), "a Y-up, +Z-front file needs no rotation");
    assert_eq!(out.scene.source_generator.as_deref(), Some("Omuzaki test builder"));
}

#[test]
fn a_file_in_metres_has_no_units_finding() {
    let mut file = FbxBuilder::new(Axis::Y, Axis::Z, Axis::X, 100.0);
    file.model("A", SCENE_ROOT, [0.0, 2.0, 0.0], [0.0; 3]);
    let out = import(&file.bytes()).expect("a valid file");
    assert!(close(out.scene.nodes[0].transform.translation, Vec3::new(0.0, 2.0, 0.0)));
    assert!(rules(&out).iter().all(|r| !r.starts_with("fbx.")), "{:?}", rules(&out));
}

#[test]
fn a_z_up_file_is_rotated_to_y_up() {
    let mut file = max();
    file.model("Head", SCENE_ROOT, [0.0, 0.0, 170.0], [0.0; 3]);
    // 50 cm along -Y, the way this file's character faces, is 0.50 m toward the front: +Z.
    file.model("Nose", SCENE_ROOT, [0.0, -50.0, 0.0], [0.0; 3]);
    let out = import(&file.bytes()).expect("a valid file");
    let t = |i: usize| out.scene.nodes[i].transform.translation;
    assert!(close(t(0), Vec3::new(0.0, 1.70, 0.0)), "{:?}", t(0));
    assert!(close(t(1), Vec3::new(0.0, 0.0, 0.50)), "{:?}", t(1));
    assert!(rules(&out).contains(&"fbx.axes_converted"), "{:?}", rules(&out));
    assert!(out.report.findings.iter().filter(|f| f.rule.starts_with("fbx.")).all(|f| f.severity == Severity::Info));
}

#[test]
fn hierarchy_and_local_transforms_survive() {
    // A three-deep chain whose middle node turns 90 degrees about the file's up axis (Z), then two children in file order.
    let mut file = max();
    let a = file.model("A", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    let b = file.model("B", a, [0.0, 0.0, 100.0], [0.0, 0.0, 90.0]);
    file.model("C", b, [0.0, 100.0, 0.0], [0.0; 3]);
    file.model("D", b, [0.0; 3], [0.0; 3]);
    let out = import(&file.bytes()).expect("a valid file");
    let s = &out.scene;
    let names: Vec<&str> = s.nodes.iter().map(|n| n.name.as_str()).collect();
    let ix = |name: &str| {
        let at = names.iter().position(|n| n.ends_with(name)).expect("node present");
        NodeIx(u32::try_from(at).expect("a small scene"))
    };
    let at = |name: &str| ix(name).index();
    assert_eq!(s.roots, [ix("A")]);
    assert_eq!(s.nodes[at("A")].children, [ix("B")]);
    assert_eq!(s.nodes[at("B")].children, [ix("C"), ix("D")], "children in file order");
    let turn = s.nodes[at("B")].transform.rotation;
    assert!(turn.dot(Quat::from_rotation_y(90f32.to_radians())).abs() > 1.0 - EPS, "{turn:?}");
    assert!(close(s.nodes[at("B")].transform.translation, Vec3::new(0.0, 1.0, 0.0)));
}

#[test]
fn malformed_and_limits() {
    // A truncated binary header and a wrong-magic file are errors, not panics.
    let truncated = import(b"Kaydara FBX Binary  \0");
    assert!(matches!(truncated, Err(IoError::Malformed { .. })), "{truncated:?}");
    let mut file = maya();
    file.model("A", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    let text = file.bytes();
    let cut = import(&text[..text.len() / 2]);
    assert!(matches!(cut, Err(IoError::Malformed { .. })), "{cut:?}");
    let wrong = import(b"GLB\0 this is not an FBX file");
    assert!(matches!(wrong, Err(IoError::Unsupported { .. })), "{wrong:?}");

    // The input cap.
    let options = ImportOptions::default().with_limits(Limits { max_input_bytes: 10, ..Limits::default() });
    let capped = run_import(&FbxImporter, &ImportInput::new(&maya().bytes()), &ImportCtx::new(&options));
    assert!(matches!(capped, Err(IoError::Limit { what: "input size in bytes", limit: 10, .. })), "{capped:?}");

    // An OBJ ufbx could read is not accepted, and says where to go instead.
    let obj = import(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    let Err(IoError::Unsupported { hint, .. }) = obj else { panic!("an OBJ is unsupported: {obj:?}") };
    assert_eq!(hint.as_deref(), Some("convert it to glTF or VRM"));
}

#[test]
fn the_node_cap_and_the_depth_cap_are_limits() {
    let mut flat = maya();
    for i in 0..3 {
        flat.model(&format!("N{i}"), SCENE_ROOT, [0.0; 3], [0.0; 3]);
    }
    let options = ImportOptions::default().with_limits(Limits { max_nodes: 2, ..Limits::default() });
    let r = run_import(&FbxImporter, &ImportInput::new(&flat.bytes()), &ImportCtx::new(&options));
    assert!(matches!(r, Err(IoError::Limit { what: "node count", limit: 2, got: 3 })), "{r:?}");

    let mut deep = maya();
    let mut parent = SCENE_ROOT;
    for i in 0..1100 {
        parent = deep.model(&format!("D{i}"), parent, [0.0; 3], [0.0; 3]);
    }
    let r = import(&deep.bytes());
    assert!(matches!(r, Err(IoError::Limit { what: "node hierarchy depth", .. })), "{r:?}");
}

#[test]
fn a_cancelled_import_stops() {
    struct Cancelled;
    impl Progress for Cancelled {
        fn stage(&self, _: &str, _: f32) {}
        fn is_cancelled(&self) -> bool {
            true
        }
    }
    let options = ImportOptions::default();
    let ctx = ImportCtx::new(&options).with_progress(&Cancelled);
    let r = run_import(&FbxImporter, &ImportInput::new(&maya().bytes()), &ctx);
    assert!(matches!(r, Err(IoError::Cancelled)), "{r:?}");
}

#[test]
fn registry() {
    omuzaki_io_fbx::link();
    assert!(omuzaki_io_api::find_importer("fbx").is_some());
    assert!(omuzaki_io_api::find_exporter("fbx").is_none(), "FBX is import only");
    let fbx = omuzaki_io_api::formats().into_iter().find(|f| f.info.id == "fbx").expect("fbx is listed");
    assert!(fbx.import && !fbx.export);
    assert_eq!(
        (fbx.info.name, fbx.info.extensions, fbx.info.media_type),
        ("FBX", &["fbx"][..], "application/vnd.autodesk.fbx")
    );
    let mut file = maya();
    file.model("A", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    let out = import(&file.bytes()).expect("a valid file");
    let note = out.report.findings.iter().find(|f| f.rule == "rights.no_license_info").expect("the rights note");
    assert_eq!(note.severity, Severity::Info);
}

#[test]
fn a_z_up_files_non_uniform_scale_lands_on_the_right_axes() {
    // This file's axes: X right, Z up, -Y toward the viewer. Canonical Y (up) is the file's Z and canonical Z (front) is
    // the file's -Y, so a scale of (2, 3, 4) in the file is (2, 4, 3) in canonical space: the scales travel with their
    // axes, and a mirrored or dropped permutation would give (2, 3, 4) or (4, 3, 2).
    let mut file = max();
    file.scaled_model("Box", SCENE_ROOT, [0.0; 3], [0.0; 3], [2.0, 3.0, 4.0]);
    let out = import(&file.bytes()).expect("a valid file");
    let scale = out.scene.nodes[0].transform.scale;
    assert!(close(scale, Vec3::new(2.0, 4.0, 3.0)), "{scale:?}");
}

#[test]
fn a_left_handed_file_is_mirrored_to_right_handed() {
    // Y up, X right, and the front toward -Z: right x up is +Z, not the front, so the file is left-handed. The importer
    // mirrors it. A node 50 cm toward the front the file declares (-Z) lands 0.50 m toward +Z, and a turn of 90 degrees
    // about the file's Y is a turn of -90 degrees about +Y once the mirror has reversed its sense.
    let mut file = FbxBuilder::new(Axis::Y, Axis::NEG_Z, Axis::X, 1.0);
    file.model("Nose", SCENE_ROOT, [0.0, 0.0, -50.0], [0.0, 90.0, 0.0]);
    let out = import(&file.bytes()).expect("a valid file");
    let node = &out.scene.nodes[0].transform;
    assert!(close(node.translation, Vec3::new(0.0, 0.0, 0.50)), "{:?}", node.translation);
    let want = Quat::from_rotation_y(-90f32.to_radians());
    assert!(node.rotation.dot(want).abs() > 1.0 - EPS, "{:?}", node.rotation);
    assert!(rules(&out).contains(&"fbx.axes_converted"), "{:?}", rules(&out));
}

#[test]
fn a_binary_file_imports_as_the_same_scene_as_its_ascii_twin() {
    let mut file = max();
    let a = file.model("A", SCENE_ROOT, [0.0, 0.0, 170.0], [0.0, 0.0, 30.0]);
    file.scaled_model("B", a, [10.0, 20.0, 30.0], [0.0; 3], [1.0, 2.0, 3.0]);
    file.model("C", SCENE_ROOT, [0.0, -50.0, 0.0], [0.0; 3]);
    let binary = file.binary();
    assert_eq!(FbxImporter.sniff(&ImportInput::new(&binary)), Confidence::Certain);
    let from_binary = import(&binary).expect("a valid binary file");
    let from_ascii = import(&file.bytes()).expect("a valid ASCII file");
    assert_eq!(from_binary.scene.nodes, from_ascii.scene.nodes);
    assert_eq!(from_binary.scene.roots, from_ascii.scene.roots);
    assert_eq!(rules(&from_binary), rules(&from_ascii));
    assert!(rules(&from_binary).contains(&"fbx.axes_converted"));
    // A binary file cut short is malformed, not a panic.
    let cut = import(&binary[..binary.len() / 2]);
    assert!(matches!(cut, Err(IoError::Malformed { .. })), "{cut:?}");
}

#[test]
fn content_this_version_does_not_read_is_reported_per_file() {
    let mut file = maya();
    let body = file.model("Body", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    file.mesh(body);
    file.material(body);
    // Vertices are never read, so a vertex cap of 1 cannot trip on a mesh that has three.
    let options = ImportOptions::default().with_limits(Limits { max_vertices: 1, ..Limits::default() });
    let out = run_import(&FbxImporter, &ImportInput::new(&file.bytes()), &ImportCtx::new(&options))
        .expect("the hierarchy imports whatever the file holds besides");
    assert_eq!(out.scene.nodes.len(), 1);
    let dropped =
        out.report.findings.iter().find(|f| f.rule == "fbx.content_dropped").expect("a dropped-content finding");
    assert_eq!(dropped.severity, Severity::Warning);
    assert!(dropped.message.contains("1 meshes") && dropped.message.contains("1 materials"), "{}", dropped.message);
    assert_eq!(dropped.hint.as_deref(), Some("convert the model to glTF or VRM to keep its content"));
    // A strict import refuses what it would otherwise lose.
    let strict = ImportOptions::default().strict();
    let refused = run_import(&FbxImporter, &ImportInput::new(&file.bytes()), &ImportCtx::new(&strict));
    assert!(refused.is_err(), "a warning is an error in strict mode");
}

#[test]
fn a_file_ufbx_had_to_repair_says_so() {
    let mut file = maya();
    file.model("First", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    // The same object id twice: ufbx keeps one and warns.
    file.model_with_id(1000, "Second", SCENE_ROOT);
    let out = import(&file.bytes()).expect("a repaired file still imports");
    let repaired = out.report.findings.iter().find(|f| f.rule == "fbx.repaired").expect("a repair finding");
    assert_eq!(repaired.severity, Severity::Warning);
    assert!(repaired.message.starts_with("ufbx repaired the file: "), "{}", repaired.message);
}

/// Cancels once the import reaches a named stage, and at a given fraction of it.
struct CancelAt {
    stage: &'static str,
    fraction: Option<f32>,
    now: std::sync::atomic::AtomicBool,
}

impl Progress for CancelAt {
    fn stage(&self, name: &str, fraction: f32) {
        if name == self.stage && self.fraction.is_none_or(|at| (fraction - at).abs() < f32::EPSILON) {
            self.now.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.now.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[test]
fn a_cancel_while_the_hierarchy_is_wired_or_at_the_last_check_still_stops_the_import() {
    let mut file = maya();
    let a = file.model("A", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    file.model("B", a, [0.0; 3], [0.0; 3]);
    let bytes = file.bytes();
    let options = ImportOptions::default();
    for (stage, fraction) in [("fbx.hierarchy", None), ("fbx.nodes", Some(1.0))] {
        let progress = CancelAt { stage, fraction, now: std::sync::atomic::AtomicBool::new(false) };
        let ctx = ImportCtx::new(&options).with_progress(&progress);
        let r = run_import(&FbxImporter, &ImportInput::new(&bytes), &ctx);
        assert!(matches!(r, Err(IoError::Cancelled)), "{stage}: {r:?}");
    }
}

#[test]
fn a_limit_error_names_the_ceiling_the_load_ran_under() {
    // With a node cap of 2 the load runs under a tight allocator (about 16 MiB and 100,000 allocations). A file with tens
    // of thousands of models runs into it, and the error says that ceiling, not ufbx's fixed 1 GiB or 32,000,000.
    let mut file = maya();
    for i in 0..30_000 {
        file.model(&format!("N{i}"), SCENE_ROOT, [0.0; 3], [0.0; 3]);
    }
    let options = ImportOptions::default().with_limits(Limits { max_nodes: 2, ..Limits::default() });
    let r = run_import(&FbxImporter, &ImportInput::new(&file.bytes()), &ImportCtx::new(&options));
    match r {
        Err(IoError::Limit { what, limit, got }) if what != "node count" => {
            assert!(limit <= 17 << 20, "{what}: the error names a limit of {limit}, not the one applied");
            assert_eq!(got, limit + 1);
        }
        other => panic!("expected an allocator limit, got {other:?}"),
    }
}
