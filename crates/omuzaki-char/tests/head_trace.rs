//! The traced radius map: its byte format, its bilinear sampling (the prototype's `tracedRadius`) and its blend into the
//! head. Every grid here is synthetic and written to bytes in the test; no character's data is used.

// The grids are a few cells across, so every index and count fits the float and integer types it is cast to.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]

use std::f64::consts::{PI, TAU};
use std::sync::Arc;

use omuzaki_char::head::TraceSample;
use omuzaki_char::head::mesh::{Resolution, build};
use omuzaki_char::head::surface;
use omuzaki_char::head::trace::sample::{HeadTrace, HeadTraceError};
use omuzaki_char_api::HeadParams;
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::rng::Rng;

const NT: usize = 8;
const NP: usize = 16;

/// The bytes of a radius grid then a weight grid, as the asset lays them out.
fn grid_bytes(radii: &[f32], weights: &[f32]) -> Vec<u8> {
    radii.iter().chain(weights).flat_map(|v| v.to_le_bytes()).collect()
}

/// A map from `r(t, p)` and `w(t, p)` over the `NT x NP` cells.
fn map(r: impl Fn(usize, usize) -> f32, w: impl Fn(usize, usize) -> f32) -> HeadTrace {
    let cells = || (0..NT).flat_map(|t| (0..NP).map(move |p| (t, p)));
    let radii: Vec<f32> = cells().map(|(t, p)| r(t, p)).collect();
    let weights: Vec<f32> = cells().map(|(t, p)| w(t, p)).collect();
    HeadTrace::from_bytes(NT as u16, NP as u16, &grid_bytes(&radii, &weights))
        .unwrap_or_else(|e| panic!("a synthetic grid is valid: {e}"))
}

/// The unit direction from the head centre at polar angle `theta` and azimuth `phi` (from the front, toward +x), in
/// the single precision the sampler takes.
fn direction(theta: f64, phi: f64) -> Vec3 {
    Vec3::new((theta.sin() * phi.sin()) as f32, theta.cos() as f32, (theta.sin() * phi.cos()) as f32)
}

/// A cell centre's direction: theta is `(t + 0.5)` cells down from the top, phi `(p + 0.5)` cells round from the front.
fn cell_direction(t: usize, p: usize) -> Vec3 {
    direction((t as f64 + 0.5) * PI / NT as f64, (p as f64 + 0.5) * TAU / NP as f64)
}

/// The radius the cell-centre test writes: a smooth function of the cell's centre.
fn smooth_radius(t: usize, p: usize) -> f64 {
    let theta = (t as f64 + 0.5) * PI / NT as f64;
    let phi = (p as f64 + 0.5) * TAU / NP as f64;
    0.09 + 0.01 * theta.sin() * phi.cos()
}

#[test]
fn cell_centres_give_their_own_radius_and_weight() {
    let trace = map(|t, p| smooth_radius(t, p) as f32, |t, p| ((t + p) % 2) as f32);
    for t in 0..NT {
        for p in 0..NP {
            let (r, w) = trace.sample(cell_direction(t, p));
            let want_r = smooth_radius(t, p);
            assert!((f64::from(r) - want_r).abs() < 1e-6, "radius at ({t}, {p}): {r} against {want_r}");
            assert!((w - ((t + p) % 2) as f32).abs() < 1e-6, "weight at ({t}, {p}): {w}");
        }
    }
}

#[test]
fn between_cell_centres_the_radius_is_bilinear() {
    let trace = map(|t, p| 0.1 + 0.001 * t as f32 + 0.0001 * p as f32, |_, _| 1.0);
    let mut rng = Rng::new(163);
    for _ in 0..1000 {
        let ft = f64::from(rng.range_f32(0.0, (NT - 1) as f32));
        let fp = f64::from(rng.range_f32(0.0, (NP - 1) as f32));
        let dir = direction((ft + 0.5) * PI / NT as f64, (fp + 0.5) * TAU / NP as f64);
        let (r, _) = trace.sample(dir);
        let want = 0.1 + 0.001 * ft + 0.0001 * fp;
        assert!((f64::from(r) - want).abs() < 1e-6, "at ({ft}, {fp}): {r} against {want}");
    }
}

