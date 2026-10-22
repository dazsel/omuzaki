//! The brass buckles and the strap tails that pass through them (`boots.js` `planarLoop`, `buildBuckle`).
//!
//! A buckle sits on its strap at the front-outside of the boot ([`BUCKLE_PHI`]): a rounded-rectangle brass frame, a
//! prong on its hinge bar, and a tongue of leather lying over the strap toward the toe with a row of holes and edge
//! stitching.

use omuzaki_core::glam::DVec3;

use super::profile::Boot;
use super::stitch::{dash, disc};
use super::strap::Strap;
use super::{Builders, Tess};
use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction, lerp};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::sweep::tube;

/// The angle round the boot the buckles sit at (front, toward the outside).
pub const BUCKLE_PHI: f64 = 1.02;
/// The frame's outer width (along the strap) and height, corner radius and wire radius, metres.
const FRAME: (f64, f64, f64, f64) = (0.0108, 0.0166, 0.0026, 0.00100);
/// How far the frame floats above the strap's face, and the base point's offset, metres.
const FRAME_LIFT: f64 = 0.0014;
const BASE_LIFT: f64 = 0.0009;
/// Points on each rounded corner.
const CORNER_POINTS: usize = 6;
/// The prong: radius, how far in from the frame's ends it starts, and its path's rise (above the strap) at the start,
/// the arch's height and the slope of the tip, metres.
const PRONG_RADIUS: f64 = 0.00085;
const PRONG_INSET: f64 = 0.0012;
const PRONG_RISE: (f64, f64, f64) = (0.0015, 0.0012, 0.0004);
/// Points along the prong.
const PRONG_POINTS: usize = 10;
/// The hinge's ball radius, metres.
const HINGE_RADIUS: f64 = 0.0016;
/// The tail's length, thickness, and the strap's height it takes, metres, as a fraction.
const TAIL_LENGTH: f64 = 0.0200;
const TAIL_THICKNESS: f64 = 0.0018;
const TAIL_HEIGHT: f64 = 0.92;
/// The tail rounds off over this much of its end, metres.
const TAIL_TIP: f64 = 0.0060;
/// The tail turns toward the toe by arc length over this radius (an approximate boot radius), metres.
const TAIL_RADIUS: f64 = 0.038;
/// The tail starts this far behind the frame's centre, metres.
const TAIL_START: f64 = 0.001;
/// The tail's face sits this much over its walls to keep them from fighting, metres.
const FACE_LIFT: f64 = 0.00004;
/// Edge stitches along the tail: where they start and stop short of its ends, their pitch, the dash's half length, their
/// inset from the edge and the least they may be from the centre, their lift and their width, metres.
const EDGE_STITCH: EdgeStitch = EdgeStitch {
    margin: 0.0012,
    pitch: 0.0030,
    half: 0.0009,
    inset: 0.0017,
    least: 0.0008,
    lift: 0.0003,
    width: 0.00032,
};
/// Holes: the first one's distance along the tail, their spacing, count and radius, metres.
const HOLES: (f64, f64, usize, f64) = (0.0075, 0.0036, 3, 0.00115);
/// The tail's face and wall colours.
const FACE: [f32; 4] = [0.98, 0.94, 0.90, 1.0];
const WALL: [f32; 4] = [0.80, 0.76, 0.72, 1.0];

#[derive(Copy, Clone)]
struct EdgeStitch {
    margin: f64,
    pitch: f64,
    half: f64,
    inset: f64,
    least: f64,
    lift: f64,
    width: f64,
}

/// The distances along a tail of strap half-height `half_height` that carry edge stitches, with the tail's half width there.
pub fn edge_stitches(half_height: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut s = EDGE_STITCH.margin;
    while s < TAIL_LENGTH - EDGE_STITCH.margin {
        let v = tail_half_width(half_height, s) - EDGE_STITCH.inset;
        if v > EDGE_STITCH.least {
            out.push((s, v));
        }
        s += EDGE_STITCH.pitch;
    }
    out
}

/// How many boundary loops one buckle's meshes have: the tail's face, its two walls (which meet at the tip, so welded
/// they are one loop), the prong's two open ends, a dash per edge stitch on each side and a disc per hole.
pub fn open_borders(half_height: f64) -> usize {
    2 + 2 + 2 * edge_stitches(half_height).len() + HOLES.2
}

/// The tail's half width at `s` along it: the strap's, rounding off over the last [`TAIL_TIP`].
fn tail_half_width(half_height: f64, s: f64) -> f64 {
    let full = half_height * TAIL_HEIGHT;
    let t = s - (TAIL_LENGTH - TAIL_TIP);
    if t <= 0.0 { full } else { full * (1.0 - (t / TAIL_TIP).powi(2)).max(0.0).sqrt() }
}

