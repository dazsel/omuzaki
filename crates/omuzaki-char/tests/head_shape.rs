//! Behaviour of the head cranium, face-shape outline and chin stretch on synthetic parameters: the mesh is closed with
//! outward normals, the chin lands on the plausible outline's chin, the surface is the unreshaped cranium, a blend
//! between two face shapes moves the mesh in proportion to the blend, and a Live build is cheap. The head-quality
//! metrics over the face-shape space are in `tests/head_quality`. Parity with the JS prototype is in
//! `crates/omuzaki-test/tests/char_head_analytic_shape_parity.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // small loop counters and metres to millimetres

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use omuzaki_char::head::cranium;
use omuzaki_char::head::mesh::{HeadMeshError, Resolution, build};
use omuzaki_char::head::outline::{MAX_BAND, plausible};
use omuzaki_char::head::{TraceSample, surface};
use omuzaki_char_api::{Chin, Cranium, HeadOutline, HeadParams, TracedHead};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::Validate;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};

// ------------------------------------------------------------------------------------------------ generators

fn arb_cranium() -> impl Strategy<Value = Cranium> {
    let extent = 0.05..0.4_f64;
    (
        (0.3..1.3_f64, -0.1..0.1_f64),
        (extent.clone(), extent.clone(), extent.clone(), extent.clone(), extent.clone(), extent),
        (0.0..0.05_f64, 0.0..0.2_f64),
    )
        .prop_map(|((y, z), (w, df, db, up, dnb, dnf), (cheek, flatten))| Cranium {
            centre: Some([0.0, y, z]),
            half_width: w,
            half_depth_front: df,
            half_depth_back: db,
            half_height_up: up,
            half_height_down_back: dnb,
            half_height_down_front: dnf,
            cheek_fullness: cheek,
            face_flatten: flatten,
        })
}

/// An outline over the whole valid range, with a chin about half the time (a chin that may well sit above the head's
/// lowest point: then the face is left unstretched).
fn arb_outline() -> impl Strategy<Value = HeadOutline> {
    let chin = (-0.5..-0.01_f64, 0.01..0.6_f64, 0.2..1.0_f64).prop_map(|(y, gap, min_ratio)| Chin {
        y,
        stretch_top: (y + gap).min(0.2),
        min_ratio,
    });
    (0.05..0.4_f64, -0.3..0.2_f64, 0.02..0.4_f64, 1.0..4.0_f64, proptest::option::of(chin), 0.0..0.05_f64).prop_map(
        |(half_width, top, height, exponent, chin, cheek_puff)| HeadOutline {
            half_width,
            top,
            height,
            exponent,
            exponent_lower: None,
            chin,
            cheek_puff,
        },
    )
}

/// A cranium within 25% of the default's extents.
fn arb_real_cranium() -> impl Strategy<Value = Cranium> {
    let d = Cranium::default();
    let scale = 0.8..1.25_f64;
    (scale.clone(), scale.clone(), scale.clone(), scale.clone(), scale.clone(), scale, 0.0..0.03_f64).prop_map(
        move |(w, df, db, up, dnb, dnf, cheek)| Cranium {
            half_width: d.half_width * w,
            half_depth_front: d.half_depth_front * df,
            half_depth_back: d.half_depth_back * db,
            half_height_up: d.half_height_up * up,
            half_height_down_back: d.half_height_down_back * dnb,
            half_height_down_front: d.half_height_down_front * dnf,
            cheek_fullness: cheek,
            ..d.clone()
        },
    )
}

fn arb_params() -> impl Strategy<Value = HeadParams> {
    (arb_cranium(), arb_outline()).prop_map(|(cranium, outline)| HeadParams {
        cranium,
        outline,
        ..HeadParams::default()
    })
}

fn live(params: &HeadParams) -> MeshData {
    build(params, Resolution::LIVE, None).unwrap()
}

fn lowest_local(mesh: &MeshData, params: &HeadParams) -> f64 {
    f64::from(mesh.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min)) - params.cranium.centre()[1]
}

