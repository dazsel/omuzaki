//! Stitching and seams: dashed thread along the welt, the cuff, the back seam and the toe-cap seam, the dark seam
//! tubes, and the dash and disc primitives the strap tails reuse (`boots.js` `dash`, `disc`, `stitchLine`,
//! `buildStitching`, `buildSeams`).
//!
//! A stitch is a flat quad lifted [`LIFT`] off the leather, not a tube: it reads as thread at stream size for a fifth of
//! the triangles, and it can be laid along any rail by two points and a normal.

use omuzaki_core::glam::DVec3;

use super::cuff::{CUFF_CENTRE, CUFF_STAND_OFF};
use super::profile::Boot;
use super::{Stitching, Tess};
use crate::kit::KitError;
use crate::kit::curve::{TAU, clamp, fraction, lerp};
use crate::kit::grid::{GridOptions, MeshBuilder, Skin};
use crate::kit::sweep::tube;

/// How far a dash is lifted along its normal, metres: above the leather, never touching it.
const LIFT: f64 = 0.00002;
/// The shortest dash drawn, metres.
const MIN_DASH: f64 = 1e-6;
/// A thread's width, metres.
pub const THREAD_WIDTH: f64 = 0.00045;
/// How far stitch rows stand off the surface, metres.
const ROW_LIFT: f64 = 0.0003;
/// The stitch pitch the line table below is given for, metres: the welt row's.
pub const PITCH: f64 = 0.0046;
/// Points a hole is drawn with.
const DISC_POINTS: usize = 10;
/// The seam tubes' radius, metres, and the offset they stand at.
const SEAM_RADIUS: f64 = 0.00055;
const SEAM_LIFT: f64 = 0.0002;
/// The back seam's rows sit this far either side of the centre line, radians.
const BACK_SEAM_SPLIT: f64 = 0.07;
/// The toe-cap seam reaches this far round either side of the toe, radians, and its rows this far either side of it, metres.
const TOE_CAP_REACH: f64 = 1.25;
const TOE_CAP_SPLIT: f64 = 0.0030;
/// The toe-cap seam's height over the toe, boot space: `base + rise * cos(phi)^2` for `cos > 0`.
const TOE_CAP_BASE: f64 = 0.028;
const TOE_CAP_RISE: f64 = 0.019;
/// The seam lines' ends, boot-space heights: the back seam's tube and its stitches.
const BACK_SEAM: (f64, f64) = (0.016, 0.093);
const BACK_SEAM_TUBE: (f64, f64) = (0.014, 0.097);
/// Where the cuff's stitch row sits relative to the cuff: this far above its centre, as a fraction of its half height,
/// and this far inside the roll's outer face, metres.
const CUFF_ROW_RISE: f64 = 0.45;
const CUFF_ROW_INSET: f64 = 0.0002;
/// Where the welt's row sits, boot-space height.
const WELT_ROW: f64 = 0.0150;
/// The finite-difference steps of the surface normal, metres and radians.
const NORMAL_STEP: (f64, f64) = (0.0006, 0.01);

/// A dashed line: its nominal length, its pitch (both as the prototype gave them) and its dash length, metres.
#[derive(Copy, Clone, Debug)]
struct Line {
    len: f64,
    pitch: f64,
    dash: f64,
}

const WELT: Line = Line { len: 0.30, pitch: 0.0046, dash: 0.0028 };
const CUFF: Line = Line { len: 0.26, pitch: 0.0050, dash: 0.0030 };
const BACK: Line = Line { len: 0.077, pitch: 0.0046, dash: 0.0028 };
const TOE_CAP: Line = Line { len: 0.105, pitch: 0.0044, dash: 0.0027 };

impl Line {
    /// How many dashes the line has at a pitch `scale` times the prototype's (`Math.round`, at least 2). The length is
    /// the nominal one, not measured on the curve, so the pitch is only approximate on the real boot.
    fn count(&self, scale: f64) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small positive count
        let n = (self.len / (self.pitch * scale)).round() as usize;
        n.max(2)
    }
}

