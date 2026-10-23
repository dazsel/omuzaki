//! The turned-down cuff, and the closed band every strap and the cuff are made from (`boots.js` `band`, `buildCuff`).

use omuzaki_core::glam::DVec3;

use super::Tess;
use super::profile::Boot;
use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction, sup};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};

/// Where the cuff's roll is centred, boot-space height (the prototype's `yc`).
pub const CUFF_CENTRE: f64 = 0.1095;
/// How far out the roll's centre stands from the boot's surface, metres.
pub const CUFF_STAND_OFF: f64 = 0.0012;
/// The roll's cross-section exponent: a rounded rectangle.
const CUFF_EXPONENT: f64 = 2.6;
/// How much darker the underside of the roll is, at the darkest.
const CUFF_UNDERSIDE: f64 = 0.12;

/// A closed band round the boot: a rounded-rectangle cross-section swept once round the outline.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Band {
    /// Boot-space height of the band's centre.
    pub yc: f64,
    /// Half its height, metres.
    pub half_height: f64,
    /// Half its thickness, metres.
    pub half_thickness: f64,
    /// How far its centre stands off the surface, metres.
    pub stand_off: f64,
    /// The cross-section's exponent (2 is an ellipse; more is squarer).
    pub exponent: f64,
    /// Segments round the boot, and round the cross-section.
    pub segs: (usize, usize),
}

impl Band {
    /// The band's points: row `i` is a point of the cross-section, column `j` an angle round the boot. Both close.
    pub fn grid(&self, boot: &Boot) -> Result<Grid, KitError> {
        let (segj, segi) = self.segs;
        Grid::from_fn(segi + 1, segj + 1, |i, j| {
            let psi = fraction(i, segi) * TAU;
            let dy = self.half_height * sup(psi.sin(), self.exponent);
            let dr = self.stand_off + self.half_thickness * sup(psi.cos(), self.exponent);
            boot.plan_at(self.yc, dy, fraction(j, segj) * TAU, dr).p
        })
    }

    /// Adds the band to `builder`, row `i` coloured by `colour(i)`.
    pub fn add_to(
        &self,
        boot: &Boot,
        builder: &mut MeshBuilder,
        colour: &dyn Fn(usize) -> [f32; 4],
    ) -> Result<(), KitError> {
        let grid = self.grid(boot)?;
        let segj = self.segs.0;
        let inside = |_: usize, j: usize| boot.plan(self.yc, fraction(j, segj) * TAU, self.stand_off).p;
        let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);
        let paint = |i: usize, _: usize, _: DVec3| colour(i);
        let options = GridOptions {
            wrap_i: true,
            wrap_j: true,
            inside: Some(&inside),
            skin: Skin::Per(&skin),
            colour: Some(&paint),
            ..GridOptions::default()
        };
        builder.add_grid(&grid, &options)
    }
}

/// Adds the cuff to `builder`: a roll of leather round the top of the shaft, darker on its underside.
pub(super) fn add(boot: &Boot, tess: &Tess, height: f64, fold: f64, builder: &mut MeshBuilder) -> Result<(), KitError> {
    let band = Band {
        yc: CUFF_CENTRE,
        half_height: height,
        half_thickness: fold,
        stand_off: boot.cuff_stand_off(fold, CUFF_STAND_OFF),
        exponent: CUFF_EXPONENT,
        segs: tess.cuff,
    };
    let segi = tess.cuff.1;
    let shade = |i: usize| {
        let psi = fraction(i, segi) * TAU;
        let k = (1.0 - CUFF_UNDERSIDE * (-psi.sin()).max(0.0)) as f32;
        [k, k, k, 1.0]
    };
    band.add_to(boot, builder, &shade)
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
    fn a_band_is_a_closed_torus_that_stands_out_by_its_thickness() {
        let band =
            Band { yc: 0.1, half_height: 0.01, half_thickness: 0.003, stand_off: 0.001, exponent: 2.6, segs: (24, 12) };
        let boot = boot();
        let grid = band.grid(&boot).unwrap();
        assert_eq!((grid.rows(), grid.cols()), (13, 25));
        assert!((grid.at(12, 5) - grid.at(0, 5)).length() < 1e-9 && (grid.at(3, 24) - grid.at(3, 0)).length() < 1e-9);
        let outer = boot.plan(0.1, 0.0, 0.004).p;
        assert!((grid.at(0, 0) - outer).length() < 1e-12, "the cross-section starts on the outer face");
        let (lo, hi) =
            (0..13).map(|i| grid.at(i, 0).y).fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), y| (l.min(y), h.max(y)));
        assert!((hi - lo - 0.02).abs() < 1e-6, "{} tall", hi - lo);
    }

    #[test]
    fn the_cuff_is_a_closed_roll_darker_on_its_underside() {
        let (boot, tess) = (boot(), Tess::OFFLINE);
        let mut b = MeshBuilder::with_colours();
        add(&boot, &tess, 0.01, 0.0034, &mut b).unwrap();
        assert_eq!(b.triangle_count(), 2880);
        let colours = &b.mesh().colours;
        let (lightest, darkest) = colours.iter().fold((0.0_f32, 1.0_f32), |(l, d), c| (l.max(c[0]), d.min(c[0])));
        assert!((lightest - 1.0).abs() < 1e-6 && (darkest - 0.88).abs() < 1e-3, "{lightest}..{darkest}");
    }
}
