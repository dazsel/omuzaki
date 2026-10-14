//! Geometry kit II: parity of `catmull`, `frames`, `sweep`, `tube`, `place` and `resample` with the prototype, and
//! properties of the same functions.
//!
//! The fixtures in `fixtures/kit/sweep_*.json` were produced by running the prototype's `uniform-geo.js` and `hair.js`
//! under Node with three.js 0.170.0 the prototype's generator script; these tests rebuild the same inputs and compare.
//! Tolerance: 1e-9 m (the issue asks for 1e-6 m on the Catmull-Rom points; the worst difference is printed with
//! `--nocapture` and sits near rounding noise). The properties are checked on random polylines.

#![allow(clippy::cast_precision_loss)] // small loop counters
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers load fixtures; a bad fixture should stop the test with its reason

use omuzaki_char::kit::KitError;
use omuzaki_char::kit::catmull::spaced_points;
use omuzaki_char::kit::resample::resample;
use omuzaki_char::kit::sweep::{DEFAULT_UP, Sweep, SweepOptions, frames, place, sweep, tube};
use omuzaki_core::glam::DVec3;
use proptest::prelude::*;
use serde::Deserialize;

/// How far a Rust result may be from the prototype's recorded one, in metres.
const PARITY: f64 = 1e-9;

fn vec3s(points: &[[f64; 3]]) -> Vec<DVec3> {
    points.iter().map(|p| DVec3::from_array(*p)).collect()
}

/// The largest distance between matching points of two equally long lists; panics if the lengths differ.
fn worst_distance(got: &[DVec3], want: &[DVec3]) -> f64 {
    assert_eq!(got.len(), want.len(), "point counts");
    got.iter().zip(want).map(|(g, w)| g.distance(*w)).fold(0.0, f64::max)
}

// ---------------------------------------------------------------------------------------------------------------
// Catmull-Rom

#[derive(Deserialize)]
struct CatmullRun {
    n: usize,
    points: Vec<[f64; 3]>,
}

#[derive(Deserialize)]
struct CatmullCase {
    name: String,
    points: Vec<[f64; 3]>,
    runs: Vec<CatmullRun>,
}

#[derive(Deserialize)]
struct CatmullFixture {
    cases: Vec<CatmullCase>,
}

#[test]
fn spaced_points_match_three_on_nine_point_sets_and_three_counts() {
    let f: CatmullFixture = serde_json::from_str(include_str!("fixtures/kit/sweep_catmull.json")).unwrap();
    let names: Vec<&str> = f.cases.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "straight",
            "planar arc",
            "helix",
            "duplicate points",
            "duplicate ends",
            "near-duplicate points",
            "hair-like chain",
            "two points",
            "three points"
        ]
    );
    let mut overall = 0.0_f64;
    for case in &f.cases {
        assert_eq!(case.runs.iter().map(|r| r.n).collect::<Vec<_>>(), [2, 14, 40], "{}", case.name);
        let pts = vec3s(&case.points);
        for run in &case.runs {
            let got = spaced_points(&pts, run.n).unwrap();
            let worst = worst_distance(&got, &vec3s(&run.points));
            assert!(worst < PARITY, "{} with n = {}: worst difference {worst:e} m", case.name, run.n);
            overall = overall.max(worst);
        }
    }
    println!("catmull parity, worst over 9 sets x 3 counts: {overall:e} m");
}

// ---------------------------------------------------------------------------------------------------------------
// frames, sweep, tube, place, resample

#[derive(Deserialize)]
struct FrameRow {
    t: [f64; 3],
    n: [f64; 3],
    b: [f64; 3],
}

#[derive(Deserialize)]
struct FramesCase {
    name: String,
    up: Option<[f64; 3]>,
    points: Vec<[f64; 3]>,
    frames: Vec<FrameRow>,
}

#[derive(Deserialize)]
struct SweepCase {
    name: String,
    points: Vec<[f64; 3]>,
    seg: usize,
    up: Option<[f64; 3]>,
    rows: Vec<Vec<[f64; 3]>>,
    centres: Vec<[f64; 3]>,
}

#[derive(Deserialize)]
struct PlaceCase {
    pos: [f64; 3],
    dir: [f64; 3],
    spin: f64,
    scale: f64,
    matrix: [f64; 16],
}

