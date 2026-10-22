//! The welted sole and the low heel block under it (`boots.js` `buildSole`).
//!
//! Both are lathes of the boot's own outline: every ring is the outline at one fixed height, pushed out for the welt and
//! shrunk toward the foot's centre for the flat top and bottom, so the sole is exactly as wide as the boot above it.

use omuzaki_core::glam::DVec3;

use super::profile::Boot;
use super::{Heel, Sole, Tess};
use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};

/// The thickness the sole's rows below are given for, metres: the prototype's.
const SOLE_THICKNESS: f64 = 0.0115;
/// The welt's width the rows' offsets are given for, metres.
const WELT: f64 = 0.0025;
/// The heel's height the heel rows are given for, metres.
const HEEL_HEIGHT: f64 = 0.0200;
/// `(height, push out along the outline, scale about the centre)` for each ring of the sole, bottom to top. The first
/// three and last three rings are the flat bottom and top (concentric rings shrinking to the centre).
const SOLE_ROWS: [(f64, f64, f64); 12] = [
    (0.0, 0.0, 0.0),
    (0.0, 0.0, 0.45),
    (0.0, 0.0, 0.8),
    (0.0, 0.0, 1.0),
    (0.0010, 0.0013, 1.0),
    (0.0026, 0.0022, 1.0),
    (0.0055, 0.0025, 1.0),
    (0.0085, 0.0021, 1.0),
    (0.0106, 0.0012, 1.0),
    (SOLE_THICKNESS, 0.0, 1.0),
    (SOLE_THICKNESS, 0.0, 0.7),
    (SOLE_THICKNESS, 0.0, 0.0),
];
/// The same for the heel block.
const HEEL_ROWS: [(f64, f64, f64); 12] = [
    (0.0, 0.0, 0.0),
    (0.0, 0.0, 0.45),
    (0.0, 0.0, 0.8),
    (0.0, 0.0, 1.0),
    (0.0012, 0.0014, 1.0),
    (0.0035, 0.0024, 1.0),
    (0.0100, 0.0024, 1.0),
    (0.0155, 0.0022, 1.0),
    (0.0188, 0.0012, 1.0),
    (HEEL_HEIGHT, 0.0, 1.0),
    (HEEL_HEIGHT, 0.0, 0.6),
    (HEEL_HEIGHT, 0.0, 0.0),
];
/// The boot-space height the heel's back is read at.
const HEEL_BACK_AT: f64 = 0.01;
/// The heel's breast (its front face) curves back by this much at `HEEL_BREAST_WIDTH` from the centre.
const HEEL_BREAST_CURVE: f64 = 0.005;
const HEEL_BREAST_WIDTH: f64 = 0.04;
/// Where inside the sole and the heel the normals are oriented away from: heights above the floor, and how far in front
/// of the heel's breast the heel's reference lies.
const SOLE_INSIDE_HEIGHT: f64 = 0.006;
const HEEL_INSIDE: (f64, f64) = (0.008, 0.025);

/// Adds the sole to `builder`: a closed slab.
pub(super) fn add_sole(boot: &Boot, tess: &Tess, sole: &Sole, builder: &mut MeshBuilder) -> Result<(), KitError> {
    let (scale_y, scale_d) = (sole.thickness / SOLE_THICKNESS, sole.welt / WELT);
    let seg = tess.sole_seg;
    let space = boot.space();
    let ctr = boot.secs(sole.thickness);
    let grid = Grid::from_fn(SOLE_ROWS.len(), seg + 1, |i, j| {
        let (y, d, sc) = SOLE_ROWS[i];
        let p = boot.plan(sole.thickness, fraction(j, seg) * TAU, d * scale_d).p;
        DVec3::new(ctr.cx + (p.x - ctr.cx) * sc, space.flat(y * scale_y), ctr.cz + (p.z - ctr.cz) * sc)
    })?;
    let inside = DVec3::new(ctr.cx, space.flat(SOLE_INSIDE_HEIGHT), ctr.cz);
    add_slab(boot, &grid, inside, builder)
}

