//! Parity of the `skirt` garment (`omuzaki_char::outfit::skirt`, #192) with the prototype's knife-pleated skirt
//! (`uniform.js` `buildSkirt`), as #192's reference gives it.
//!
//! The oracle is the sample character's dump (`geometry/game/skirt` and `skirt-band`) and the profile table it was built
//! with (`derived/skirt-profile.json` next to the dumps: the prototype's three envelope curves, rewritten as knots against
//! the fraction of the way from the hem to the waist). Both are private test material kept in the local assets folder:
//! without them (always, in CI and in the cloud) every test here skips with a message.
//!
//! The garment is built with that table as its explicit profile, on the soft-sd template, with the waist and hem the
//! prototype's. Then the skirt is the prototype's: 12 rows x 145 columns twice and the roll for the pleats mesh, two band
//! tubes for the band mesh, and every vertex is compared with its counterpart: position, normal, uv, vertex colour and skin
//! weights, and the triangle indices. The plaid itself is painted by a rasteriser the character area's cross-crate tests
//! don't have; its pixels are checked in `omuzaki-char/tests/outfit_skirt.rs`, so the build here uses a plain cloth.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic on small counts

#[path = "support/outfit_conformance.rs"]
mod conformance;

use std::collections::BTreeMap;

use omuzaki_char::kit::js_names;
use omuzaki_char::outfit::skirt::{
    LAYER_SKIRT, PROTOTYPE_BLEND, PROTOTYPE_REACH, SkirtParams, SkirtProfile, ThighPull,
};
use omuzaki_char_api::{BodyShape, BuildServices, JointRef};
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;
use serde::Deserialize;

/// How far a vertex may be from the prototype's, metres.
const POSITION: f64 = 2e-5;
/// How far a normal may be from the prototype's, as the angle between them, radians.
const NORMAL: f64 = 1e-3;
/// How far a texture coordinate or a vertex colour may be from the prototype's.
const UV: f64 = 1e-4;
const COLOUR: f32 = 1e-5;
/// How far a skin weight may be from the prototype's.
const WEIGHT: f32 = 1e-3;

#[derive(Deserialize)]
struct Table {
    top: f64,
    hem: f64,
    profile: Profile,
}

#[derive(Deserialize)]
struct Profile {
    rx: Vec<[f64; 2]>,
    rz: Vec<[f64; 2]>,
    cz: Vec<[f64; 2]>,
}

struct Oracle {
    pleats: MeshData,
    band: MeshData,
    bones: Vec<JointRef>,
    table: Table,
}

/// The dump writes a side as `upper_legL`; the prototype's code, and the name map, say `upper_leg.L`.
fn dotted(name: &str) -> String {
    for side in ["L", "R"] {
        if let Some(stem) = name.strip_suffix(side).filter(|s| s.ends_with(|c: char| c.is_ascii_lowercase())) {
            return format!("{stem}.{side}");
        }
    }
    name.to_owned()
}

fn oracle() -> Option<Oracle> {
    let js = JsRef::open()?;
    let path = js.root().join("derived/skirt-profile.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipping: no {}", path.display());
        return None;
    };
    let geometry = js.geometry("game").expect("the geometry dump loads");
    let bones = geometry.bones.iter().map(|n| js_names::joint(&dotted(n)).unwrap()).collect();
    Some(Oracle {
        pleats: geometry.mesh("skirt").expect("the dump has the skirt").mesh.clone(),
        band: geometry.mesh("skirt-band").expect("the dump has the band").mesh.clone(),
        bones,
        table: serde_json::from_str(&text).expect("the table parses"),
    })
}