/// The number of dashes the boot's stitch rows have, for the pitch of `spec`, with the toe-cap rows or without.
pub fn dash_count(spec: &Stitching, cap: bool) -> usize {
    let scale = spec.pitch / PITCH;
    [(WELT, 1), (CUFF, 1), (BACK, 2), (TOE_CAP, if cap { 2 } else { 0 })]
        .iter()
        .map(|(line, copies)| line.count(scale) * copies)
        .sum()
}

// --------------------------------------------------------------------------------------------------- primitives

/// Adds one dash from `a` to `b`, `w` wide, flat on a surface with unit normal `nrm`, skinned like the boot at its middle.
///
/// # Errors
/// As [`MeshBuilder::add_polygon`].
pub(super) fn dash(
    boot: &Boot,
    builder: &mut MeshBuilder,
    (a, b): (DVec3, DVec3),
    w: f64,
    nrm: DVec3,
) -> Result<(), KitError> {
    let d = b - a;
    if d.length() < MIN_DASH {
        return Ok(());
    }
    let side = d.normalize().cross(nrm).normalize_or_zero() * (w / 2.0);
    let lift = nrm * LIFT;
    let quad = [a - side + lift, a + side + lift, b + side + lift, b - side + lift];
    builder.add_polygon(&quad, nrm, [1.0; 4], &boot.skin((a + b) / 2.0))
}

/// Adds a flat disc of radius `r` at `c` on a surface with unit normal `nrm` (a hole).
///
/// # Errors
/// As [`MeshBuilder::add_polygon`].
pub(super) fn disc(boot: &Boot, builder: &mut MeshBuilder, c: DVec3, nrm: DVec3, r: f64) -> Result<(), KitError> {
    let up = if nrm.y.abs() < 0.9 { DVec3::Y } else { DVec3::X };
    let t1 = nrm.cross(up).normalize_or_zero();
    let t2 = nrm.cross(t1);
    let ring: Vec<DVec3> = (0..DISC_POINTS)
        .map(|k| {
            let a = fraction(k, DISC_POINTS) * TAU;
            c + (t1 * a.cos() + t2 * a.sin()) * r
        })
        .collect();
    builder.add_polygon(&ring, nrm, [1.0; 4], &boot.skin(c))
}

/// A point of a path and the surface normal there.
type Station = (DVec3, DVec3);

/// Dashes along `path(t)`, `t` in 0..=1: `n` of them, each `line.dash` long, centred in its share of the path and lying
/// on the normal at its start. The path's length is taken as the line's nominal one.
fn stitch_line(
    boot: &Boot,
    builder: &mut MeshBuilder,
    path: &dyn Fn(f64) -> Station,
    line: &Line,
    scale: f64,
) -> Result<(), KitError> {
    let n = line.count(scale);
    let half = line.dash / line.len / 2.0;
    for k in 0..n {
        let mid = (real(k) + 0.5) / real(n);
        let (a, b) = (path(clamp(mid - half, 0.0, 1.0)), path(clamp(mid + half, 0.0, 1.0)));
        dash(boot, builder, (a.0, b.0), THREAD_WIDTH, a.1)?;
    }
    Ok(())
}

/// `n` as a float; dash counts are tiny.
fn real(n: usize) -> f64 {
    fraction(n, 1)
}

// -------------------------------------------------------------------------------------------------------- rows

/// The toe-cap seam's height at angle `phi`, boot space: an arch over the vamp, lower at the sides.
pub fn toe_cap_height(phi: f64) -> f64 {
    TOE_CAP_BASE + TOE_CAP_RISE * phi.cos().max(0.0).powi(2)
}

/// The outward unit normal of the upper at `dy` metres above boot-space height `yc`, angle `phi`, from finite differences.
fn surface_normal(boot: &Boot, yc: f64, dy: f64, phi: f64) -> DVec3 {
    let at = |dy: f64, phi: f64| boot.plan_at(yc, dy, phi, 0.0).p;
    let (up, round) = NORMAL_STEP;
    let n = (at(dy + up, phi) - at(dy - up, phi)).cross(at(dy, phi + round) - at(dy, phi - round)).normalize_or_zero();
    if n.dot(boot.plan_at(yc, dy, phi, 0.0).n) < 0.0 { -n } else { n }
}