#[test]
fn phi_wraps_between_the_last_and_first_columns() {
    let trace = map(|_, p| 0.1 + 0.01 * p as f32, |_, _| 1.0);
    // Phi = 0 on a row centre: half-way between the last column and the first.
    let (r, _) = trace.sample(direction((3.5) * PI / NT as f64, 0.0));
    let want = (0.1 + 0.01 * (NP - 1) as f32 + 0.1) / 2.0;
    assert!((r - want).abs() < 1e-6, "got {r}, want the mean of the wrap {want}");
}

#[test]
fn theta_clamps_at_the_poles_without_wrapping() {
    let trace = map(|t, p| 0.1 + 0.01 * t as f32 + 0.001 * p as f32, |_, _| 1.0);
    // The mean of a row's last and first columns, which is where a pole direction lands in phi.
    let row_mean = |t: usize| 0.1 + 0.01 * t as f32 + 0.001 * (NP - 1) as f32 / 2.0;
    let first_row = row_mean(0);
    let last_row = row_mean(NT - 1);
    let (top, _) = trace.sample(Vec3::Y);
    assert!((top - first_row).abs() < 1e-6, "+Y gives row 0's wrap mean {first_row}, got {top}");
    let (bottom, _) = trace.sample(Vec3::NEG_Y);
    assert!((bottom - last_row).abs() < 1e-6, "-Y gives the last row's wrap mean {last_row}, got {bottom}");
}

#[test]
fn a_grid_round_trips_through_its_bytes() {
    let radii: Vec<f32> = (0..NT * NP).map(|i| 0.05 + 0.001 * i as f32).collect();
    let weights: Vec<f32> = (0..NT * NP).map(|i| (i % 3) as f32 / 2.0).collect();
    let trace = HeadTrace::from_bytes(NT as u16, NP as u16, &grid_bytes(&radii, &weights)).unwrap();
    assert_eq!((trace.theta_steps(), trace.phi_steps()), (NT as u16, NP as u16));
    let (r, w) = trace.sample(cell_direction(2, 5));
    let cell = 2 * NP + 5;
    assert!((r - radii[cell]).abs() < 1e-6 && (w - weights[cell]).abs() < 1e-6, "got ({r}, {w})");
}

#[test]
fn a_wrong_byte_length_is_an_error() {
    let bytes = grid_bytes(&vec![0.1; NT * NP], &vec![1.0; NT * NP]);
    let short = HeadTrace::from_bytes(NT as u16, NP as u16, &bytes[1..]);
    let mut long = bytes.clone();
    long.push(0);
    let empty = HeadTrace::from_bytes(NT as u16, NP as u16, &[]);
    let expected = 2 * 4 * NT * NP;
    for result in [short, HeadTrace::from_bytes(NT as u16, NP as u16, &long), empty] {
        assert!(matches!(result, Err(HeadTraceError::Length { expected: e, .. }) if e == expected), "{result:?}");
    }
    let message = HeadTrace::from_bytes(NT as u16, NP as u16, &[]).unwrap_err().to_string();
    assert_eq!(message, format!("a radius map needs {expected} bytes, got 0"));
}

#[test]
fn a_non_finite_value_names_its_grid_row_and_column() {
    let mut radii = vec![0.1_f32; NT * NP];
    radii[3 * NP + 7] = f32::NAN;
    let bytes = grid_bytes(&radii, &vec![1.0; NT * NP]);
    let err = HeadTrace::from_bytes(NT as u16, NP as u16, &bytes).unwrap_err();
    assert_eq!(err, HeadTraceError::NotFinite { grid: "radius", row: 3, column: 7 });
    assert!(
        err.to_string().contains("radius") && err.to_string().contains("row 3") && err.to_string().contains("column 7")
    );

    let mut weights = vec![1.0_f32; NT * NP];
    weights[NP - 1] = f32::INFINITY;
    let bytes = grid_bytes(&vec![0.1; NT * NP], &weights);
    let err = HeadTrace::from_bytes(NT as u16, NP as u16, &bytes).unwrap_err();
    assert_eq!(err, HeadTraceError::NotFinite { grid: "weight", row: 0, column: NP - 1 });
}

