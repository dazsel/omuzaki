//! Parity of the body loft (`omuzaki_char::body::mesh`) with the prototype: the legs of `body.js` `buildLeg` and the torso
//! loft of `uniform.js`.
//!
//! Two oracles, because the sample character's dumps stay in a local assets folder and CI has none:
//!
//! - **Committed fixture** (`fixtures/char_body/loft.json`, made by the prototype's generator script running the prototype's own `buildLeg`
//!   and torso loft under Node with three.js): the left leg's 49 x 45 grid and the torso's 15 x 45 grid, positions and
//!   normals in the prototype's vertex order. Runs everywhere.
//! - **The JS dump** (`geometry/game/tights`, skipped without the drive): the tights mesh, both legs, against T-GEOM.
//!
//! The prototype's rows are heights `y`; the layout's regions run along an axis (a leg from the hip down), so each row is
//! mapped to the fraction of the region it lies at, and the prototype's `dome` and `bot` rows are caps of the same shape
//! (`mesh::Cap`). The soft-sd layout reproduces `LEG` and `TORSO` (`template_soft_sd.rs`), so the grids must agree to
//! rounding: the fixture is f32, so about 1e-7 m. The Rust surface has a pole vertex and a triangle fan where the
//! prototype has a collapsed row, so the surfaces are compared as grids, which is where they are the same.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss)] // row indices are small

use omuzaki_char::body::mesh::{Cap, ring_grid, rings_at};
use omuzaki_char::kit::grid::{GridOptions, grid_normals};
use omuzaki_char::template::layout::{self, BodyLayout};
use omuzaki_char::template::soft_sd::TABLE;
use omuzaki_char_api::{BodyRegion, BodyShape, HandParams, Proportions};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_test::jsref::JsRef;
use serde::Deserialize;

#[derive(Deserialize)]
struct Block {
    rows: usize,
    cols: usize,
    positions: Vec<f32>,
    normals: Vec<f32>,
    #[serde(default)]
    ys: Vec<f64>,
}

#[derive(Deserialize)]
struct Fixture {
    leg: Block,
    torso: Block,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/char_body/loft.json")).unwrap()
}

/// The loft the prototype's numbers describe: the layout before the blend at the hip, which reshapes the legs.
fn soft_sd() -> BodyLayout {
    layout::build_loft(&TABLE, &Proportions::default(), &BodyShape::default(), &HandParams::default()).unwrap()
}

/// The prototype's `buildLeg` rows, bottom to top: `bot` (6 cap rows under the sole), `rowsOf([0.04, 0.12, 8], [0.12, 0.40,
/// 28])` (the 37 body rows, heights returned here), `dome` (6 cap rows above). The leg is `0.42 - y` along from the hip.
fn leg_rows() -> Vec<f64> {
    let mut ys: Vec<f64> = (0..=8).map(|i| 0.04 + 0.08 * f64::from(i) / 8.0).collect();
    ys.extend((1..=28).map(|i| 0.12 + 0.28 * f64::from(i) / 28.0));
    ys
}

/// Where the prototype's dome starts (`top`): its curves stop at 0.405, 5 mm above the last body row. The Rust cap grows
/// from a ring, so the grid has a ring there that the prototype has not.
const TOP: f64 = 0.405;

/// The Rust grid of the leg (rows from the hip down: the dome's 6 rows, the ring at `TOP`, the 37 body rows, the sole's 6
/// cap rows), with the centres of its rows.
fn leg_grid(layout: &BodyLayout) -> (omuzaki_char::kit::grid::Grid, Vec<DVec3>) {
    let leg = layout.region(BodyRegion::LegL).unwrap();
    let ts: Vec<f64> =
        std::iter::once(TOP).chain(leg_rows().into_iter().rev()).map(|y| (0.42 - y) / leg.length()).collect();
    let rings = rings_at(layout, BodyRegion::LegL, &ts).unwrap();
    ring_grid(&rings, Cap { length: 0.03, rings: 6 }, Cap { length: 0.012, rings: 6 }, 44).unwrap()
}

/// The prototype's row of the Rust grid's row `r`, or `None` for the ring at `TOP` that only Rust has.
fn prototype_row(r: usize, rows: usize) -> Option<usize> {
    match r {
        0..=5 => Some(rows - 2 - r),
        6 => None,
        _ => Some(rows - 1 - r),
    }
}

/// The worst distance between the grid's points and the fixture's, over the rows both have.
fn worst_distance(
    grid: &omuzaki_char::kit::grid::Grid,
    fixture: &Block,
    row_of: impl Fn(usize) -> Option<usize>,
) -> f64 {
    let mut worst = 0.0_f64;
    for r in 0..grid.rows() {
        let Some(k) = row_of(r) else { continue };
        for j in 0..fixture.cols {
            let at = (k * fixture.cols + j) * 3;
            let expected = Vec3::new(fixture.positions[at], fixture.positions[at + 1], fixture.positions[at + 2]);
            worst = worst.max((grid.at(r, j) - expected.as_dvec3()).length());
        }
    }
    worst
}

