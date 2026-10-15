//! Parity of the hair tube geometry (`omuzaki_char::hair::tube`) with the prototype's `tubeInto`, `tubeIndex` and
//! `outwardRest` (`parts/hair.js`), and of the legacy random stream with its `rnd()`.
//!
//! The oracle is a committed fixture (`fixtures/char_hair_tubes/hair_tubes.json`, made by the prototype's generator script, which cuts the
//! prototype's own functions out of `hair.js` and runs them under Node with three.js): five synthetic polylines (a
//! fringe, a side lock round the head, a long lock leaving the head's blend zone, a lock whose tangent is parallel to
//! outward, and the two-point minimum) with the prototype's width and thickness functions, and 10 000 draws of the
//! Park-Miller stream from seed 7. No character data; runs everywhere.
//!
//! **Tolerance (T-GEOM).** The prototype computes in `f64` and stores `f32`; this port computes in `f32`. Positions agree
//! within 2e-4 m (measured far inside that, see the assertion messages), normals within 1 degree, `hair-uv` and
//! `hair-info` within 1e-5, the index list exactly.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::chunks_exact_to_as_chunks)] // f64 fixture values to the mesh's f32

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use omuzaki_char::hair::tube::{OutwardFrame, RING, TubeLook, TubeMesh, polar_angles};
use omuzaki_char_api::{Bulge, HAIR_INFO, HAIR_UV, HairRng, LegacyLcg, Taper};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_core::rng::Rng;
use serde::Deserialize;

/// Position tolerance, metres.
const POSITION: f32 = 2e-4;
/// Normal tolerance, degrees.
const NORMAL_DEG: f32 = 1.0;
/// Attribute tolerance.
const ATTRIBUTE: f32 = 1e-5;
/// The prototype's body axis `z` (`lerp(-0.03, C.z, tH)` at `tH = 0`).
const BODY_AXIS_Z: f32 = -0.03;

#[derive(Deserialize)]
struct Case {
    points: Vec<[f64; 3]>,
    taper: Spec4,
    bulge: Spec2,
    seed: f32,
    shade: f32,
    outward: Vec<[f64; 3]>,
    positions: Vec<f32>,
    normals: Vec<f32>,
    uv: Vec<f32>,
    info: Vec<f32>,
    indices: Vec<u32>,
}

#[derive(Deserialize)]
struct Spec4 {
    start: f64,
    mid: f64,
    tip: f64,
    tip_at: f64,
}

#[derive(Deserialize)]
struct Spec2 {
    base: f64,
    swell: f64,
}

#[derive(Deserialize)]
struct Fixture {
    #[serde(rename = "K")]
    ring: usize,
    head_centre: [f32; 3],
    stream_seed7: Vec<f64>,
    cases: BTreeMap<String, Case>,
}

fn fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/char_hair_tubes/hair_tubes.json");
    serde_json::from_str(&fs::read_to_string(path).expect("the committed fixture")).expect("a well-formed fixture")
}

fn vec3(v: [f64; 3]) -> Vec3 {
    Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32)
}

fn frame(f: &Fixture) -> OutwardFrame {
    OutwardFrame { head: Vec3::from_array(f.head_centre), body_axis: Vec2::new(0.0, BODY_AXIS_Z) }
}

fn build(f: &Fixture, case: &Case) -> MeshData {
    let points: Vec<Vec3> = case.points.iter().copied().map(vec3).collect();
    let taper = Taper { start: case.taper.start, mid: case.taper.mid, tip: case.taper.tip, tip_at: case.taper.tip_at };
    let bulge = Bulge { base: case.bulge.base, swell: case.bulge.swell };
    let frame = frame(f);
    let polar = polar_angles(&points, frame.head);
    let look = TubeLook { seed: case.seed, shade: case.shade, polar: &polar, tint: None, flip: 1.0 };
    let mut mesh = TubeMesh::new();
    mesh.push(&points, &taper, &bulge, |p| frame.rest(p), &look).unwrap();
    mesh.finish().unwrap()
}

#[test]
fn the_ring_size_is_the_prototypes() {
    assert_eq!(fixture().ring, RING);
}

#[test]
fn tube_vertices_match_tube_into() {
    let f = fixture();
    for (name, case) in &f.cases {
        let mesh = build(&f, case);
        assert_eq!(mesh.positions.len() * 3, case.positions.len(), "{name}: vertex count");
        let worst = mesh
            .positions
            .iter()
            .zip(case.positions.chunks_exact(3))
            .map(|(p, q)| p.distance(Vec3::new(q[0], q[1], q[2])))
            .fold(0.0_f32, f32::max);
        assert!(worst <= POSITION, "{name}: worst position error {worst} m");
        eprintln!("{name}: positions within {worst:.2e} m");
    }
}

#[test]
fn tube_normals_match_within_a_degree() {
    let f = fixture();
    for (name, case) in &f.cases {
        let mesh = build(&f, case);
        let worst = mesh
            .normals
            .iter()
            .zip(case.normals.chunks_exact(3))
            .map(|(n, q)| n.angle_between(Vec3::new(q[0], q[1], q[2])).to_degrees())
            .fold(0.0_f32, f32::max);
        assert!(worst <= NORMAL_DEG, "{name}: worst normal error {worst} degrees");
        eprintln!("{name}: normals within {worst:.3} degrees");
    }
}

#[test]
fn hair_uv_and_hair_info_match() {
    let f = fixture();
    for (name, case) in &f.cases {
        let mesh = build(&f, case);
        for (attribute, expected, label) in [(HAIR_UV, &case.uv, "hair-uv"), (HAIR_INFO, &case.info, "hair-info")] {
            let got = &mesh.extra_attribute(attribute).unwrap().data;
            assert_eq!(got.len(), expected.len(), "{name}: {label} length");
            let worst = got.iter().zip(expected).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
            assert!(worst <= ATTRIBUTE, "{name}: {label} differs by {worst}");
        }
    }
}

#[test]
fn the_index_list_is_tube_index_exactly() {
    let f = fixture();
    for (name, case) in &f.cases {
        assert_eq!(build(&f, case).indices, case.indices, "{name}");
    }
}

#[test]
fn outward_rest_matches_at_every_point() {
    let f = fixture();
    let frame = frame(&f);
    for (name, case) in &f.cases {
        for (p, o) in case.points.iter().zip(&case.outward) {
            let got = frame.rest(vec3(*p));
            assert!(got.angle_between(vec3(*o)).to_degrees() <= NORMAL_DEG, "{name}: {got} vs {o:?}");
        }
    }
}

#[test]
fn the_legacy_stream_equals_the_prototypes_rnd_for_ten_thousand_draws() {
    let f = fixture();
    assert_eq!(f.stream_seed7.len(), 10_000);
    let mut lcg = LegacyLcg::new(7).expect("7 is a valid seed");
    for (i, expected) in f.stream_seed7.iter().enumerate() {
        assert_eq!(lcg.next_unit(), *expected, "draw {i}");
    }
}

#[test]
fn the_pipeline_draws_the_same_stream_in_legacy_mode() {
    // `compile::stream` is what every generator's `ctx.rnd()` reads: in parity mode it is that very stream.
    let f = fixture();
    let mut unused = Rng::new(1);
    let mut stream = omuzaki_char::hair::compile::stream(HairRng::LegacyLcg { seed: 7 }, &mut unused).unwrap();
    for (i, expected) in f.stream_seed7.iter().enumerate() {
        assert_eq!(stream.next_unit(), *expected, "draw {i}");
    }
}