/// The station at fraction `t` of a row all the way round the boot at boot-space height `yc`, `off` out from it.
fn circle(boot: &Boot, yc: f64, off: f64, t: f64) -> Station {
    let p = boot.plan(yc, lerp(0.0, TAU, t), off + ROW_LIFT);
    (p.p, p.n)
}

/// Adds the stitch rows: the welt all the way round, the cuff, the back seam in two rows, and (with `cap`) the toe-cap
/// seam in two rows. `cuff` is the cuff's height and fold, which the cuff row follows.
pub(super) fn add_rows(
    boot: &Boot,
    spec: &Stitching,
    cuff: (f64, f64),
    cap: bool,
    builder: &mut MeshBuilder,
) -> Result<(), KitError> {
    let scale = spec.pitch / PITCH;
    stitch_line(boot, builder, &|t| circle(boot, WELT_ROW, 0.0, t), &WELT, scale)?;
    let (cuff_row, cuff_off) =
        (CUFF_CENTRE + CUFF_ROW_RISE * cuff.0, boot.cuff_stand_off(cuff.1, CUFF_STAND_OFF) + cuff.1 - CUFF_ROW_INSET);
    stitch_line(boot, builder, &|t| circle(boot, cuff_row, cuff_off, t), &CUFF, scale)?;
    for split in [-BACK_SEAM_SPLIT, BACK_SEAM_SPLIT] {
        let path = |t: f64| {
            let p = boot.plan(lerp(BACK_SEAM.0, BACK_SEAM.1, t), std::f64::consts::PI + split, ROW_LIFT);
            (p.p, p.n)
        };
        stitch_line(boot, builder, &path, &BACK, scale)?;
    }
    if cap {
        for dy in [-TOE_CAP_SPLIT, TOE_CAP_SPLIT] {
            let path = |t: f64| {
                let phi = lerp(-TOE_CAP_REACH, TOE_CAP_REACH, t);
                let yc = toe_cap_height(phi);
                (boot.plan_at(yc, dy, phi, ROW_LIFT).p, surface_normal(boot, yc, dy, phi))
            };
            stitch_line(boot, builder, &path, &TOE_CAP, scale)?;
        }
    }
    Ok(())
}