#[test]
fn the_leg_grid_is_buildleg_in_the_prototypes_vertex_order() {
    let f = fixture();
    let (grid, _) = leg_grid(&soft_sd());
    assert_eq!((grid.rows(), grid.cols()), (f.leg.rows + 1, f.leg.cols));
    let worst = worst_distance(&grid, &f.leg, |r| prototype_row(r, grid.rows()));
    eprintln!("leg grid vs buildLeg: worst vertex distance {worst:.3e} m");
    assert!(worst < 2e-4, "T-GEOM: {worst:e} m");
    assert!(worst < 1e-6, "the grids are the same up to rounding: {worst:e} m");
}

#[test]
fn the_leg_normals_point_where_the_prototypes_do() {
    let f = fixture();
    let (grid, centres) = leg_grid(&soft_sd());
    let inside = |i: usize, _: usize| centres[i];
    let normals =
        grid_normals(&grid, &GridOptions { wrap_j: true, inside: Some(&inside), ..GridOptions::default() }).unwrap();
    let mut worst = 0.0_f64;
    for r in 0..grid.rows() {
        // The extra ring changes the central difference of the two rows next to it, so they are left out.
        let Some(k) = prototype_row(r, grid.rows()).filter(|_| !(5..=7).contains(&r)) else { continue };
        for j in 0..f.leg.cols {
            let at = (k * f.leg.cols + j) * 3;
            let expected = DVec3::new(
                f64::from(f.leg.normals[at]),
                f64::from(f.leg.normals[at + 1]),
                f64::from(f.leg.normals[at + 2]),
            );
            worst = worst.max(normals.normals[r * f.leg.cols + j].dot(expected).clamp(-1.0, 1.0).acos().to_degrees());
        }
    }
    eprintln!("leg normals vs buildLeg: worst angle {worst:.3e} degrees");
    assert!(worst < 1.0, "T-GEOM: {worst} degrees");
}

#[test]
fn the_torso_grid_is_the_shirts_loft_of_the_torso_curves() {
    let f = fixture();
    let layout = soft_sd();
    let torso = layout.region(BodyRegion::Torso).unwrap();
    let ts: Vec<f64> = f.torso.ys.iter().map(|y| (y - 0.325) / torso.length()).collect();
    let rings = rings_at(&layout, BodyRegion::Torso, &ts).unwrap();
    let (grid, centres) =
        ring_grid(&rings, Cap { length: 0.05, rings: 2 }, Cap { length: 0.02, rings: 2 }, 44).unwrap();
    // Two cap rows, one at each end, come before and after the 15 body rows.
    let worst = worst_distance(&grid, &f.torso, |r| r.checked_sub(2).filter(|k| *k < f.torso.rows));
    eprintln!("torso grid vs the shirt loft: worst vertex distance {worst:.3e} m");
    assert!(worst < 1e-6, "{worst:e} m");
    let inside = |i: usize, _: usize| centres[i];
    let normals =
        grid_normals(&grid, &GridOptions { wrap_j: true, inside: Some(&inside), ..GridOptions::default() }).unwrap();
    // The caps change the end rows' normals (the prototype's loft is open), so compare the interior rows.
    let mut worst = 0.0_f64;
    for k in 1..f.torso.rows - 1 {
        for j in 0..f.torso.cols {
            let at = (k * f.torso.cols + j) * 3;
            let expected = DVec3::new(
                f64::from(f.torso.normals[at]),
                f64::from(f.torso.normals[at + 1]),
                f64::from(f.torso.normals[at + 2]),
            );
            let got = normals.normals[(k + 2) * f.torso.cols + j];
            worst = worst.max(got.dot(expected).clamp(-1.0, 1.0).acos().to_degrees());
        }
    }
    eprintln!("torso normals vs the shirt loft: worst angle {worst:.3e} degrees");
    assert!(worst < 1.0, "T-GEOM: {worst} degrees");
}

#[test]
fn the_tights_dump_meets_t_geom() {
    let Some(js) = JsRef::open() else { return };
    let dump = js.geometry("game").unwrap();
    let tights = &dump.mesh("tights").unwrap().mesh;
    let (grid, _) = leg_grid(&soft_sd());
    let (rows, cols) = (grid.rows() - 1, grid.cols());
    assert_eq!(tights.positions.len(), 2 * rows * cols, "both legs");
    let (mut left, mut right) = (0.0_f64, 0.0_f64);
    for r in 0..grid.rows() {
        let Some(k) = prototype_row(r, grid.rows()) else { continue };
        for j in 0..cols {
            let want = grid.at(r, j).as_vec3();
            left = left.max(f64::from((tights.positions[k * cols + j] - want).length()));
            let mirrored = Vec3::new(-want.x, want.y, want.z);
            right = right.max(f64::from((tights.positions[rows * cols + k * cols + j] - mirrored).length()));
        }
    }
    eprintln!("tights dump vs the leg grid: left {left:.3e} m, right {right:.3e} m");
    assert!(left < 2e-4 && right < 2e-4, "T-GEOM: left {left:e}, right {right:e}");
}