#[derive(Deserialize)]
struct ResampleCase {
    name: String,
    points: Vec<[f64; 3]>,
    n: usize,
    samples: Vec<[f64; 3]>,
}

#[derive(Deserialize)]
struct GeometryFixture {
    frames: Vec<FramesCase>,
    sweeps: Vec<SweepCase>,
    tubes: Vec<SweepCase>,
    places: Vec<PlaceCase>,
    resamples: Vec<ResampleCase>,
}

fn geometry() -> GeometryFixture {
    serde_json::from_str(include_str!("fixtures/kit/sweep_geometry.json")).unwrap()
}

#[test]
fn frames_match_the_prototype_including_the_parallel_up_fallback() {
    let mut overall = 0.0_f64;
    for case in &geometry().frames {
        let up = case.up.map_or(DEFAULT_UP, DVec3::from_array);
        let got = frames(&vec3s(&case.points), up).unwrap();
        assert_eq!(got.len(), case.frames.len());
        for (k, (g, w)) in got.iter().zip(&case.frames).enumerate() {
            let worst =
                g.t.distance(DVec3::from_array(w.t))
                    .max(g.n.distance(DVec3::from_array(w.n)))
                    .max(g.b.distance(DVec3::from_array(w.b)));
            assert!(worst < PARITY, "{} frame {k}: {worst:e}", case.name);
            assert_eq!(g.p, DVec3::from_array(case.points[k]), "the frame sits on its path point");
            overall = overall.max(worst);
        }
    }
    println!("frames parity, worst: {overall:e}");
}

/// The sweep cases' radius, shape and twist, as written out in the fixture (the generator mirrors these).
fn sweep_case(case: &SweepCase) -> Sweep {
    let pts = vec3s(&case.points);
    let up = case.up.map_or(DEFAULT_UP, DVec3::from_array);
    let plain = SweepOptions { up, ..SweepOptions::default() };
    match case.name.as_str() {
        "helix, constant radius" => sweep(&pts, |_| (0.02, 0.02), case.seg, &plain),
        "arc, elliptic radius that varies" => {
            sweep(&pts, |t| (0.03 + 0.01 * (3.0 * t).sin(), 0.015 + 0.005 * t), case.seg, &plain)
        }
        "hair-like chain, shape and twist and up = +Y" => {
            let shape = |t: f64, phi: f64| (1.0 + 0.2 * (2.0 * phi).cos(), 1.0 - 0.1 * t);
            let twist = |t: f64| 0.8 * t;
            let options = SweepOptions { up, shape: Some(&shape), twist: Some(&twist) };
            sweep(&pts, |t| (0.01 * (1.0 - 0.5 * t), 0.01 * (1.0 - 0.5 * t)), case.seg, &options)
        }
        "constant radius 0.00058, 5 segments" => tube(&pts, |_| 0.00058, case.seg),
        "radius that varies, 6 segments" => tube(&pts, |t| 0.004 * (1.0 + 0.5 * (5.0 * t).sin()), case.seg),
        other => panic!("no definition for the fixture case {other:?}"),
    }
    .unwrap()
}

fn assert_sweep_matches(case: &SweepCase) -> f64 {
    let got = sweep_case(case);
    let g = &got.grid;
    assert_eq!((g.rows(), g.cols()), (case.rows.len(), case.seg + 1), "{}", case.name);
    let mut worst = worst_distance(&got.centres, &vec3s(&case.centres));
    for (i, row) in case.rows.iter().enumerate() {
        for (j, p) in row.iter().enumerate() {
            worst = worst.max(g.at(i, j).distance(DVec3::from_array(*p)));
        }
    }
    assert!(worst < PARITY, "{}: worst difference {worst:e} m", case.name);
    worst
}

#[test]
fn sweeps_match_the_prototype_with_radius_functions_shape_twist_and_up() {
    let fixture = geometry();
    assert_eq!(fixture.sweeps.len(), 3);
    let worst = fixture.sweeps.iter().map(assert_sweep_matches).fold(0.0, f64::max);
    println!("sweep parity, worst over 3 cases: {worst:e} m");
}

#[test]
fn tubes_match_the_prototype_with_a_constant_and_a_varying_radius() {
    let fixture = geometry();
    assert_eq!(fixture.tubes.len(), 2);
    let worst = fixture.tubes.iter().map(assert_sweep_matches).fold(0.0, f64::max);
    println!("tube parity, worst over 2 cases: {worst:e} m");
}

