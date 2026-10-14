//! Parity of the capsule-segment auto-weights (`omuzaki_char::kit::skin`) with the prototype's `skinKit`.
//!
//! Two oracles:
//! - `fixtures/char_auto_weights/capsule.json`: numbers from running the prototype's own `build.js` (`SEG`, `capsuleWeights`
//!   and `setWeights`) under Node on 5 000 synthetic points around the sample character's segment set
//!   (the prototype's generator script). The points are repeated here from the same seeded generator.
//! - The `tights` and `hands` meshes of the JS reference dump, which hold the prototype's own skin weights. These
//!   tests skip, with a message, when the local assets are not present (the dump is the sample character's local material and never in
//!   git).
//!
//! Tolerances: capsule weights 1e-4 per weight (the fixture rounds to 1e-5); the dumps 1e-3 after the top-four cut.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a bad fixture should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)] // fixture indices and f32 inputs

use std::collections::BTreeMap;

use omuzaki_char::kit::js_names;
use omuzaki_char::kit::skin::{CapsuleParams, SegmentSet, blend_between, limit_influences, normalise};
use omuzaki_char_api::{JointFit, JointRef, MaterialName, MeshData, PartMesh};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_test::jsref::JsRef;
use serde::Deserialize;

/// The capsule-weight tolerance of the issue: per weight.
const CAPSULE_TOLERANCE: f64 = 1e-4;
/// The tolerance against the dumps, after truncation to four influences.
const DUMP_TOLERANCE: f32 = 1e-3;

#[derive(Deserialize)]
struct Fixture {
    seed: u32,
    joints: BTreeMap<String, [f64; 3]>,
    crown: [f64; 3],
    bones: Vec<String>,
    segments: Vec<[[f64; 3]; 2]>,
    first_points: Vec<[f64; 3]>,
    capsule_1e5: Vec<Vec<i32>>,
    top4: Vec<(usize, Vec<(usize, f64)>)>,
    subsets: Vec<Subset>,
}

#[derive(Deserialize)]
struct Subset {
    bones: Vec<String>,
    weights: Vec<Vec<i32>>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/char_auto_weights/capsule.json")).expect("the fixture parses")
}

fn joint(name: &str) -> JointRef {
    js_names::joint(name).expect("a prototype bone name")
}

/// mulberry32, the same 32-bit integer arithmetic as the fixture generator, so the points are bit-identical.
struct Mulberry32(u32);

impl Mulberry32 {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut t = (self.0 ^ (self.0 >> 15)).wrapping_mul(1 | self.0);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(0x3D | t)) ^ t;
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

/// The generator's points: three in five uniform in a box round the body, two in five within 5 cm per axis of a random
/// point on a random segment. The draws happen in the generator's order.
fn points(f: &Fixture, count: usize) -> Vec<DVec3> {
    let mut rng = Mulberry32(f.seed);
    (0..count)
        .map(|i| {
            if i % 5 < 3 {
                let (rx, ry, rz) = (rng.next(), rng.next(), rng.next());
                DVec3::new(-0.3 + 0.6 * rx, 0.95 * ry, -0.2 + 0.4 * rz)
            } else {
                let (rs, rt, jx, jy, jz) = (rng.next(), rng.next(), rng.next(), rng.next(), rng.next());
                let [a, b] = f.segments[(rs * f.bones.len() as f64).floor() as usize];
                let at = |k: usize| a[k] + (b[k] - a[k]) * rt;
                DVec3::new(
                    at(0) + (jx * 2.0 - 1.0) * 0.05,
                    at(1) + (jy * 2.0 - 1.0) * 0.05,
                    at(2) + (jz * 2.0 - 1.0) * 0.05,
                )
            }
        })
        .collect()
}

/// The sample character's segment set the way the template would build it: from the joints (rounded to f32, as `BodyFit` holds
/// them), the prototype's terminal rules, and the crown the head part knows.
fn mira(f: &Fixture) -> (SegmentSet, Vec<JointRef>) {
    let fit =
        JointFit::new(f.joints.iter().map(|(name, p)| (joint(name), Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32))));
    let bones: Vec<JointRef> = f.bones.iter().map(|b| joint(b)).collect();
    let set = SegmentSet::from_fit(&fit, &bones).expect("finite joints").with_crown(DVec3::from_array(f.crown));
    (set, bones)
}

fn normalised(raw: &[(JointRef, f64)]) -> Vec<f64> {
    let total: f64 = raw.iter().map(|(_, w)| w).sum();
    raw.iter().map(|(_, w)| w / total).collect()
}

