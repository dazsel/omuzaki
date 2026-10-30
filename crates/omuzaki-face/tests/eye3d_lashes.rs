//! Lashes placed by hand on the 3D eye: each placed clump grows its own hairs where it says along its lid, gathered to
//! one tip, on top of the grown lashes or alone; adding a clump never moves another; `face.eye3d.lashes` places them as
//! one undoable edit.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::glam::Vec3;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::eye3d::{EyeDesign, meshes};
use omuzaki_face_api::FaceProfile;
use omuzaki_face_api::eye3d::LashClump;
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

/// Vertices per lash: eleven points along it, two across.
const PER_LASH: usize = 22;

fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    omuzaki_face::link();
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

fn clump(upper: bool, at: f32, strands: u32) -> LashClump {
    LashClump { upper, at, strands, spread: 0.0008, length: 0.008, curl: 1.0, sweep: 20.0 }
}

fn lashes(design: &EyeDesign) -> Vec<Vec3> {
    meshes(design, Vec3::new(0.0, 0.0, 0.07), Vec3::Z).lashes.positions
}

#[test]
fn a_placed_clump_grows_its_hairs_where_it_says() {
    let mut design = EyeDesign::default();
    (design.lashes.upper, design.lashes.lower) = (0, 0);
    assert!(lashes(&design).is_empty(), "no grown lashes, no placed ones: nothing");

    design.lashes.clumps = vec![clump(true, 0.2, 6)];
    let one = lashes(&design);
    assert_eq!(one.len(), 6 * PER_LASH, "six hairs");
    // At the outer part of the upper lid: toward -x, above the eye's centre line.
    let roots: Vec<Vec3> = one.chunks(PER_LASH).map(|hair| hair[0]).collect();
    assert!(roots.iter().all(|r| r.x < -0.003 && r.y > 0.0), "{roots:?}");
    // Gathered: the tips are much closer together than the roots.
    let tips: Vec<Vec3> = one.chunks(PER_LASH).map(|hair| hair[PER_LASH - 1]).collect();
    let span = |ps: &[Vec3]| ps.iter().map(|p| (*p - ps[0]).length()).fold(0.0, f32::max);
    assert!(span(&tips) < 0.5 * span(&roots), "a clump gathers to one tip");

    design.lashes.clumps.push(clump(false, 0.6, 3));
    let two = lashes(&design);
    assert_eq!(two.len(), 9 * PER_LASH);
    assert_eq!(&two[..one.len()], &one[..], "adding a clump never moves another");
}

#[test]
fn an_agent_places_lashes_alone_and_undo_takes_them_away() {
    let face = serde_json::to_value(FaceProfile::default()).unwrap();
    let mut doc = MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }));
    let before = doc.root().clone();
    let args = json!({ "character": "mira", "grown": [0, 0], "clumps": [
        { "upper": true, "at": 0.2, "strands": 8, "spread": 0.0008, "length": 0.009, "curl": 1.2, "sweep": 30 },
        { "upper": false, "at": 0.3, "strands": 3, "length": 0.004 }
    ]});
    let plan = run(&doc, "face.eye3d.lashes", args).expect("places");
    assert_eq!(plan.output.as_ref().unwrap()["clumps"], json!(2));
    doc.apply(&plan.patch).expect("applies");
    let path = DocPath::parse("characters/mira/face").unwrap();
    let face: FaceProfile = serde_json::from_value(doc.get(&path).cloned().unwrap()).unwrap();
    let eye = face.eye3d.expect("a 3D eye");
    assert_eq!((eye.lashes.upper, eye.lashes.lower), (0, 0));
    assert_eq!(lashes(&eye).len(), 11 * PER_LASH, "only the placed hairs");

    doc.apply(&plan.patch.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before);
    let bad = json!({ "character": "mira", "clumps": [{ "upper": true, "at": 1.5, "strands": 4, "length": 0.008 }] });
    let err = run(&doc, "face.eye3d.lashes", bad).unwrap_err();
    assert!(err.to_string().contains("lashes/clumps/0/at"), "{err}");
}