#[test]
fn placement_matrices_match_the_prototype() {
    let fixture = geometry();
    assert_eq!(fixture.places.len(), 6);
    let mut overall = 0.0_f64;
    for (n, c) in fixture.places.iter().enumerate() {
        let m = place(DVec3::from_array(c.pos), DVec3::from_array(c.dir), c.spin, c.scale).unwrap();
        let worst = m.to_cols_array().iter().zip(&c.matrix).map(|(g, w)| (g - w).abs()).fold(0.0, f64::max);
        assert!(worst < PARITY, "placement {n}: {worst:e}");
        overall = overall.max(worst);
    }
    println!("place parity, worst: {overall:e}");
}

#[test]
fn resampled_polylines_match_the_prototype_including_a_zero_length_segment() {
    let fixture = geometry();
    assert_eq!(fixture.resamples.len(), 4);
    let mut overall = 0.0_f64;
    for c in &fixture.resamples {
        let got = resample(&vec3s(&c.points), c.n).unwrap();
        assert_eq!(got.len(), c.n + 1, "{}", c.name);
        let worst = worst_distance(&got, &vec3s(&c.samples));
        assert!(worst < PARITY, "{}: {worst:e}", c.name);
        overall = overall.max(worst);
    }
    println!("resample parity, worst: {overall:e}");
}

// ---------------------------------------------------------------------------------------------------------------
// A helix: even rings, steady frames

/// `turns` turns of a helix about +Z, `per_turn` control points a turn.
fn helix(turns: usize, per_turn: usize, radius: f64, pitch: f64) -> Vec<DVec3> {
    let count = turns * per_turn + 1;
    (0..count)
        .map(|i| {
            let turn = i as f64 / per_turn as f64;
            let a = std::f64::consts::TAU * turn;
            DVec3::new(radius * a.cos(), radius * a.sin(), pitch * turn)
        })
        .collect()
}

fn degrees_between(a: DVec3, b: DVec3) -> f64 {
    a.cross(b).length().atan2(a.dot(b)).to_degrees()
}

/// The spread (largest minus smallest) of the values.
fn spread(values: impl Iterator<Item = f64>) -> f64 {
    let (lo, hi) = values.fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    hi - lo
}

/// What a sweep along `centres` (a helix, 3.6 degrees round the axis apart) must show: the rings evenly spaced
/// (`ring_spread`), each a true circle of the swept radius, centred on its path point and square to the path
/// (`ring_error`), and the frames turning smoothly (`worst_turn`, in degrees).
struct HelixMeasures {
    ring_spread: f64,
    ring_error: f64,
    worst_turn: f64,
}

fn helix_measures(centres: &[DVec3]) -> HelixMeasures {
    const RADIUS: f64 = 0.002;
    // A tube's rings are spaced like a sweep's; check both build the same centres.
    let piping = tube(centres, |_| RADIUS, 8).unwrap();
    let swept = sweep(centres, |_| (RADIUS, RADIUS), 8, &SweepOptions::default()).unwrap();
    assert_eq!(piping.centres, swept.centres);
    let ring_spread = spread(swept.centres.windows(2).map(|w| w[1].distance(w[0])));

    let path = frames(centres, DEFAULT_UP).unwrap();
    let mut ring_error = 0.0_f64;
    for (i, frame) in path.iter().enumerate() {
        for j in 0..swept.grid.cols() {
            let offset = swept.grid.at(i, j) - swept.centres[i];
            ring_error = ring_error.max((offset.length() - RADIUS).abs()).max(offset.dot(frame.t).abs());
        }
    }
    let worst_turn = path.windows(2).map(|w| degrees_between(w[0].n, w[1].n)).fold(0.0, f64::max);
    HelixMeasures { ring_spread, ring_error, worst_turn }
}

#[test]
fn a_sweep_along_a_helix_has_evenly_spaced_round_rings_and_frames_that_never_flip() {
    // Two turns of a 5 cm helix, 200 points: here the points themselves are even.
    let m = helix_measures(&helix(2, 100, 0.05, 0.02)[..200]);
    assert!(m.ring_spread < 1e-12, "ring centres are {:e} m uneven", m.ring_spread);
    assert!(m.ring_error < 1e-12, "rings are {:e} m off a circle square to the path", m.ring_error);
    assert!(m.worst_turn < 5.0, "consecutive normals turn up to {} degrees", m.worst_turn);
    println!(
        "even helix: ring spread {:e} m, ring error {:e} m, worst normal turn {:.3} degrees",
        m.ring_spread, m.ring_error, m.worst_turn
    );
}