#[test]
fn the_generator_repeats_and_the_segments_are_the_prototypes_seg() {
    let f = fixture();
    let pts = points(&f, 3);
    for (mine, theirs) in pts.iter().zip(&f.first_points) {
        assert_eq!(mine.to_array(), *theirs, "the seeded generators must agree before anything else can");
    }
    let (set, bones) = mira(&f);
    assert_eq!(set.segments().len(), 19);
    let mut worst = 0.0_f64;
    for ((bone, name), [a, b]) in bones.iter().zip(&f.bones).zip(&f.segments) {
        let s = set.get(bone).unwrap_or_else(|| panic!("no segment for {name}"));
        worst = worst.max(s.head.distance(DVec3::from_array(*a))).max(s.tail.distance(DVec3::from_array(*b)));
    }
    println!("SegmentSet::from_fit vs the prototype's SEG: worst endpoint difference {worst:.2e} m");
    assert!(worst < 1e-6, "{worst:e}");
}

#[test]
fn capsule_weights_match_the_prototype_on_5000_points() {
    let f = fixture();
    let (set, bones) = mira(&f);
    let pts = points(&f, 5000);
    assert_eq!((pts.len(), f.capsule_1e5.len()), (5000, 5000));
    let mut worst = (0.0_f64, 0);
    for (i, (p, expected)) in pts.iter().zip(&f.capsule_1e5).enumerate() {
        let mine = normalised(&set.capsule_weights(*p, &bones, CapsuleParams::default()));
        assert_eq!(mine.len(), expected.len());
        for (m, e) in mine.iter().zip(expected) {
            let error = (m - f64::from(*e) / 1e5).abs();
            if error > worst.0 {
                worst = (error, i);
            }
        }
    }
    println!("capsuleWeights, 5000 points x 19 bones: worst per-weight error {:.2e} (point {})", worst.0, worst.1);
    assert!(worst.0 <= CAPSULE_TOLERANCE, "point {}: {:e}", worst.1, worst.0);
}

#[test]
fn the_top_four_selection_matches_set_weights() {
    let f = fixture();
    let (set, bones) = mira(&f);
    let pts = points(&f, 5000);
    let mut worst = 0.0_f64;
    for (i, expected) in &f.top4 {
        let mine = set.weights_at(pts[*i], &bones, CapsuleParams::default());
        assert_eq!(mine.len(), expected.len(), "point {i}: {mine:?}");
        for (bone, w) in expected {
            let got = mine.iter().find(|(j, _)| *j == bones[*bone]).map_or(0.0, |(_, w)| f64::from(*w));
            worst = worst.max((got - w).abs());
        }
    }
    println!("setWeights top four, {} points: worst per-weight error {worst:.2e}", f.top4.len());
    assert!(worst <= CAPSULE_TOLERANCE, "{worst:e}");
}

#[test]
fn candidate_subsets_match_the_prototype() {
    let f = fixture();
    let (set, _) = mira(&f);
    let pts = points(&f, 400);
    for subset in &f.subsets {
        let candidates: Vec<JointRef> = subset.bones.iter().map(|b| joint(b)).collect();
        let mut worst = 0.0_f64;
        for (p, expected) in pts.iter().zip(&subset.weights) {
            let mine = normalised(&set.capsule_weights(*p, &candidates, CapsuleParams::default()));
            for (m, e) in mine.iter().zip(expected) {
                worst = worst.max((m - f64::from(*e) / 1e5).abs());
            }
        }
        println!("candidates {:?}: worst per-weight error {worst:.2e}", subset.bones);
        assert!(worst <= CAPSULE_TOLERANCE, "{:?}: {worst:e}", subset.bones);
    }
}

