//! IEEE half floats, for the specular cube (`Rgba16Float`: filterable on every adapter, unlike `Rgba32Float`).

/// The largest finite half.
const MAX: f32 = 65504.0;

/// The half-precision bits of a non-negative finite `v`, rounded to nearest even and saturated at the largest finite
/// half (a sun brighter than that is still the brightest thing in the sky, not infinity: infinity would poison every
/// filtered texel beside it).
pub(super) fn half_bits(v: f32) -> u16 {
    let v = v.clamp(0.0, MAX);
    if v < 6.103_515_6e-5 {
        // Subnormal halves are multiples of 2^-24; 1024 of them is exactly the smallest normal number's bits.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=1024 by the branch
        return (v * 16_777_216.0).round_ties_even() as u16;
    }
    let bits = v.to_bits();
    // Rebias the exponent from 127 to 15 and keep the top ten mantissa bits; v >= 2^-14 and <= 65504 keep it in 1..=30.
    #[allow(clippy::cast_possible_truncation)] // fits 15 bits by the range above
    let mut half = ((((bits >> 23) - 112) << 10) | ((bits & 0x007f_ffff) >> 13)) as u16;
    let rest = bits & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && half & 1 == 1) {
        // A carry out of the mantissa bumps the exponent, which is the right answer.
        half += 1;
    }
    half
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value of half bits `h` (positive halves only).
    fn value(h: u16) -> f32 {
        let (exp, man) = (i32::from(h >> 10), f32::from(h & 0x3ff));
        if exp == 0 { man * 2.0_f32.powi(-24) } else { (1.0 + man / 1024.0) * 2.0_f32.powi(exp - 15) }
    }

    #[test]
    fn known_values_encode_exactly() {
        assert_eq!(half_bits(0.0), 0x0000);
        assert_eq!(half_bits(1.0), 0x3c00);
        assert_eq!(half_bits(0.5), 0x3800);
        assert_eq!(half_bits(2.0), 0x4000);
        assert_eq!(half_bits(65504.0), 0x7bff);
        assert_eq!(half_bits(2.0_f32.powi(-24)), 0x0001, "the smallest subnormal");
        assert_eq!(half_bits(2.0_f32.powi(-14)), 0x0400, "the smallest normal");
    }

    #[test]
    fn bright_values_saturate_and_ties_round_to_even() {
        assert_eq!(half_bits(1.0e9), 0x7bff);
        assert_eq!(half_bits(f32::MAX), 0x7bff);
        assert_eq!(half_bits(-3.0), 0, "negative radiance is not a thing; it clamps to black");
        // Halfway between 1.0 (0x3c00) and the next half: ties go to the even mantissa, so 1.0.
        assert_eq!(half_bits(1.0 + 2.0_f32.powi(-11)), 0x3c00);
        assert_eq!(half_bits(1.0 + 3.0 * 2.0_f32.powi(-11)), 0x3c02);
    }

    #[test]
    fn the_round_trip_error_is_at_most_half_a_unit_in_the_last_place() {
        let mut v = 1.0e-6_f32;
        while v < 60_000.0 {
            let back = value(half_bits(v));
            // A half has eleven significant bits, so the relative error is below 2^-11 in the normal range.
            assert!(
                (back - v).abs() <= v.max(2.0_f32.powi(-14)) * 2.0_f32.powi(-11) + 2.0_f32.powi(-25),
                "{v} -> {back}"
            );
            v *= 1.07;
        }
    }
}