fn ours(table: &Table) -> Vec<omuzaki_char_api::PartMesh> {
    let params = SkirtParams {
        // The prototype's waist and hem, as fractions of this body's torso and thigh (soft-sd reproduces its body).
        top: (table.top - 0.325) / (0.545 - 0.325),
        hem: (0.38 - table.hem) / (0.38 - 0.19),
        profile: Some(SkirtProfile {
            rx: table.profile.rx.clone(),
            rz: table.profile.rz.clone(),
            cz: table.profile.cz.clone(),
        }),
        plaid: None,
        // The prototype's cap: a thigh carries at most 95 % of a vertex.
        thigh_cap: 0.95,
        thigh_reach: PROTOTYPE_REACH,
        hip_blend: PROTOTYPE_BLEND,
        thigh_pull: ThighPull::Falloff,
        ..SkirtParams::default()
    };
    let value = serde_json::to_value(&params).unwrap();
    let def = conformance::character("soft-sd", &BodyShape::default(), &[("skirt", "skirt", LAYER_SKIRT, value)]);
    let stack = conformance::build(def, &BuildServices::default()).expect("the skirt builds");
    stack.output("skirt").meshes.clone()
}

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

/// The worst gaps between two meshes of the same layout, vertex for vertex.
#[derive(Debug, Default)]
struct Gaps {
    position: f64,
    normal: f64,
    uv: f64,
    colour: f32,
    weight: f32,
}

fn compare(
    label: &str,
    mine: &omuzaki_char_api::PartMesh,
    theirs: &MeshData,
    bones: &[JointRef],
    coloured: bool,
) -> Gaps {
    let g = &mine.geometry;
    assert_eq!(g.positions.len(), theirs.positions.len(), "{label}: vertex count");
    assert_eq!(g.indices.len(), theirs.indices.len(), "{label}: index count");
    let mut gaps = Gaps::default();
    for v in 0..g.positions.len() {
        gaps.position = gaps.position.max((g.positions[v].as_dvec3() - theirs.positions[v].as_dvec3()).length());
        let (a, b) = (g.normals[v].as_dvec3(), theirs.normals[v].as_dvec3());
        gaps.normal = gaps.normal.max(a.dot(b).clamp(-1.0, 1.0).acos());
        gaps.uv = gaps.uv.max((g.uvs[v].as_dvec2() - theirs.uvs[v].as_dvec2()).abs().max_element());
        if coloured {
            for c in 0..3 {
                gaps.colour = gaps.colour.max((g.colours[v][c] - theirs.colours[v][c]).abs());
            }
        }
        let gap = weight_gap(&skin(g, &mine.joint_bones, v), &skin(theirs, bones, v));
        gaps.weight = gaps.weight.max(gap);
    }
    eprintln!("{label}: {gaps:?}");
    gaps
}

#[test]
fn the_pleats_are_the_prototypes_vertex_for_vertex() {
    let Some(o) = oracle() else {
        eprintln!("skipping: no JS dump (set OMUZAKI_ASSETS to the assets folder)");
        return;
    };
    let built = ours(&o.table);
    let gaps = compare("skirt pleats", &built[0], &o.pleats, &o.bones, true);
    assert_eq!(built[0].geometry.indices, o.pleats.indices, "the triangles are the prototype's");
    assert!(gaps.position <= POSITION, "positions: {:.2e} m", gaps.position);
    assert!(gaps.normal <= NORMAL, "normals: {:.2e} rad", gaps.normal);
    assert!(gaps.uv <= UV, "uvs: {:.2e}", gaps.uv);
    assert!(gaps.colour <= COLOUR, "colours: {:.2e}", gaps.colour);
    assert!(gaps.weight <= WEIGHT, "weights: {:.2e}", gaps.weight);
}

#[test]
fn the_band_lines_are_the_prototypes_vertex_for_vertex() {
    let Some(o) = oracle() else {
        eprintln!("skipping: no JS dump (set OMUZAKI_ASSETS to the assets folder)");
        return;
    };
    let built = ours(&o.table);
    let gaps = compare("skirt band", &built[1], &o.band, &o.bones, false);
    assert_eq!(built[1].geometry.indices, o.band.indices, "the triangles are the prototype's");
    assert!(gaps.position <= POSITION, "positions: {:.2e} m", gaps.position);
    assert!(gaps.normal <= NORMAL, "normals: {:.2e} rad", gaps.normal);
    assert!(gaps.weight <= WEIGHT, "weights: {:.2e}", gaps.weight);
}
