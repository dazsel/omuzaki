//! Natural hair colour (#2524): the hair's colours from how much melanin it holds and of which kind, with a dye and
//! greying over them.
//!
//! Real hair is coloured by two pigments: eumelanin (brown to black) and pheomelanin (red to yellow). Light passing
//! through a strand loses each channel by `exp(-sigma)`, `sigma` being the pigments' absorption times their amount,
//! plus the keratin's own faint yellow. The colour of a head of hair is that transmittance over a small surface
//! reflection ([`FLOOR`]). The amount of melanin grows as the square of [`HairPigment::melanin`], so the slider moves
//! evenly from platinum through blond and brown to black.
//!
//! The absorption spectra start from Chiang et al. 2016 ("A Practical and Controllable Hair and Fur Model for
//! Production Path Tracing") and are fitted, together with the scale, to nine swatches of common natural colours
//! (platinum, golden blond, light, medium and dark brown, black, red, auburn and strawberry blond) in sRGB, to within
//! about 8 levels on average. A dye adds its own absorption (`-ln` of its colour, times its strength), so it shows fully
//! on light hair and hardly at all on black, as a real dye does. Greying mixes toward unpigmented hair.
//!
//! From the one colour, [`natural`] makes the hair's toon colours: darker roots and lighter tips by
//! [`HairPigment::gradient`], a shadow tone that deepens the hue, strand lines and outline a shade darker, and a ring
//! and rim lit toward white.

use omuzaki_char_api::HairPigment;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;

/// Eumelanin's absorption per unit, linear RGB.
const EUMELANIN: Vec3 = Vec3::new(1.568, 1.360, 1.868);
/// Pheomelanin's absorption per unit, linear RGB.
const PHEOMELANIN: Vec3 = Vec3::new(1.152, 3.221, 4.580);
/// The keratin's own absorption: the faint yellow of unpigmented hair.
const KERATIN: Vec3 = Vec3::new(0.148, 0.330, 0.741);
/// Melanin units at `melanin` 1: the amount is this times `melanin` squared.
const SCALE: f32 = 3.45;
/// The surface's own reflection, which even black hair shows.
const FLOOR: f32 = 0.0117;
/// Unpigmented (grey or white) hair, linear.
const GREY: Vec3 = Vec3::new(0.69, 0.66, 0.60);

/// The colours [`natural`] makes, sRGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NaturalColours {
    /// At the root.
    pub root: Colour,
    /// Along the lengths.
    pub mid: Colour,
    /// At the tips.
    pub tip: Colour,
    /// The shadow tone, a multiplier.
    pub shade: Colour,
    /// The strand lines.
    pub line: Colour,
    /// The ink outline.
    pub outline: Colour,
    /// The angel ring.
    pub ring: Colour,
    /// The rim light.
    pub rim: Colour,
}

/// The colour of hair with `pigment`, dyed `dye` (sRGB) if given, linear RGB.
#[must_use]
pub fn base(pigment: &HairPigment, dye: Option<Colour>) -> Vec3 {
    let melanin = narrow(pigment.melanin.clamp(0.0, 1.0));
    let red = narrow(pigment.redness.clamp(0.0, 1.0));
    let amount = SCALE * melanin * melanin;
    let mut sigma = amount * EUMELANIN.lerp(PHEOMELANIN, red) + KERATIN;
    if let Some(dye) = dye {
        let strength = narrow(pigment.dye_strength.clamp(0.0, 1.0));
        sigma += -dye.to_linear().max(Vec3::splat(0.02)).min(Vec3::ONE).ln() * strength;
    }
    let colour = Vec3::splat(FLOOR) + (1.0 - FLOOR) * (-sigma).exp();
    colour.lerp(GREY, narrow(pigment.grey.clamp(0.0, 1.0)))
}

