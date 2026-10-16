//! Parity of the `legwear` garment (`omuzaki_char::outfit::legwear`, #193) with the prototype's tights (`body.js`
//! `buildLeg`), as #193's reference gives them.
//!
//! The oracle is the sample character's dump (`geometry/game/tights`, private test material kept in the local assets
//! folder): without it (always, in CI and in the cloud) every test here skips with a message. The prototype's tights
//! *are* its legs and sit on the body curve, while the garment sits `ease` outside the body fit, so the garment is built
//! on the soft-sd template, whose layout reproduces the prototype's `LEG`, with the least ease the contract allows, and
//! each vertex is compared with the prototype's pushed out by that ease along the ring's radii. The rows go bottom to
//! top as the prototype's do, and each of the garment's body rows below the body's own hip blend is matched to the prototype's row at the same height. The
//! garment starts above the foot and stops at the torso's lower edge, so the prototype's cap under the ankle, its hip rows
//! and its dome (inside the foot and the torso) have no counterpart, nor do the cuff, the top edge and the finer rows near
//! the hip.
//!
//! Compared: the positions, the vertex colours (the spec: within 2/255) and the skin weights, left leg and right.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic on small counts

#[path = "support/outfit_conformance.rs"]
mod conformance;

use std::collections::BTreeMap;

use omuzaki_char::kit::js_names;
use omuzaki_char::outfit::common::GarmentCommon;
use omuzaki_char::outfit::legwear::{COLUMNS, LAYER_LEGWEAR, LegTop, LegwearParams, LegwearStyle, MIDPLANE_GAP};
use omuzaki_char_api::{BodyShape, BuildServices, JointRef};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;

/// The least ease a garment may have, metres: what the garment is built with so it lies closest to the prototype's legs.
const EASE: f64 = 0.001;
/// How far a position may be from the prototype's (plus the ease), metres.
const POSITION: f64 = 2e-6;
/// How far a vertex colour may be from the prototype's (the spec says 2/255).
const COLOUR: f32 = 2.0 / 255.0;
/// How far a skin weight may be from the prototype's.
const WEIGHT: f32 = 1e-3;
/// Above this height the body's leg is no longer the prototype's `LEG` curve: the body blends the leg into the hip there
/// (`template_soft_sd.rs`), so the garment follows the blend and the prototype's table is not what it fits. Measured on
/// soft-sd, the rows to 0.27 m agree to 1e-7 m and the first row above it is 0.1 mm off, growing to 4 mm at the torso edge.
const HIP_BLEND: f64 = 0.28;
/// The prototype's rows under the sole (`bot`), then its body rows, then its dome.
const CAP_ROWS: usize = 6;
const LEG_ROWS: usize = 49;

/// The dump writes a side as `upper_legL`; the prototype's code, and the name map, say `upper_leg.L`.
fn dotted(name: &str) -> String {
    for side in ["L", "R"] {
        if let Some(stem) = name.strip_suffix(side).filter(|s| s.ends_with(|c: char| c.is_ascii_lowercase())) {
            return format!("{stem}.{side}");
        }
    }
    name.to_owned()
}

fn dump() -> Option<(MeshData, Vec<String>)> {
    let js = JsRef::open()?;
    let geometry = js.geometry("game").expect("the geometry dump loads");
    let mesh = geometry.mesh("tights").expect("the dump has the tights").mesh.clone();
    Some((mesh, geometry.bones.clone()))
}

fn ours(top: Option<LegTop>) -> omuzaki_char_api::PartMesh {
    let params = LegwearParams {
        common: GarmentCommon { ease: EASE, ..GarmentCommon::default() },
        style: LegwearStyle::Tights,
        top,
        thickness: 0.0,
        ..LegwearParams::default()
    };
    let value = serde_json::to_value(&params).unwrap();
    let def = conformance::character("soft-sd", &BodyShape::default(), &[("legwear", "legwear", LAYER_LEGWEAR, value)]);
    let stack = conformance::build(def, &BuildServices::default()).expect("the legwear builds");
    stack.output("legwear").meshes[0].clone()
}

/// The skin of vertex `v` of `mesh` as joint -> weight, for the prototype's mesh via its bone list.
fn skin(mesh: &MeshData, bones: &[JointRef], v: usize) -> BTreeMap<String, f32> {
    let mut out = BTreeMap::new();
    for k in 0..4 {
        let w = mesh.weights[v][k];
        if w > 0.0 {
            *out.entry(format!("{:?}", bones[usize::from(mesh.joints[v][k])])).or_insert(0.0) += w;
        }
    }
    out
}