/// A closed tube along the planar loop `points`, `r` thick with `seg` segments round, its rings square to the loop's
/// plane (normal `nrm`) so it doesn't twist. Row `k` is the ring at point `k`; the last row repeats the first.
fn planar_loop(points: &[DVec3], nrm: DVec3, r: f64, seg: usize) -> Result<(Grid, Vec<DVec3>), KitError> {
    let n = points.len();
    let grid = Grid::from_fn(n + 1, seg + 1, |k, j| {
        let (p, prev, next) = (points[k % n], points[(k + n - 1) % n], points[(k + 1) % n]);
        let tangent = (next - prev).normalize_or_zero();
        let m = nrm.cross(tangent).normalize_or_zero();
        let a = fraction(j, seg) * TAU;
        p + (nrm * a.cos() + m * a.sin()) * r
    })?;
    let centres = (0..=n).map(|k| points[k % n]).collect();
    Ok((grid, centres))
}

/// The brass frame's loop in the strap's plane: a rounded rectangle, 24 points from the first corner counter-clockwise.
fn frame_loop() -> Vec<(f64, f64)> {
    let (w, h, corner, _) = FRAME;
    let (hw, hh) = (w / 2.0 - corner, h / 2.0 - corner);
    let mut out = Vec::with_capacity(4 * CORNER_POINTS);
    for (cx, cy, start) in [
        (hw, hh, 0.0),
        (-hw, hh, std::f64::consts::FRAC_PI_2),
        (-hw, -hh, std::f64::consts::PI),
        (hw, -hh, 1.5 * std::f64::consts::PI),
    ] {
        for k in 0..CORNER_POINTS {
            let a = start + fraction(k, CORNER_POINTS - 1) * std::f64::consts::FRAC_PI_2;
            out.push((cx + corner * a.cos(), cy + corner * a.sin()));
        }
    }
    out
}

/// Adds one buckle on `strap` to the boot's builders: brass frame, prong and hinge, and the leather tail with its stitches
/// and holes.
pub(super) fn add(boot: &Boot, tess: &Tess, strap: &Strap, out: &mut Builders) -> Result<(), KitError> {
    let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);
    let base = boot.plan_at(strap.yc, 0.0, BUCKLE_PHI, strap.top_offset + BASE_LIFT);
    let (b, n_h, up) = (base.p, base.n, DVec3::Y);
    let t_h = up.cross(n_h).normalize_or_zero();
    let (width, ..) = FRAME;

    let points: Vec<DVec3> = frame_loop().iter().map(|&(a, v)| b + t_h * a + up * v + n_h * FRAME_LIFT).collect();
    let (grid, centres) = planar_loop(&points, n_h, FRAME.3, tess.frame_seg)?;
    let inside = |i: usize, _: usize| centres[i];
    let options = GridOptions {
        wrap_i: true,
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    out.brass.add_grid(&grid, &options)?;

    let prong: Vec<DVec3> = (0..PRONG_POINTS)
        .map(|k| {
            let t = fraction(k, PRONG_POINTS - 1);
            let along = lerp(-width / 2.0 + PRONG_INSET, width / 2.0 - PRONG_INSET, t);
            let lift = PRONG_RISE.0 + PRONG_RISE.1 * (t * std::f64::consts::PI).sin() - PRONG_RISE.2 * t;
            b + t_h * along + n_h * lift
        })
        .collect();
    let swept = tube(&prong, |_| PRONG_RADIUS, tess.prong_seg)?;
    let inside = |i: usize, _: usize| swept.centres[i];
    let options = GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Per(&skin), ..GridOptions::default() };
    out.brass.add_grid(&swept.grid, &options)?;

    hinge(boot, tess, prong[0], &mut out.brass)?;
    tail(boot, tess, strap, width, out)
}

/// The prong's hinge: a small ball at `centre`.
fn hinge(boot: &Boot, tess: &Tess, centre: DVec3, brass: &mut MeshBuilder) -> Result<(), KitError> {
    let (ws, hs) = tess.hinge;
    let grid = Grid::from_fn(hs + 1, ws + 1, |i, j| {
        let (theta, phi) = (std::f64::consts::PI * fraction(i, hs), TAU * fraction(j % ws, ws));
        centre
            + DVec3::new(
                -HINGE_RADIUS * phi.cos() * theta.sin(),
                HINGE_RADIUS * theta.cos(),
                HINGE_RADIUS * phi.sin() * theta.sin(),
            )
    })?;
    let inside = |_: usize, _: usize| centre;
    let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);
    let options = GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Per(&skin), ..GridOptions::default() };
    brass.add_grid(&grid, &options)
}