/// The hair's colours for `pigment`, dyed `dye` (sRGB) if given.
#[must_use]
pub fn natural(pigment: &HairPigment, dye: Option<Colour>) -> NaturalColours {
    let mid = base(pigment, dye);
    let g = narrow(pigment.gradient.clamp(0.0, 1.0));
    // Roots hold more pigment (newer, unbleached); tips are sun-lightened and thinner.
    let root = mid * (1.0 - 0.35 * g);
    let tip = mid.lerp(Vec3::ONE, 0.18 * g) * (1.0 + 0.08 * g);
    let luma = mid.dot(Vec3::new(0.2126, 0.7152, 0.0722)).max(1e-4);
    // The shadow deepens the hue a little and leans cool, as painted hair shadows do: a multiplier that keeps luma
    // near 0.55 and saturates toward the hair's own colour.
    let hue = (mid / luma).clamp(Vec3::splat(0.2), Vec3::splat(2.5));
    let shade = (Vec3::new(0.56, 0.52, 0.60) * hue.powf(0.35)).min(Vec3::ONE);
    // Lines and outline: the hair a shade darker and richer; on dark hair, a touch lighter so they still read.
    let ink = |k: f32| (mid * k).max(Vec3::splat(0.012)).lerp(mid * 1.6, (0.02 - luma).max(0.0) * 30.0);
    let ring = mid.lerp(Vec3::ONE, 0.75).min(Vec3::ONE);
    let rim = mid.lerp(Vec3::ONE, 0.6).min(Vec3::ONE);
    let srgb = |v: Vec3| Colour::from_linear(v.clamp(Vec3::ZERO, Vec3::ONE), 1.0);
    NaturalColours {
        root: srgb(root),
        mid: srgb(mid),
        tip: srgb(tip),
        shade: srgb(shade),
        line: srgb(ink(0.45)),
        outline: srgb(ink(0.35)),
        ring: srgb(ring),
        rim: srgb(rim),
    }
}

#[allow(clippy::cast_possible_truncation)] // parameters checked to 0..=1
fn narrow(x: f64) -> f32 {
    x as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(melanin: f64, redness: f64) -> Colour {
        natural(&HairPigment { melanin, redness, ..HairPigment::default() }, None).mid
    }

    fn close(a: Colour, b: (u8, u8, u8), within: u8) -> bool {
        a.r.abs_diff(b.0) <= within && a.g.abs_diff(b.1) <= within && a.b.abs_diff(b.2) <= within
    }

    #[test]
    fn the_melanin_sweep_reads_as_natural_colours() {
        // Platinum, golden blond, light, medium and dark brown, black, red and auburn.
        for (m, r, want) in [
            (0.0, 0.3, (232, 220, 192)),
            (0.3, 0.35, (200, 164, 106)),
            (0.5, 0.35, (140, 104, 69)),
            (0.65, 0.3, (94, 66, 50)),
            (0.8, 0.25, (59, 42, 34)),
            (1.0, 0.2, (28, 23, 22)),
            (0.45, 0.95, (160, 70, 42)),
            (0.65, 0.85, (110, 52, 36)),
        ] {
            let got = at(m, r);
            assert!(close(got, want, 24), "melanin {m}, redness {r}: {got:?} against {want:?}");
        }
    }

    #[test]
    fn more_melanin_is_darker_and_more_redness_is_redder() {
        let luma = |c: Colour| c.to_linear().dot(Vec3::new(0.2126, 0.7152, 0.0722));
        let sweep: Vec<f32> = (0..=10).map(|i| luma(at(f64::from(i) / 10.0, 0.3))).collect();
        assert!(sweep.windows(2).all(|w| w[1] < w[0]), "{sweep:?}");
        let (ash, red) = (at(0.5, 0.0).to_linear(), at(0.5, 1.0).to_linear());
        assert!(red.x / red.z > 2.0 * ash.x / ash.z, "{ash} / {red}");
    }

    #[test]
    fn a_dye_shows_on_light_hair_and_hardly_on_black() {
        let blue = Some(Colour::rgb(0x30, 0x60, 0xe0));
        let shift = |m: f64| {
            let p = HairPigment { melanin: m, redness: 0.2, dye_strength: 1.0, ..HairPigment::default() };
            (base(&p, None) - base(&p, blue)).length()
        };
        assert!(shift(0.05) > 5.0 * shift(1.0), "light {} black {}", shift(0.05), shift(1.0));
        let dyed = base(&HairPigment { melanin: 0.05, dye_strength: 1.0, ..HairPigment::default() }, blue);
        assert!(dyed.z > dyed.x, "light hair dyed blue reads blue: {dyed}");
    }

    #[test]
    fn roots_are_darker_than_tips_and_grey_goes_grey() {
        let c = natural(&HairPigment::default(), None);
        let luma = |c: Colour| c.to_linear().dot(Vec3::new(0.2126, 0.7152, 0.0722));
        assert!(luma(c.root) < luma(c.mid) && luma(c.mid) < luma(c.tip), "{c:?}");
        let grey = natural(&HairPigment { melanin: 0.9, grey: 1.0, ..HairPigment::default() }, None).mid.to_linear();
        assert!((grey - GREY).length() < 0.02, "{grey}");
    }
}