fn weight_gap(a: &BTreeMap<String, f32>, b: &BTreeMap<String, f32>) -> f32 {
    a.keys()
        .chain(b.keys())
        .map(|k| (a.get(k).copied().unwrap_or(0.0) - b.get(k).copied().unwrap_or(0.0)).abs())
        .fold(0.0, f32::max)
}

#[test]
fn the_legs_are_the_tights_row_for_row() {
    let Some((proto, proto_bones)) = dump() else {
        eprintln!("skipping: no JS dump (set OMUZAKI_ASSETS to the assets folder)");
        return;
    };
    assert_eq!(proto.positions.len(), 2 * LEG_ROWS * COLUMNS, "the dump's tights are two legs of 49 x 45");
    let mine = ours(Some(LegTop::Waist));
    let g = &mine.geometry;
    let per_leg = g.positions.len() / 2;
    let rows = per_leg / COLUMNS;
    assert!(rows > 20, "the garment has most of the body rows: {rows}");
    let proto_joints: Vec<JointRef> = proto_bones.iter().map(|n| js_names::joint(&dotted(n)).unwrap()).collect();
    let (mut worst_position, mut worst_colour, mut worst_weight) = (0.0_f64, 0.0_f32, 0.0_f32);
    let (mut matched, mut clamped) = (0, 0);
    for (leg, right) in [(0, false), (1, true)] {
        // The garment's rows between its cuff and its top edge, each against the prototype's row at its height.
        for i in 1..rows - 1 {
            let y = f64::from(g.positions[leg * per_leg + i * COLUMNS].y);
            if y > HIP_BLEND {
                continue;
            }
            let row = (CAP_ROWS..CAP_ROWS + 37)
                .min_by(|&a, &b| {
                    let gap =
                        |r: usize| (f64::from(proto.positions[leg * LEG_ROWS * COLUMNS + r * COLUMNS].y) - y).abs();
                    gap(a).total_cmp(&gap(b))
                })
                .unwrap();
            // The garment cuts the spans near the hip finer than the prototype's rows; those have no counterpart.
            if (f64::from(proto.positions[leg * LEG_ROWS * COLUMNS + row * COLUMNS].y) - y).abs() > 1e-6 {
                continue;
            }
            matched += 1;
            for j in 0..COLUMNS {
                // The right leg is the left mirrored, so its column `j` is ours at `44 - j`.
                let ours_j = if right { (COLUMNS - 1 - j) % (COLUMNS - 1) } else { j };
                let a = leg * per_leg + i * COLUMNS + ours_j;
                let b = leg * LEG_ROWS * COLUMNS + row * COLUMNS + j;
                // Where the legs are nearer each other than the ease, the tubes stay on their side of the midplane.
                if f64::from(g.positions[a].x).abs() <= MIDPLANE_GAP * 1.01 {
                    clamped += 1;
                    continue;
                }
                let phi = std::f64::consts::TAU * ours_j as f64 / (COLUMNS - 1) as f64;
                let expected = proto.positions[b].as_dvec3()
                    + omuzaki_core::glam::DVec3::new(EASE * phi.sin(), 0.0, EASE * phi.cos());
                let gap = (g.positions[a].as_dvec3() - expected).length();
                worst_position = worst_position.max(gap);
                worst_colour = worst_colour.max((g.colours[a][0] - proto.colours[b][0]).abs());
                let weights = skin(g, &mine.joint_bones, a);
                worst_weight = worst_weight.max(weight_gap(&weights, &skin(&proto, &proto_joints, b)));
            }
        }
    }
    let rows = matched / 2;
    eprintln!(
        "tights vs dump over {rows} body rows x 2 legs ({clamped} vertices on the midplane skipped): position {worst_position:.2e} m, colour {worst_colour:.4}, weight {worst_weight:.4}"
    );
    assert!(worst_position <= POSITION, "positions are {worst_position:.2e} m from the prototype's");
    assert!(worst_colour <= COLOUR, "vertex colours are {worst_colour} from the prototype's");
    assert!(worst_weight <= WEIGHT, "skin weights are {worst_weight} from the prototype's");
    let _ = Vec3::ZERO;
}