/// The strap tail: a tongue of leather lying over the strap toward the toe, rounded at the end, with walls, edge
/// stitches and holes.
fn tail(boot: &Boot, tess: &Tess, strap: &Strap, frame_width: f64, out: &mut Builders) -> Result<(), KitError> {
    let (yc, top) = (strap.yc, strap.top_offset);
    // The tail runs toward decreasing phi, by arc length over the boot's approximate radius.
    let arc = |s: f64| BUCKLE_PHI - (s + frame_width / 2.0 - TAIL_START) / TAIL_RADIUS;
    let surf = |s: f64, v: f64, lift: f64| boot.plan_at(yc, v, arc(s), top + lift).p;
    let normal = |s: f64| boot.plan_at(yc, 0.0, arc(s), 0.0).n;
    let width_at = |s: f64| tail_half_width(strap.half_height, s);
    let (rows, cols) = (tess.tail_rows, tess.tail_cols);
    let ss: Vec<f64> = (0..=rows).map(|i| fraction(i, rows) * TAIL_LENGTH).collect();
    let reference = |i: usize, _: usize| boot.plan(yc, arc(ss[i]), 0.0).p;
    let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);

    let face = Grid::from_fn(rows + 1, cols + 1, |i, c| {
        let v = (fraction(c, cols) * 2.0 - 1.0) * width_at(ss[i]);
        surf(ss[i], v, TAIL_THICKNESS + FACE_LIFT)
    })?;
    let paint_face = |_: usize, _: usize, _: DVec3| FACE;
    let options = GridOptions {
        inside: Some(&reference),
        skin: Skin::Per(&skin),
        colour: Some(&paint_face),
        ..GridOptions::default()
    };
    out.leather.add_grid(&face, &options)?;

    let paint_wall = |_: usize, _: usize, _: DVec3| WALL;
    for side in [-1.0, 1.0] {
        let wall = Grid::from_fn(rows + 1, 2, |i, c| {
            let w = width_at(ss[i]);
            if c == 0 { surf(ss[i], side * w, TAIL_THICKNESS) } else { surf(ss[i], side * w * 0.995, 0.0) }
        })?;
        let options = GridOptions {
            inside: Some(&reference),
            skin: Skin::Per(&skin),
            colour: Some(&paint_wall),
            ..GridOptions::default()
        };
        out.leather.add_grid(&wall, &options)?;
    }

    for side in [-1.0, 1.0] {
        for (s, v) in edge_stitches(strap.half_height) {
            let lift = TAIL_THICKNESS + EDGE_STITCH.lift;
            let ends = (surf(s - EDGE_STITCH.half, side * v, lift), surf(s + EDGE_STITCH.half, side * v, lift));
            dash(boot, &mut out.thread, ends, EDGE_STITCH.width, normal(s))?;
        }
    }
    for k in 0..HOLES.2 {
        let s = HOLES.0 + fraction(k, 1) * HOLES.1;
        disc(boot, &mut out.holes, surf(s, 0.0, TAIL_THICKNESS + EDGE_STITCH.lift), normal(s), HOLES.3)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strap_of_the_prototypes_height_has_six_edge_stitches_a_side_and_a_tail_that_rounds_off() {
        let stitches = edge_stitches(0.006);
        assert_eq!(stitches.len(), 6);
        assert!((stitches[0].0 - 0.0012).abs() < 1e-12 && (stitches[1].0 - 0.0042).abs() < 1e-12);
        assert!(stitches.windows(2).all(|w| w[1].0 > w[0].0 && w[1].1 <= w[0].1 + 1e-12), "narrowing toward the tip");
        assert!((tail_half_width(0.006, 0.0) - 0.006 * TAIL_HEIGHT).abs() < 1e-12);
        assert!(tail_half_width(0.006, TAIL_LENGTH).abs() < 1e-12, "the tip closes");
        assert!(edge_stitches(0.0021).is_empty(), "a strap too narrow to stitch has none");
    }

    #[test]
    fn a_buckle_counts_its_loops() {
        assert_eq!(open_borders(0.006), 2 + 2 + 12 + 3);
    }

    #[test]
    fn the_frame_is_a_rounded_rectangle_of_the_size_asked_for() {
        let pts = frame_loop();
        assert_eq!(pts.len(), 24);
        let (w, h, ..) = FRAME;
        let x = pts.iter().map(|p| p.0);
        let (lo, hi) = (x.clone().fold(f64::INFINITY, f64::min), x.fold(f64::NEG_INFINITY, f64::max));
        assert!((hi - lo - w).abs() < 1e-12, "{} wide", hi - lo);
        let y = pts.iter().map(|p| p.1);
        assert!((y.clone().fold(f64::NEG_INFINITY, f64::max) - y.fold(f64::INFINITY, f64::min) - h).abs() < 1e-12);
    }

    #[test]
    fn a_planar_loop_is_a_closed_untwisted_tube() {
        let ring: Vec<DVec3> =
            (0..24).map(|k| DVec3::new((fraction(k, 24) * TAU).cos(), (fraction(k, 24) * TAU).sin(), 0.0)).collect();
        let (grid, centres) = planar_loop(&ring, DVec3::Z, 0.1, 7).unwrap();
        assert_eq!((grid.rows(), grid.cols(), centres.len()), (25, 8, 25));
        assert!((grid.at(24, 3) - grid.at(0, 3)).length() < 1e-12, "the last ring repeats the first");
        for (i, centre) in centres.iter().take(24).enumerate() {
            for j in 0..8 {
                assert!((grid.at(i, j).distance(*centre) - 0.1).abs() < 1e-12, "{i},{j}");
            }
        }
    }
}