/// Adds the dark seam tubes: the toe-cap seam's centre line (with `cap`) and the back seam's.
pub(super) fn add_seams(boot: &Boot, tess: &Tess, cap: bool, builder: &mut MeshBuilder) -> Result<(), KitError> {
    let mut tubes: Vec<Vec<DVec3>> = Vec::new();
    if cap {
        tubes.push(
            (0..=tess.seam_toe)
                .map(|k| {
                    let phi = lerp(-TOE_CAP_REACH, TOE_CAP_REACH, fraction(k, tess.seam_toe));
                    boot.plan(toe_cap_height(phi), phi, SEAM_LIFT).p
                })
                .collect(),
        );
    }
    tubes.push(
        (0..=tess.seam_back)
            .map(|k| {
                let y = lerp(BACK_SEAM_TUBE.0, BACK_SEAM_TUBE.1, fraction(k, tess.seam_back));
                boot.plan(y, std::f64::consts::PI, SEAM_LIFT).p
            })
            .collect(),
    );
    for points in &tubes {
        let swept = tube(points, |_| SEAM_RADIUS, tess.seam_seg)?;
        let inside = |i: usize, _: usize| swept.centres[i];
        let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);
        let options =
            GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Per(&skin), ..GridOptions::default() };
        builder.add_grid(&swept.grid, &options)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outfit::boots::profile::{BootProfile, Shape};

    fn boot() -> Boot {
        let flat = |v: f64| vec![[0.0, v], [0.12, v]];
        let profile = BootProfile { cx: flat(0.0), zb: flat(-0.04), zf: flat(0.06), rx: flat(0.03) };
        Boot::explicit(&profile, Shape { widen: 0.075, squareness: 2.65 }, 0.0115).unwrap()
    }

    #[test]
    fn a_dash_is_a_flat_quad_lifted_off_the_surface_along_its_normal() {
        let mut b = MeshBuilder::new();
        dash(&boot(), &mut b, (DVec3::ZERO, DVec3::new(0.003, 0.0, 0.0)), 0.00045, DVec3::Z).unwrap();
        let g = b.mesh();
        assert_eq!((g.positions.len(), g.triangle_count()), (4, 2));
        for p in &g.positions {
            assert!((f64::from(p.z) - LIFT).abs() < 1e-9, "lifted by {}", p.z);
            assert!(f64::from(p.y).abs() <= 0.00045 / 2.0 + 1e-9, "half the width either side: {}", p.y);
        }
        assert!(g.normals.iter().all(|n| (n.z - 1.0).abs() < 1e-6), "flat shaded on the surface normal");
    }

    #[test]
    fn a_dash_shorter_than_a_micrometre_draws_nothing() {
        let mut b = MeshBuilder::new();
        dash(&boot(), &mut b, (DVec3::ZERO, DVec3::new(1e-7, 0.0, 0.0)), 0.00045, DVec3::Z).unwrap();
        assert_eq!(b.triangle_count(), 0);
    }

    #[test]
    fn a_disc_is_a_ten_gon_of_the_radius_asked_for() {
        let mut b = MeshBuilder::new();
        disc(&boot(), &mut b, DVec3::new(0.0, 0.05, 0.0), DVec3::Z, 0.001).unwrap();
        let g = b.mesh();
        assert_eq!((g.positions.len(), g.triangle_count()), (10, 8));
        for p in &g.positions {
            assert!((p.as_dvec3() - DVec3::new(0.0, 0.05, 0.0)).length() - 0.001 < 1e-7);
        }
    }

    #[test]
    fn the_rows_have_the_prototypes_dash_counts_at_its_pitch() {
        let spec = Stitching::default();
        assert_eq!(WELT.count(1.0), 65);
        assert_eq!(CUFF.count(1.0), 52);
        assert_eq!(BACK.count(1.0), 17);
        assert_eq!(TOE_CAP.count(1.0), 24);
        assert_eq!(dash_count(&spec, true), 65 + 52 + 2 * 17 + 2 * 24);
        assert_eq!(dash_count(&spec, false), 65 + 52 + 2 * 17, "no toe cap, no cap rows");
        let coarse = Stitching { pitch: 0.0092, ..Stitching::default() };
        assert!(dash_count(&coarse, true) < dash_count(&spec, true) / 2 + 8, "a longer pitch has fewer stitches");
    }

    #[test]
    fn a_row_built_has_the_counted_dashes() {
        let boot = boot();
        let mut b = MeshBuilder::new();
        add_rows(&boot, &Stitching::default(), (0.01, 0.0034), true, &mut b).unwrap();
        assert_eq!(b.triangle_count(), 2 * 199);
        let mut plain = MeshBuilder::new();
        add_rows(&boot, &Stitching::default(), (0.01, 0.0034), false, &mut plain).unwrap();
        assert_eq!(plain.triangle_count(), 2 * dash_count(&Stitching::default(), false));
    }

    #[test]
    fn the_toe_cap_seam_arches_over_the_vamp() {
        assert!((toe_cap_height(0.0) - 0.047).abs() < 1e-12);
        assert!((toe_cap_height(std::f64::consts::FRAC_PI_2) - 0.028).abs() < 1e-12);
        assert!(
            (toe_cap_height(std::f64::consts::PI) - 0.028).abs() < 1e-12,
            "behind the toe it stays at the sides' height"
        );
    }

    #[test]
    fn the_seam_tubes_are_thin_dark_lines_lying_on_the_boot() {
        let boot = boot();
        let tess = Tess::OFFLINE;
        let mut b = MeshBuilder::new();
        add_seams(&boot, &tess, true, &mut b).unwrap();
        assert_eq!(b.vertex_count(), 31 * 6 + 17 * 6);
        let mut plain = MeshBuilder::new();
        add_seams(&boot, &tess, false, &mut plain).unwrap();
        assert_eq!(plain.vertex_count(), 17 * 6, "only the back seam without a toe cap");
    }
}
