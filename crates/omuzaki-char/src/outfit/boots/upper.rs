//! The leather upper: an open tube over the foot and shaft, with ankle creases (`boots.js` `buildUpper`, `creaseAt`).

use omuzaki_core::glam::DVec3;

use super::Tess;
use super::profile::Boot;
use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction, rows_of, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};

/// The boot-space height of the upper's top; the cuff covers it.
pub const UPPER_TOP: f64 = 0.108;
/// Ankle creases: `(boot-space height, depth in metres)`. They dent the front of the boot inward.
pub const CREASES: [(f64, f64); 2] = [(0.0725, 0.00075), (0.0775, 0.00065)];
/// A crease's half width, boot-space metres (the dent), and the width of its shadow in the vertex colours.
const CREASE_WIDTH: f64 = 0.0017;
const CREASE_SHADOW_WIDTH: f64 = 0.0014;
/// The darkest the contact shadow above the sole goes, and how high it fades out.
const CONTACT: (f64, f64, f64) = (0.80, 0.003, 0.028);
/// The shadow under the cuff: strength, centre, width.
const UNDER_CUFF: (f64, f64, f64) = (0.20, 0.0935, 0.005);
/// How much darker the inner side (facing the other boot) is.
const INNER_SIDE: f64 = 0.07;
/// The polished toe cap: strength, centre height, width.
const TOE_POLISH: (f64, f64, f64) = (0.07, 0.028, 0.012);

/// How deep the creases dent the surface at `(y, phi)`, `scale` times the prototype's: only on the front, fading out to the sides.
pub fn crease_at(y: f64, phi: f64, scale: f64) -> f64 {
    let front = sstep(-0.2, 0.55, phi.cos());
    let depth: f64 = CREASES.iter().map(|&(yc, a)| a * (-((y - yc) / CREASE_WIDTH).powi(2)).exp()).sum();
    scale * depth * front
}

/// The grey the vertex at height `y` and angle `phi` is painted: the contact shadow, the shadow under the cuff, the
/// creases' shadows, the darker inner side and the polish of the toe cap, multiplied.
fn shade(y: f64, phi: f64, creases: bool) -> f64 {
    let (cph, sph) = (phi.cos(), phi.sin());
    let mut k = 1.0;
    k *= CONTACT.0 + (1.0 - CONTACT.0) * sstep(CONTACT.1, CONTACT.2, y);
    k *= 1.0 - UNDER_CUFF.0 * (-((y - UNDER_CUFF.1) / UNDER_CUFF.2).powi(2)).exp();
    if creases {
        for &(yc, _) in &CREASES {
            k *= 1.0 - 0.20 * (-((y - yc) / CREASE_SHADOW_WIDTH).powi(2)).exp() * sstep(-0.2, 0.55, cph);
        }
    }
    k *= 1.0 - INNER_SIDE * (-sph).max(0.0);
    k *= 1.0 + TOE_POLISH.0 * (-((y - TOE_POLISH.1) / TOE_POLISH.2).powi(2)).exp() * sstep(0.6, 1.0, cph);
    k
}

/// The boot-space heights of the upper's rows.
pub fn rows(tess: &Tess) -> Vec<f64> {
    let [a, b, c] = tess.upper_rows;
    rows_of(&[(0.0, 0.016, a), (0.016, 0.05, b), (0.05, UPPER_TOP, c)])
}

/// Adds the upper to `builder`: an open tube with a wrapped seam, no caps.
pub(super) fn add(boot: &Boot, tess: &Tess, creases: bool, builder: &mut MeshBuilder) -> Result<(), KitError> {
    let ys = rows(tess);
    let seg = tess.upper_seg;
    let dent = if creases { boot.crease_scale() } else { 0.0 };
    let space = boot.space();
    let grid = Grid::from_fn(ys.len(), seg + 1, |i, j| {
        let phi = fraction(j, seg) * TAU;
        let p = boot.plan(ys[i], phi, -crease_at(ys[i], phi, dent));
        DVec3::new(p.p.x, space.up(ys[i]), p.p.z)
    })?;
    let inside = |i: usize, _: usize| {
        let s = boot.secs(ys[i]);
        DVec3::new(s.cx, space.up(ys[i]), s.cz)
    };
    let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);
    let paint = |i: usize, j: usize, _: DVec3| {
        let k = shade(ys[i], fraction(j, seg) * TAU, creases) as f32;
        [k, k, k, 1.0]
    };
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Per(&skin),
        colour: Some(&paint),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creases_dent_the_front_at_their_heights_and_nowhere_else() {
        let (y, depth) = CREASES[0];
        assert!((crease_at(y, 0.0, 1.0) - depth).abs() < depth * 0.05, "full depth on the toe side");
        assert_eq!(crease_at(y, std::f64::consts::PI, 1.0), 0.0, "none at the heel");
        assert!(crease_at(0.04, 0.0, 1.0) < 1e-9, "none between them");
        assert_eq!(crease_at(y, 0.0, 0.0), 0.0, "and none when switched off");
    }

    #[test]
    fn the_rows_are_the_prototypes_42_and_end_at_the_top() {
        let rows = rows(&Tess::OFFLINE);
        assert_eq!(rows.len(), 42);
        assert!(rows[0].abs() < 1e-12 && (rows[41] - UPPER_TOP).abs() < 1e-12);
        assert!(rows.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn the_shading_darkens_above_the_sole_under_the_cuff_and_on_the_inner_side() {
        let k = |y: f64, phi: f64| shade(y, phi, true);
        assert!(k(0.0, 0.0) < k(0.05, 0.0), "contact shadow at the sole");
        assert!(k(UNDER_CUFF.1, 0.0) < k(0.085, 0.0), "shadow under the cuff");
        assert!(k(0.05, -std::f64::consts::FRAC_PI_2) < k(0.05, std::f64::consts::FRAC_PI_2), "inner side darker");
        assert!(k(TOE_POLISH.1, 0.0) > k(0.07, 0.0) * 0.95, "the toe cap is polished");
        assert!(k(CREASES[0].0, 0.0) < shade(CREASES[0].0, 0.0, false), "creases shade the front");
    }
}
