//! Colours at the file boundary: the palette's sRGB bytes and the scene's linear floats.
//!
//! An `IoScene` keeps glTF and `MToon` factors as linear `f32` so an import and re-export reproduces a file's numbers
//! (`docs/design/io-api.md`, D1 and Q10). The palette type [`Colour`] is sRGB with 8 bits per channel; the one
//! transfer function lives in `omuzaki-core`, so a palette colour looks the same in the renderer and in an exported VRM.
//! Alpha is linear in both and is carried as is.
//!
//! The direction matters: **8-bit to linear and back is exact** for every value, but linear to 8-bit rounds to the
//! nearest byte and clamps to `0..=1`, so a file factor that is not on the 8-bit grid (or an emissive above 1) does
//! not survive a trip through [`Colour`]. Convert to [`Colour`] only for values that become palette entries.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec4;

/// The linear-light colour of a palette colour: `(r, g, b)` decoded from sRGB, `w` the alpha.
pub fn to_linear(colour: Colour) -> Vec4 {
    colour.to_linear().extend(colour.alpha())
}

/// The palette colour nearest to a linear one (components clamped to `0..=1`, rounded to the nearest byte).
pub fn from_linear(linear: Vec4) -> Colour {
    Colour::from_linear(linear.truncate(), linear.w)
}

#[cfg(test)]
mod tests {
    use omuzaki_core::rng::Rng;

    use super::*;

    fn round_trip(colour: Colour) -> Colour {
        from_linear(to_linear(colour))
    }

    #[test]
    fn every_byte_of_every_channel_round_trips() {
        // Each channel is converted on its own, so sweeping all 256 values on each of the four channels (with the
        // others at a different value, to catch any crosstalk) covers every one of the 256^3 colours and alphas.
        for v in 0..=255u8 {
            let others = 255 - v;
            for colour in [
                Colour::rgba(v, others, v / 2, others),
                Colour::rgba(others, v, others / 2, v),
                Colour::rgba(v / 2, others, v, v),
                Colour::rgba(others, v / 3, others, v),
            ] {
                assert_eq!(round_trip(colour), colour);
            }
        }
    }

    #[test]
    fn the_whole_colour_cube_round_trips_on_a_lattice_and_at_its_corners() {
        let steps: Vec<u8> = (0..=255).step_by(15).collect();
        assert_eq!(steps.last(), Some(&255), "the lattice includes the far corner");
        for &r in &steps {
            for &g in &steps {
                for &b in &steps {
                    let colour = Colour::rgb(r, g, b);
                    assert_eq!(round_trip(colour), colour);
                }
            }
        }
        for corner in 0..8u8 {
            let bit = |n: u8| if corner >> n & 1 == 1 { 255 } else { 0 };
            let colour = Colour::rgba(bit(0), bit(1), bit(2), 255);
            assert_eq!(round_trip(colour), colour);
        }
    }

    #[test]
    fn ten_thousand_random_colours_round_trip() {
        let mut rng = Rng::new(0x1055);
        for _ in 0..10_000 {
            let [r, g, b, a, ..] = rng.next_u64().to_le_bytes();
            let colour = Colour::rgba(r, g, b, a);
            assert_eq!(round_trip(colour), colour);
        }
    }

    #[test]
    fn linear_values_are_decoded_not_just_scaled() {
        let mid = to_linear(Colour::rgba(128, 128, 128, 128));
        assert!((mid.x - 0.2158).abs() < 1e-3, "sRGB 128 is about 0.216 in linear light, got {}", mid.x);
        assert!((mid.w - 128.0 / 255.0).abs() < 1e-6, "alpha is not gamma encoded");
        assert_eq!(to_linear(Colour::WHITE), Vec4::ONE);
        assert_eq!(to_linear(Colour::TRANSPARENT), Vec4::ZERO);
    }

    #[test]
    fn linear_values_outside_the_range_are_clamped() {
        assert_eq!(from_linear(Vec4::new(2.0, -1.0, 0.0, 3.0)), Colour::rgba(255, 0, 0, 255));
    }
}