/// Adds the heel block to `builder`: a second closed slab, D-shaped, its front cut off.
pub(super) fn add_heel(
    boot: &Boot,
    tess: &Tess,
    sole: &Sole,
    heel: &Heel,
    builder: &mut MeshBuilder,
) -> Result<(), KitError> {
    let (scale_y, scale_d) = (heel.height / HEEL_HEIGHT, sole.welt / WELT);
    let seg = tess.sole_seg;
    let space = boot.space();
    let ctr = boot.secs(sole.thickness);
    let back = boot.secs(HEEL_BACK_AT).zb;
    let zcut = back + heel.length;
    let (hx, hz) = (ctr.cx, f64::midpoint(zcut, back));
    let grid = Grid::from_fn(HEEL_ROWS.len(), seg + 1, |i, j| {
        let (y, d, sc) = HEEL_ROWS[i];
        let p = boot.plan(sole.thickness, fraction(j, seg) * TAU, d * scale_d).p;
        let limit = zcut - HEEL_BREAST_CURVE * ((p.x - ctr.cx) / HEEL_BREAST_WIDTH).powi(2);
        let z = p.z.min(limit);
        DVec3::new(hx + (p.x - hx) * sc, space.heel(y * scale_y), hz + (z - hz) * sc)
    })?;
    let inside = DVec3::new(ctr.cx, space.heel(HEEL_INSIDE.0), zcut - HEEL_INSIDE.1);
    add_slab(boot, &grid, inside, builder)
}

/// Adds a lathed slab, oriented away from `inside`, skinned by position.
fn add_slab(boot: &Boot, grid: &Grid, inside: DVec3, builder: &mut MeshBuilder) -> Result<(), KitError> {
    let reference = |_: usize, _: usize| inside;
    let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);
    let options =
        GridOptions { wrap_j: true, inside: Some(&reference), skin: Skin::Per(&skin), ..GridOptions::default() };
    builder.add_grid(grid, &options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outfit::boots::Tess;
    use crate::outfit::boots::profile::{BootProfile, Shape};

    fn boot() -> Boot {
        let flat = |v: f64| vec![[0.0, v], [0.12, v]];
        let profile = BootProfile { cx: flat(0.0), zb: flat(-0.04), zf: flat(0.06), rx: flat(0.03) };
        Boot::explicit(&profile, Shape { widen: 0.075, squareness: 2.65 }, SOLE_THICKNESS).unwrap()
    }

    #[test]
    fn the_sole_and_the_heel_are_closed_slabs_of_the_prototypes_size() {
        let (boot, tess) = (boot(), Tess::OFFLINE);
        let mut b = MeshBuilder::new();
        add_sole(&boot, &tess, &Sole::default(), &mut b).unwrap();
        assert_eq!(b.triangle_count(), 1848);
        add_heel(&boot, &tess, &Sole::default(), &Heel::default(), &mut b).unwrap();
        assert_eq!(b.triangle_count(), 2 * 1848);
        let ys = b.mesh().positions.iter().map(|p| p.y);
        assert!(ys.clone().fold(f32::INFINITY, f32::min).abs() < 1e-7, "on the floor");
        assert!((ys.fold(0.0, f32::max) - 0.02).abs() < 1e-6, "the heel is 20 mm");
    }

    #[test]
    fn the_heels_front_is_cut_off_at_its_breast() {
        let (boot, tess) = (boot(), Tess::OFFLINE);
        let heel = Heel { length: 0.03, height: 0.02 };
        let mut b = MeshBuilder::new();
        add_heel(&boot, &tess, &Sole::default(), &heel, &mut b).unwrap();
        let front = b.mesh().positions.iter().map(|p| p.z).fold(f32::NEG_INFINITY, f32::max);
        assert!(f64::from(front) <= -0.04 + heel.length + 0.003, "the block stops {front} m forward");
    }

    #[test]
    fn a_thicker_sole_and_welt_scale_the_rows() {
        let (boot, tess) = (boot(), Tess::OFFLINE);
        let sole = Sole { thickness: 0.0115, welt: 0.005, ..Sole::default() };
        let (mut thin, mut wide) = (MeshBuilder::new(), MeshBuilder::new());
        add_sole(&boot, &tess, &Sole::default(), &mut thin).unwrap();
        add_sole(&boot, &tess, &sole, &mut wide).unwrap();
        let width = |b: &MeshBuilder| {
            let xs = b.mesh().positions.iter().map(|p| p.x);
            xs.clone().fold(f32::NEG_INFINITY, f32::max) - xs.fold(f32::INFINITY, f32::min)
        };
        assert!(width(&wide) > width(&thin) + 0.004, "the welt stands out further");
    }
}
