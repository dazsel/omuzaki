//! The buckle straps: leather bands round the shaft that the buckles sit on (`boots.js` `buildStrap`).

use super::cuff::Band;
use super::profile::Boot;
use super::{Straps, Tess};
use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction, lerp};
use crate::kit::grid::MeshBuilder;

/// Boot-space height of the lowest and the highest strap's centre.
const STRAP_SPAN: (f64, f64) = (0.0595, 0.0880);
/// The highest strap is this much of the lowest one's height (the prototype's `0.0058 / 0.0060`).
const UPPER_STRAP_RATIO: f64 = 0.0058 / 0.0060;
/// Half a strap's thickness and how far its centre stands off the surface, metres.
const HALF_THICKNESS: f64 = 0.0012;
const STAND_OFF: f64 = 0.0016;
/// A strap's cross-section is nearly rectangular.
const EXPONENT: f64 = 3.6;
/// How sharply the strap's edges darken: the colour falls off as `|sin psi|` to this power.
const EDGE_POWER: i32 = 6;

/// One strap, as the buckle needs it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Strap {
    /// Boot-space height of its centre.
    pub yc: f64,
    /// Half its height, metres.
    pub half_height: f64,
    /// How far its outer face stands off the surface, metres.
    pub top_offset: f64,
}

/// The straps `spec` asks for, low to high: evenly spaced between the prototype's two, shrinking a little going up.
pub fn layout(spec: &Straps) -> Vec<Strap> {
    let n = usize::from(spec.count);
    (0..n)
        .map(|k| {
            let t = if n > 1 { fraction(k, n - 1) } else { 0.5 };
            let shrink = if n > 1 { lerp(1.0, UPPER_STRAP_RATIO, t) } else { 1.0 };
            Strap {
                yc: lerp(STRAP_SPAN.0, STRAP_SPAN.1, t),
                half_height: spec.height * shrink,
                top_offset: STAND_OFF + HALF_THICKNESS,
            }
        })
        .collect()
}

/// Adds the strap band to `builder`, shaded lighter on its face and darker at its edges.
pub(super) fn add(boot: &Boot, tess: &Tess, strap: &Strap, builder: &mut MeshBuilder) -> Result<(), KitError> {
    let band = Band {
        yc: strap.yc,
        half_height: strap.half_height,
        half_thickness: HALF_THICKNESS,
        stand_off: STAND_OFF,
        exponent: EXPONENT,
        segs: tess.strap,
    };
    let segi = tess.strap.1;
    let shade = |i: usize| {
        let psi = fraction(i, segi) * TAU;
        let k = 1.10 - 0.22 * psi.sin().abs().powi(EDGE_POWER);
        [(k * 0.86) as f32, (k * 0.83) as f32, (k * 0.80) as f32, 1.0]
    };
    band.add_to(boot, builder, &shade)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_straps_are_the_prototypes() {
        let straps = layout(&Straps { count: 2, height: 0.006 });
        assert_eq!(straps.len(), 2);
        assert!((straps[0].yc - 0.0595).abs() < 1e-12 && (straps[1].yc - 0.0880).abs() < 1e-12);
        assert!((straps[0].half_height - 0.0060).abs() < 1e-12 && (straps[1].half_height - 0.0058).abs() < 1e-12);
        assert!((straps[0].top_offset - 0.0028).abs() < 1e-12);
    }

    #[test]
    fn one_strap_sits_halfway_between_the_two_and_none_is_none() {
        let one = layout(&Straps { count: 1, height: 0.006 });
        assert_eq!(one.len(), 1);
        assert!(
            (one[0].yc - f64::midpoint(0.0595, 0.0880)).abs() < 1e-12 && (one[0].half_height - 0.006).abs() < 1e-12
        );
        assert!(layout(&Straps { count: 0, height: 0.006 }).is_empty(), "no straps");
        let three = layout(&Straps { count: 3, height: 0.006 });
        assert!(three.windows(2).all(|w| w[1].yc > w[0].yc && w[1].half_height < w[0].half_height));
    }
}