#[test]
fn a_sweep_along_arc_length_spaced_catmull_points_of_a_helix_keeps_its_rings_within_1e_5() {
    // The same helix as 160 control points, spaced to 200 rings by three's 200-chord arc length: the rings are as even
    // as that measure allows (about 2e-6 m of a 3.2 mm spacing; 20 control points a turn would give 1.5e-5).
    let m = helix_measures(&spaced_points(&helix(2, 80, 0.05, 0.02), 200).unwrap());
    assert!(m.ring_spread < 1e-5, "ring centres are {:e} m uneven", m.ring_spread);
    assert!(m.ring_error < 1e-12, "rings are {:e} m off a circle square to the path", m.ring_error);
    assert!(m.worst_turn < 5.0, "consecutive normals turn up to {} degrees", m.worst_turn);
    println!(
        "spaced helix: ring spread {:e} m, ring error {:e} m, worst normal turn {:.3} degrees",
        m.ring_spread, m.ring_error, m.worst_turn
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Degenerate input is an error, never NaN or a panic

#[test]
fn degenerate_curves_are_typed_errors_in_every_function() {
    let p = DVec3::new(0.2, 0.4, 0.6);
    let same = [p, p, p, p];
    assert!(matches!(spaced_points(&same, 5), Err(KitError::Degenerate { .. })), "coincident points");
    assert!(matches!(resample(&same, 5), Err(KitError::Degenerate { .. })));
    assert!(matches!(frames(&same, DEFAULT_UP), Err(KitError::Degenerate { .. })));
    assert!(matches!(tube(&same, |_| 0.01, 6), Err(KitError::Degenerate { .. })));
    assert!(matches!(sweep(&same, |_| (0.1, 0.1), 6, &SweepOptions::default()), Err(KitError::Degenerate { .. })));

    let nan = [DVec3::ZERO, DVec3::new(f64::NAN, 0.0, 0.0), DVec3::X];
    assert!(matches!(spaced_points(&nan, 5), Err(KitError::NonFinite { .. })));
    assert!(matches!(resample(&nan, 5), Err(KitError::NonFinite { .. })));
    assert!(matches!(frames(&nan, DEFAULT_UP), Err(KitError::NonFinite { .. })));
    assert!(matches!(tube(&nan, |_| 0.01, 6), Err(KitError::NonFinite { .. })));

    let lone = [DVec3::X];
    assert!(matches!(spaced_points(&lone, 5), Err(KitError::TooFew { .. })));
    assert!(matches!(resample(&lone, 5), Err(KitError::TooFew { .. })));
    assert!(matches!(tube(&lone, |_| 0.01, 6), Err(KitError::TooFew { .. })));
    assert!(matches!(spaced_points(&[DVec3::ZERO, DVec3::X], 1), Err(KitError::TooFew { .. })), "n < 2");
}

// ---------------------------------------------------------------------------------------------------------------
// Properties on random polylines

/// A coordinate on a coarse lattice, so random polylines often repeat points, double back and run straight.
fn lattice() -> impl Strategy<Value = f64> {
    (-3_i8..=3).prop_map(|c| f64::from(c) * 0.25)
}

fn polyline() -> impl Strategy<Value = Vec<DVec3>> {
    let point = prop_oneof![
        (-2.0_f64..2.0, -2.0_f64..2.0, -2.0_f64..2.0).prop_map(|(x, y, z)| DVec3::new(x, y, z)),
        (lattice(), lattice(), lattice()).prop_map(|(x, y, z)| DVec3::new(x, y, z)),
    ];
    prop::collection::vec(point, 0..14)
}

/// `prop_assert!` prints its expression with `format!`, which cannot hold the braces of a `matches!` pattern.
fn is_non_finite<T>(result: &Result<T, KitError>) -> bool {
    matches!(result, Err(KitError::NonFinite { .. }))
}

fn all_finite(points: &[DVec3]) -> bool {
    points.iter().all(|p| p.is_finite())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    #[test]
    fn spaced_points_are_finite_ordered_from_end_to_end_or_a_typed_error(pts in polyline(), n in 0_usize..70) {
        match spaced_points(&pts, n) {
            Ok(out) => {
                prop_assert_eq!(out.len(), n);
                prop_assert!(all_finite(&out));
                prop_assert!(out[0].distance(pts[0]) < 1e-9, "starts on the first point");
                prop_assert!(out[n - 1].distance(pts[pts.len() - 1]) < 1e-9, "ends on the last point");
            }
            Err(KitError::TooFew { .. } | KitError::Degenerate { .. }) => {
                // Only too short an input, or one with no length at all.
                prop_assert!(pts.len() < 2 || n < 2 || pts.windows(2).all(|w| w[0] == w[1]));
            }
            Err(other) => prop_assert!(false, "unexpected error {other}"),
        }
    }

    #[test]
    fn resampled_points_are_finite_evenly_spread_and_never_further_apart_than_their_share(pts in polyline(), n in 0_usize..70) {
        match resample(&pts, n) {
            Ok(out) => {
                prop_assert_eq!(out.len(), n + 1);
                prop_assert!(all_finite(&out));
                let length: f64 = pts.windows(2).map(|w| w[1].distance(w[0])).sum();
                for w in out.windows(2) {
                    // A chord between neighbouring samples is at most their arc share (equal on a straight stretch).
                    prop_assert!(w[1].distance(w[0]) <= length / n as f64 + 1e-9);
                }
                prop_assert!(out[0].distance(pts[0]) < 1e-9 && out[n].distance(pts[pts.len() - 1]) < 1e-9);
            }
            Err(KitError::TooFew { .. } | KitError::Degenerate { .. }) => {
                prop_assert!(pts.len() < 2 || n == 0 || pts.windows(2).all(|w| w[0] == w[1]));
            }
            Err(other) => prop_assert!(false, "unexpected error {other}"),
        }
    }

    #[test]
    fn swept_surfaces_and_their_frames_are_finite_and_orthonormal_or_a_typed_error(pts in polyline(), seg in 1_usize..13) {
        match frames(&pts, DEFAULT_UP) {
            Ok(path) => {
                for f in &path {
                    prop_assert!((f.t.length() - 1.0).abs() < 1e-9 && (f.n.length() - 1.0).abs() < 1e-9 && (f.b.length() - 1.0).abs() < 1e-9);
                    prop_assert!(f.t.dot(f.n).abs() < 1e-9 && f.t.dot(f.b).abs() < 1e-9 && f.n.dot(f.b).abs() < 1e-9);
                }
                let s = sweep(&pts, |t| (0.01 + 0.01 * t, 0.02), seg, &SweepOptions::default()).unwrap();
                prop_assert!(all_finite(s.grid.points()) && all_finite(&s.centres));
                prop_assert_eq!((s.grid.rows(), s.grid.cols()), (pts.len(), seg + 1));
                let t = tube(&pts, |_| 0.003, seg).unwrap();
                prop_assert!(all_finite(t.grid.points()));
            }
            Err(KitError::TooFew { .. } | KitError::Degenerate { .. }) => {
                prop_assert!(sweep(&pts, |_| (0.1, 0.1), seg, &SweepOptions::default()).is_err());
                prop_assert!(tube(&pts, |_| 0.1, seg).is_err());
            }
            Err(other) => prop_assert!(false, "unexpected error {other}"),
        }
    }

    #[test]
    fn a_non_finite_coordinate_anywhere_is_an_error_in_every_function(
        mut pts in prop::collection::vec((-2.0_f64..2.0, -2.0_f64..2.0, -2.0_f64..2.0), 2..10),
        at in 0_usize..10,
        bad in prop_oneof![Just(f64::NAN), Just(f64::INFINITY), Just(f64::NEG_INFINITY)],
        axis in 0_usize..3,
    ) {
        let at = at % pts.len();
        match axis {
            0 => pts[at].0 = bad,
            1 => pts[at].1 = bad,
            _ => pts[at].2 = bad,
        }
        let pts: Vec<DVec3> = pts.into_iter().map(|(x, y, z)| DVec3::new(x, y, z)).collect();
        prop_assert!(is_non_finite(&spaced_points(&pts, 8)));
        prop_assert!(is_non_finite(&resample(&pts, 8)));
        prop_assert!(is_non_finite(&frames(&pts, DEFAULT_UP)));
        prop_assert!(is_non_finite(&tube(&pts, |_| 0.01, 6)));
    }
}