#[test]
fn a_weight_outside_zero_to_one_is_an_error_and_its_bounds_parse() {
    for weight in [-1e-6_f32, 1.0 + 1e-6] {
        let mut weights = vec![1.0_f32; NT * NP];
        weights[2 * NP + 4] = weight;
        let bytes = grid_bytes(&vec![0.1; NT * NP], &weights);
        let err = HeadTrace::from_bytes(NT as u16, NP as u16, &bytes).unwrap_err();
        assert_eq!(err, HeadTraceError::WeightOutOfRange { row: 2, column: 4, weight });
        assert!(err.to_string().contains("row 2, column 4"), "{err}");
    }
    for weight in [0.0_f32, 1.0] {
        let bytes = grid_bytes(&vec![0.1; NT * NP], &vec![weight; NT * NP]);
        assert!(HeadTrace::from_bytes(NT as u16, NP as u16, &bytes).is_ok(), "weight {weight} should parse");
    }
}

#[test]
fn a_negative_radius_is_an_error_and_zero_parses() {
    let mut radii = vec![0.1_f32; NT * NP];
    radii[5 * NP + 1] = -1e-6;
    let bytes = grid_bytes(&radii, &vec![1.0; NT * NP]);
    let err = HeadTrace::from_bytes(NT as u16, NP as u16, &bytes).unwrap_err();
    assert_eq!(err, HeadTraceError::NegativeRadius { row: 5, column: 1, radius: -1e-6 });
    assert!(err.to_string().contains("row 5, column 1"), "{err}");

    let bytes = grid_bytes(&vec![0.0; NT * NP], &vec![1.0; NT * NP]);
    assert!(HeadTrace::from_bytes(NT as u16, NP as u16, &bytes).is_ok(), "a radius of zero should parse");
}

#[test]
fn an_empty_grid_is_an_error() {
    assert!(matches!(HeadTrace::from_bytes(0, 4, &[]), Err(HeadTraceError::Empty { theta_steps: 0, phi_steps: 4 })));
    assert!(matches!(HeadTrace::from_bytes(4, 0, &[]), Err(HeadTraceError::Empty { theta_steps: 4, phi_steps: 0 })));
}

#[test]
fn degenerate_directions_do_not_panic() {
    let trace = map(|_, _| 0.1, |_, _| 1.0);
    let (r, w) = trace.sample(Vec3::ZERO);
    assert!(r.is_finite() && w.is_finite());
    // A NaN component has no direction, so the answer is NaN; the point is that nothing panics.
    let _ = trace.sample(Vec3::new(f32::NAN, 0.0, 1.0));
}

#[test]
fn a_constant_map_is_the_head_radius_through_the_surface() {
    let trace = map(|_, _| 0.12, |_, _| 1.0);
    let head = surface(&HeadParams::default(), Some(Arc::new(trace)));
    let mut rng = Rng::new(163);
    for _ in 0..100 {
        let dir = random_unit(&mut rng);
        let r = head.radius(dir);
        assert!((r - 0.12).abs() < 1e-6, "radius along {dir:?} is {r}");
    }
}

#[test]
fn a_zero_weight_map_leaves_the_mesh_bit_identical() {
    let trace = Arc::new(map(|_, _| 0.12, |_, _| 0.0));
    let params = HeadParams::default();
    let traced = build(&params, Resolution::LIVE, Some(trace.as_ref())).unwrap();
    let untraced = build(&params, Resolution::LIVE, None).unwrap();
    assert_eq!(traced, untraced);
}

/// A uniformly random unit direction.
fn random_unit(rng: &mut Rng) -> Vec3 {
    loop {
        let v = DVec3::new(
            f64::from(rng.range_f32(-1.0, 1.0)),
            f64::from(rng.range_f32(-1.0, 1.0)),
            f64::from(rng.range_f32(-1.0, 1.0)),
        );
        let len = v.length();
        if len > 1e-3 && len <= 1.0 {
            return (v / len).as_vec3();
        }
    }
}