/// Deterministic unless `PROPTEST_RNG_SEED` is set, and without regression files: a failure found by exploring
/// (`PROPTEST_RNG_SEED=7 PROPTEST_CASES=5000 cargo test -p omuzaki-char --test head_shape`) becomes an ordinary test.
/// `PROPTEST_CASES` overrides `cases` too.
fn config(cases: u32) -> Config {
    let mut config = Config { failure_persistence: None, ..Config::with_cases(cases) };
    if matches!(config.rng_seed, RngSeed::Random) {
        config.rng_seed = RngSeed::Fixed(0x5EED);
    }
    config
}

// ------------------------------------------------------------------------------------------------ closed, outward

/// Asserts that `mesh` is a closed, consistently oriented surface wound counter-clockwise from outside: welding vertices
/// that coincide, every directed edge of a real (non-degenerate) triangle has exactly one opposite edge; the enclosed
/// volume is positive; and every vertex normal is unit length.
fn assert_closed_and_wound_outward(mesh: &MeshData) {
    let key = |p: Vec3| [p.x, p.y, p.z].map(|v| (f64::from(v) * 1e5).round() as i64);
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let welded: Vec<u32> = mesh
        .positions
        .iter()
        .map(|p| {
            let next = ids.len() as u32;
            *ids.entry(key(*p)).or_insert(next)
        })
        .collect();
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    let mut volume = 0.0_f64;
    for tri in mesh.indices.as_chunks::<3>().0 {
        let p = tri.map(|i| mesh.positions[i as usize].as_dvec3());
        volume += p[0].dot(p[1].cross(p[2])) / 6.0;
        let w = tri.map(|i| welded[i as usize]);
        if w[0] == w[1] || w[1] == w[2] || w[2] == w[0] {
            continue; // the collapsed cells at the poles
        }
        for (a, b) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    for (&(a, b), &count) in &edges {
        assert_eq!(count, 1, "edge {a}->{b} is used {count} times in the same direction");
        assert_eq!(edges.get(&(b, a)), Some(&1), "edge {a}->{b} has no opposite: the surface has a hole");
    }
    assert!(volume > 0.0, "enclosed volume {volume} is not positive: the triangles wind inward");
    for (i, n) in mesh.normals.iter().enumerate() {
        assert!((n.length() - 1.0).abs() < 1e-4, "normal {i} has length {}", n.length());
    }
}

/// Every vertex normal points away from the head centre: the reshaped lower face has no ledge, fold or overhang that
/// faces back toward the centre.
fn assert_normals_point_outward(mesh: &MeshData, params: &HeadParams) {
    let centre = DVec3::from_array(params.cranium.centre());
    for (i, (p, n)) in mesh.positions.iter().zip(&mesh.normals).enumerate() {
        let outward = n.as_dvec3().dot(p.as_dvec3() - centre);
        assert!(outward > 0.0, "vertex {i}: normal {n:?} at {p:?} points inward ({outward:e})");
    }
}

proptest! {
    #![proptest_config(config(48))]

    #[test]
    fn any_valid_head_is_closed_and_wound_outward(params in arb_params()) {
        prop_assume!(params.validate().is_ok());
        assert_closed_and_wound_outward(&live(&params));
    }

    #[test]
    fn a_head_on_a_real_cranium_has_outward_normals_everywhere(outline in arb_outline(), cranium in arb_real_cranium()) {
        let params = HeadParams { cranium, outline, ..HeadParams::default() };
        prop_assume!(params.validate().is_ok());
        let mesh = live(&params);
        assert_closed_and_wound_outward(&mesh);
        assert_normals_point_outward(&mesh, &params);
    }

    /// On a cranium whose lowest point is its chin (not one whose back hangs lower than its front), the head ends on the
    /// plausible outline's chin, wherever the outline asked for it.
    #[test]
    fn the_lowest_vertex_is_the_plausible_outlines_chin(outline in arb_outline(), cranium in arb_real_cranium()) {
        let params = HeadParams { cranium, outline, ..HeadParams::default() };
        prop_assume!(params.validate().is_ok());
        let chin = plausible(&params.outline, &params.cranium).chin_y();
        let lowest = lowest_local(&live(&params), &params);
        prop_assert!((lowest - chin).abs() < 1e-4, "lowest {lowest}, chin {chin}");
    }

    #[test]
    fn the_surface_is_the_cranium_whatever_the_outline_does(params in arb_params(), x in -1.0..1.0_f32, y in -1.0..1.0_f32, z in -1.0..1.0_f32) {
        prop_assume!(params.validate().is_ok());
        let dir = Vec3::new(x, y, z);
        prop_assume!(dir.length() > 0.05);
        let s = surface(&params, None);
        let r = s.radius(dir);
        let c = &params.cranium;
        let widest = [c.half_width + c.cheek_fullness, c.half_depth_front, c.half_depth_back, c.half_height_up, c.half_height_down_front, c.half_height_down_back].into_iter().fold(0.0, f64::max);
        let narrowest = [c.half_width, c.half_depth_front, c.half_depth_back, c.half_height_up, c.half_height_down_front, c.half_height_down_back].into_iter().fold(f64::INFINITY, f64::min);
        prop_assert!(f64::from(r) <= widest + 1e-6 && f64::from(r) >= narrowest * (1.0 - c.face_flatten) - 1e-6, "{r} outside [{narrowest}, {widest}]");
        prop_assert!((f64::from(r) - cranium::radius(c, dir.as_dvec3().normalize())).abs() < 1e-6);
        // The cranium never follows the outline; what sits on the head anchors on the head as built (`build_head`).
        let other = HeadParams { outline: HeadOutline { half_width: 0.31, chin: None, cheek_puff: 0.03, ..params.outline }, ..params.clone() };
        prop_assert!(surface(&other, None).radius(dir).to_bits() == r.to_bits(), "the surface moved with the outline");
    }
}

// ------------------------------------------------------------------------------------------------ blending

/// The largest distance any vertex moves between two meshes of the same topology, in height and sideways (x or z).
fn max_step(a: &MeshData, b: &MeshData) -> (f64, f64) {
    a.positions.iter().zip(&b.positions).fold((0.0, 0.0), |(high, side), (p, q)| {
        let d = (*p - *q).abs();
        (high.max(f64::from(d.y)), side.max(f64::from(d.x.max(d.z))))
    })
}

/// The largest vertex step, in height and sideways, as the outline blends from `a` to `b` in `steps` equal steps of
/// `HeadOutline::lerp`.
fn blend_steps(a: &HeadOutline, b: &HeadOutline, steps: u32) -> (f64, f64) {
    let mut worst = (0.0_f64, 0.0_f64);
    let mut previous = None;
    for k in 0..=steps {
        let params = HeadParams { outline: a.lerp(b, f64::from(k) / f64::from(steps)), ..HeadParams::default() };
        let mesh = live(&params);
        if let Some(before) = &previous {
            let (high, side) = max_step(&mesh, before);
            worst = (worst.0.max(high), worst.1.max(side));
        }
        previous = Some(mesh);
    }
    worst
}

/// Face shapes in the ranges the presets (round-u, egg-oval, mochi, heart, long) live in, with or without a chin.
fn arb_pair_outline(with_chin: bool) -> impl Strategy<Value = HeadOutline> {
    let chin = (-0.26..-0.2_f64, -0.09..-0.06_f64).prop_map(|(y, stretch_top)| Chin { y, stretch_top, min_ratio: 0.6 });
    (0.13..0.17_f64, -0.12..-0.08_f64, 0.08..0.12_f64, 1.6..2.2_f64, 0.0..0.02_f64, chin).prop_map(
        move |(half_width, top, height, exponent, cheek_puff, chin)| HeadOutline {
            half_width,
            top,
            height,
            exponent,
            exponent_lower: None,
            chin: with_chin.then_some(chin),
            cheek_puff,
        },
    )
}

// Every step of the lower face is continuous in the parameters (no width bins, no hard switches), so a blend between
// two face shapes moves each vertex in proportion to the blend step: cutting a blend four times finer cuts its
// largest vertex step about four times. The prototype's 2 mm bins made a stretched chin jitter by up to 2 cm per 1/16
// step whatever the step size.
proptest! {
    #![proptest_config(config(12))]

    /// Width and cheek puff blend linearly: no vertex moves more than 2 x the blend step x the larger parameter change.
    #[test]
    fn blending_width_and_puff_moves_each_vertex_at_most_twice_the_parameter_step(
        shape in arb_pair_outline(false),
        (hw_a, hw_b) in (0.14..0.16_f64, 0.14..0.16_f64),
        (puff_a, puff_b) in (0.0..0.01_f64, 0.0..0.01_f64),
    ) {
        let a = HeadOutline { half_width: hw_a, cheek_puff: puff_a, ..shape };
        let b = HeadOutline { half_width: hw_b, cheek_puff: puff_b, ..shape };
        let steps = 16;
        let (high, side) = blend_steps(&a, &b, steps);
        let bound = 2.0 * (hw_a - hw_b).abs().max((puff_a - puff_b).abs()) / f64::from(steps) + 1e-6;
        prop_assert!(high <= 1e-6, "only the width and the puff change, so no height does: {high:e}");
        prop_assert!(side <= bound, "sideways step {side:.2e} against the bound {bound:.2e}");
    }

    /// Any two plausible face shapes, with explicit chins or implicit ones, blend in proportion: the largest vertex
    /// step of a 64-step blend is about a quarter of a 16-step blend's (a jump keeps it near 1). The ends are first
    /// clamped to the plausible space, as every build does. A blend of two raw outlines can run into a clamp part-way,
    /// where the shape's speed changes and the ratio says nothing about jumps. The clamps' own continuity is tested
    /// in `head::outline`.
    #[test]
    fn blending_any_two_face_shapes_moves_vertices_in_proportion_to_the_step(
        with_chin in any::<bool>(),
        (a, b) in (arb_pair_outline(true), arb_pair_outline(true)),
    ) {
        let (a, b) = if with_chin { (a, b) } else { (HeadOutline { chin: None, ..a }, HeadOutline { chin: None, ..b }) };
        let cranium = Cranium::default();
        let (a, b) = (plausible(&a, &cranium), plausible(&b, &cranium));
        let coarse = blend_steps(&a, &b, 16);
        let fine = blend_steps(&a, &b, 64);
        let (coarse, fine) = (coarse.0.max(coarse.1), fine.0.max(fine.1));
        prop_assert!(fine <= 0.35 * coarse + 1e-6, "largest step {coarse:.2e} at 16 steps, {fine:.2e} at 64");
    }
}

/// The pop found: two chinless outlines whose blend jumped by about 1 cm however finely it was cut.
#[test]
fn the_chinless_blend_of_1272_has_no_pop() {
    let a = HeadOutline {
        half_width: 0.169_018_575_607_020_35,
        top: -0.089_669_353_125_719_78,
        height: 0.089_213_496_921_877_01,
        exponent: 2.158_230_690_793_918_6,
        exponent_lower: None,
        chin: None,
        cheek_puff: 0.0,
    };
    let b = HeadOutline {
        half_width: 0.13,
        top: -0.109_450_828_117_683_81,
        height: 0.115_237_720_350_093_35,
        exponent: 1.6,
        exponent_lower: None,
        chin: None,
        cheek_puff: 0.0,
    };
    let cranium = Cranium::default();
    // Raw (each blend step is clamped by the build, as a slider drag is) and clamped at the ends first.
    for (a, b) in [(a, b), (plausible(&a, &cranium), plausible(&b, &cranium))] {
        let coarse = blend_steps(&a, &b, 8);
        let fine = blend_steps(&a, &b, 64);
        eprintln!("1272 blend: largest step {coarse:?} at 8 steps, {fine:?} at 64");
        let (coarse, fine) = (coarse.0.max(coarse.1), fine.0.max(fine.1));
        assert!(fine <= 0.35 * coarse + 1e-6, "largest step {coarse:.2e} at 8 steps, {fine:.2e} at 64");
    }
}

// ------------------------------------------------------------------------------------------------ the rest

#[derive(Debug)]
struct Sphere(f32);

impl TraceSample for Sphere {
    fn sample(&self, _: Vec3) -> (f32, f32) {
        (self.0, 1.0)
    }
}

fn traced(mix: f64) -> HeadParams {
    let traced =
        TracedHead { asset: omuzaki_store_api::ContentHash::of(b"unused"), theta_steps: 64, phi_steps: 128, mix };
    HeadParams { traced: Some(traced), ..HeadParams::default() }
}

const CENTRE: Vec3 = Vec3::new(0.0, 0.78, -0.02);

/// The distance from the head centre of every vertex, or `None` for those in the reshaped lower face (below the stretch
/// line and the tallest blend band above the outline's top): above it the mesh is the cranium, or its trace, itself.
fn cranium_distances(mesh: &MeshData) -> Vec<Option<f32>> {
    let params = traced(1.0);
    let outline = plausible(&params.outline, &params.cranium);
    let reshaped_below = outline.chin.map_or(0.0, |c| c.stretch_top).max(outline.top + MAX_BAND);
    let limit = (f64::from(CENTRE.y) + reshaped_below) as f32 + 1e-4;
    let distances: Vec<Option<f32>> =
        mesh.positions.iter().map(|p| (p.y > limit).then(|| (*p - CENTRE).length())).collect();
    let above = distances.iter().flatten().count();
    assert!(above > mesh.positions.len() / 2, "{above} of {} vertices", mesh.positions.len());
    distances
}

#[test]
fn a_full_mix_trace_replaces_the_cranium() {
    let mesh = build(&traced(1.0), Resolution::LIVE, Some(&Sphere(0.25))).unwrap();
    for (i, d) in cranium_distances(&mesh).into_iter().enumerate() {
        if let Some(d) = d {
            assert!((d - 0.25).abs() < 2e-6, "vertex {i} is {d} from the centre");
        }
    }
}

#[test]
fn a_zero_mix_is_the_analytic_mesh_and_a_half_mix_is_halfway() {
    let analytic = live(&traced(0.0));
    assert_eq!(
        build(&traced(0.0), Resolution::LIVE, Some(&Sphere(0.25))).unwrap(),
        analytic,
        "mix 0 ignores the trace"
    );
    let half = build(&traced(0.5), Resolution::LIVE, Some(&Sphere(0.25))).unwrap();
    for (i, (h, a)) in cranium_distances(&half).into_iter().zip(cranium_distances(&analytic)).enumerate() {
        if let (Some(h), Some(a)) = (h, a) {
            assert!((h - f32::midpoint(a, 0.25)).abs() < 2e-6, "vertex {i}: {h} against analytic {a}");
        }
    }
}

#[test]
fn the_published_surface_blends_the_trace_the_same_way() {
    let s = surface(&traced(1.0), Some(Arc::new(Sphere(0.25))));
    assert!((s.radius(Vec3::X) - 0.25).abs() < 1e-6);
    let half = surface(&traced(0.5), Some(Arc::new(Sphere(0.25))));
    let analytic = surface(&traced(0.5), None).radius(Vec3::X);
    assert!((half.radius(Vec3::X) - f32::midpoint(analytic, 0.25)).abs() < 1e-6);
}

#[test]
fn a_build_is_deterministic_and_resolutions_scale() {
    let params = HeadParams {
        outline: HeadOutline { chin: Some(Chin::default()), cheek_puff: 0.01, ..HeadOutline::default() },
        ..HeadParams::default()
    };
    assert_eq!(live(&params), live(&params));
    let offline = build(&params, Resolution::OFFLINE, None).unwrap();
    assert_eq!((offline.positions.len(), offline.triangle_count()), (257 * 193, 256 * 192 * 2));
    assert_closed_and_wound_outward(&offline);
    assert_normals_point_outward(&offline, &params);
    let coarse = build(&params, Resolution { segments: 8, rings: 6 }, None).unwrap();
    assert_closed_and_wound_outward(&coarse);
    assert!(matches!(
        build(&params, Resolution { segments: 2, rings: 6 }, None),
        Err(HeadMeshError::Resolution { .. })
    ));
}

#[test]
fn a_live_build_with_a_chin_is_within_the_25_ms_budget() {
    let params = HeadParams {
        outline: HeadOutline { chin: Some(Chin::default()), cheek_puff: 0.01, ..HeadOutline::default() },
        ..HeadParams::default()
    };
    let best = (0..5)
        .map(|_| {
            let start = Instant::now();
            let mesh = live(&params);
            assert_eq!(mesh.positions.len(), 129 * 97);
            start.elapsed()
        })
        .min()
        .unwrap();
    eprintln!("Live head build (128 x 96, chin, puff): best of 5 {best:?}");
    // The budget is for an optimised build; the unoptimised test profile gets ten times the room.
    let budget_ms = if cfg!(debug_assertions) { 250 } else { 25 };
    assert!(best.as_millis() < budget_ms, "{best:?} against {budget_ms} ms");
}