#[test]
fn skinning_a_whole_mesh_gives_the_same_weights_as_one_point_at_a_time() {
    let f = fixture();
    let (set, bones) = mira(&f);
    let pts = points(&f, 5000);
    let n = pts.len();
    let mut mesh = PartMesh {
        name: "cloud".to_owned(),
        geometry: MeshData {
            positions: pts.iter().map(|p| p.as_vec3()).collect(),
            normals: vec![Vec3::Y; n],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        },
        joint_bones: Vec::new(),
        material: MaterialName::new("m").unwrap(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    };
    set.skin_mesh(&mut mesh, &bones, CapsuleParams::default()).unwrap();
    mesh.validate().unwrap();
    let mut worst = 0.0_f64;
    for (i, expected) in &f.top4 {
        let (joints, weights) = (mesh.geometry.joints[*i], mesh.geometry.weights[*i]);
        for (bone, w) in expected {
            let got: f64 = joints
                .iter()
                .zip(weights)
                .filter(|(j, w)| *w > 0.0 && mesh.joint_bones[usize::from(**j)] == bones[*bone])
                .map(|(_, w)| f64::from(w))
                .sum();
            worst = worst.max((got - w).abs());
        }
    }
    println!("skin_mesh vs setWeights, {} points: worst per-weight error {worst:.2e}", f.top4.len());
    assert!(worst <= CAPSULE_TOLERANCE, "{worst:e}");
}

// ------------------------------------------------------------------------------------------------ against the dumps

/// The joint for a bone name as the dump writes it: three.js drops the dot of a node name (`hand.L` is `handL`).
fn dump_joint(name: &str) -> JointRef {
    const SIDED: [&str; 7] = ["upper_arm", "lower_arm", "hand", "upper_leg", "lower_leg", "foot", "toes"];
    let sided = name.strip_suffix(['L', 'R']).filter(|stem| SIDED.contains(stem));
    joint(&sided.map_or_else(|| name.to_owned(), |stem| format!("{stem}.{}", &name[stem.len()..])))
}

/// A dump vertex's skin as `(joint, weight)` pairs.
fn dump_skin(mesh: &MeshData, bones: &[String], vertex: usize) -> Vec<(JointRef, f32)> {
    mesh.joints[vertex]
        .iter()
        .zip(mesh.weights[vertex])
        .filter(|(_, w)| *w > 0.0)
        .map(|(j, w)| (dump_joint(&bones[usize::from(*j)]), w))
        .collect()
}

/// The largest difference between two skins' weights on any joint.
fn skin_difference(a: &[(JointRef, f32)], b: &[(JointRef, f32)]) -> f32 {
    let on = |skin: &[(JointRef, f32)], j: &JointRef| skin.iter().filter(|(k, _)| k == j).map(|(_, w)| w).sum::<f32>();
    a.iter().chain(b).map(|(j, _)| (on(a, j) - on(b, j)).abs()).fold(0.0, f32::max)
}

/// The top-four cut of the prototype's `setWeights`, on a blend built from the pieces the parts use.
fn finished(mut blend: Vec<(JointRef, f32)>) -> Vec<(JointRef, f32)> {
    limit_influences(&mut blend, 4);
    normalise(&mut blend);
    blend
}

#[test]
fn the_tights_blends_match_the_dump() {
    let Some(js) = JsRef::open() else { return };
    let dump = js.geometry("game").unwrap();
    let tights = dump.mesh("tights").expect("the dump has the tights");
    // body.js `W`: hips above 0.425 m, the thigh between the hip blend and the knee blend, the shin below 0.195 m.
    let mut worst = (0.0_f32, 0);
    for (v, p) in tights.mesh.positions.iter().enumerate() {
        let side = if p.x >= 0.0 { "L" } else { "R" };
        let (upper, lower) = (joint(&format!("upper_leg.{side}")), joint(&format!("lower_leg.{side}")));
        let up = blend_between(JointRef::HIPS, upper.clone(), 0.425, 0.335).amount(f64::from(p.y));
        let lo = blend_between(upper.clone(), lower.clone(), 0.268, 0.195).amount(f64::from(p.y));
        let mine = finished(vec![
            (JointRef::HIPS, (1.0 - up) as f32),
            (upper, (up * (1.0 - lo)) as f32),
            (lower, (up * lo) as f32),
        ]);
        let error = skin_difference(&mine, &dump_skin(&tights.mesh, &dump.bones, v));
        if error > worst.0 {
            worst = (error, v);
        }
    }
    println!(
        "tights, {} vertices: worst per-weight error {:.2e} (vertex {})",
        tights.mesh.positions.len(),
        worst.0,
        worst.1
    );
    assert!(worst.0 <= DUMP_TOLERANCE, "vertex {}: {:e}", worst.1, worst.0);
}

#[test]
fn the_hands_wrist_blend_matches_the_dump() {
    let Some(js) = JsRef::open() else { return };
    let dump = js.geometry("game").unwrap();
    let hands = dump.mesh("hands").expect("the dump has the hands");
    // body.js `handFrame` and `Wh`: the hand frame starts 0.022 m past the wrist along the forearm (uniform-dims.js `ARM`),
    // and the wrist end hands up to half its weight to the forearm.
    let (elbow, wrist) = (DVec3::new(0.155, 0.38, -0.01), DVec3::new(0.20, 0.29, 0.01));
    let along = (wrist - elbow).normalize();
    let origin = wrist + along * 0.022;
    let mut worst = (0.0_f32, 0);
    for (v, p) in hands.mesh.positions.iter().enumerate() {
        let side = if p.x >= 0.0 { "L" } else { "R" };
        let left_side = DVec3::new(f64::from(p.x.abs()), f64::from(p.y), f64::from(p.z)); // the right hand is the left's mirror
        let u = (left_side - origin).dot(along);
        let (hand, forearm) = (joint(&format!("hand.{side}")), joint(&format!("lower_arm.{side}")));
        let share = 0.5 * blend_between(hand.clone(), forearm.clone(), -0.004, -0.014).amount(u);
        let mine = finished(vec![(hand, (1.0 - share) as f32), (forearm, share as f32)]);
        let error = skin_difference(&mine, &dump_skin(&hands.mesh, &dump.bones, v));
        if error > worst.0 {
            worst = (error, v);
        }
    }
    println!(
        "hands, {} vertices: worst per-weight error {:.2e} (vertex {})",
        hands.mesh.positions.len(),
        worst.0,
        worst.1
    );
    assert!(worst.0 <= DUMP_TOLERANCE, "vertex {}: {:e}", worst.1, worst.0);
}
