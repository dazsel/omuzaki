//! `omuzaki-io-fbx` end to end: sniffing, units and axes, the hierarchy, the failure paths and the registry. The
//! documents are written by [`common::AsciiFbx`]; there is no third-party file.

mod common;

use common::{AsciiFbx, Axis, SCENE_ROOT};
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
fn maya() -> AsciiFbx {
    AsciiFbx::new(Axis::Y, Axis::Z, Axis::X, 1.0)
}

/// A 3ds Max-style file: Z up, the character faces -Y (`front` -Y), one unit is one centimetre.
fn max() -> AsciiFbx {
    AsciiFbx::new(Axis::Z, Axis::NEG_Y, Axis::X, 1.0)
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
    let mut file = AsciiFbx::new(Axis::Y, Axis::Z, Axis::X, 100.0);
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
