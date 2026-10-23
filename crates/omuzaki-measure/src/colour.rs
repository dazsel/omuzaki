//! sRGB to CIELAB, D65 white: the colour space the measures compare in (design 4.1).
//!
//! CIELAB is close to how people see lightness and colour difference, so a skin tone or a line's darkness reads the
//! same across the two images that are compared. Every step is `f64`, and the powers come from `libm`, which gives the
//! same bits on every platform (ADR 0009); the result is what a measurement is built on, so it must not drift.

use std::sync::LazyLock;

/// The D65 reference white in XYZ, the same white the sRGB standard uses.
const WHITE: [f64; 3] = [0.950_47, 1.0, 1.088_83];

/// Below this ratio the cube root gives way to a straight line, which keeps the curve's slope at zero finite (CIE 15).
const EPSILON: f64 = 216.0 / 24_389.0;

/// The slope of the straight part of the CIE curve, `24389 / 27`.
const KAPPA: f64 = 24_389.0 / 27.0;

/// The sRGB transfer function, inverted, for each byte value: the linear light of a channel that reads `v`. A table,
/// because a measurement converts every pixel of a region and the powers are the expensive part.
static LINEAR: LazyLock<[f64; 256]> = LazyLock::new(|| {
    let mut table = [0.0; 256];
    for (slot, value) in table.iter_mut().zip(0_u8..=u8::MAX) {
        let encoded = f64::from(value) / 255.0;
        *slot = if encoded <= 0.040_45 { encoded / 12.92 } else { libm::pow((encoded + 0.055) / 1.055, 2.4) };
    }
    table
});

/// CIELAB (`L*`, `a*`, `b*`) of an sRGB colour given as three bytes, D65 white.
///
/// `L*` runs from 0 (black) to 100 (white); `a*` and `b*` are about −128 to 128 and are 0 for a grey.
///
/// ```
/// let [l, a, b] = omuzaki_measure::colour::lab([255, 255, 255]);
/// assert!((l - 100.0).abs() < 0.01 && a.abs() < 0.01 && b.abs() < 0.01);
/// ```
#[must_use]
pub fn lab(rgb: [u8; 3]) -> [f64; 3] {
    let linear = LINEAR.as_ref();
    let [red, green, blue] = rgb.map(|channel| linear[usize::from(channel)]);
    // The sRGB primaries to XYZ (D65), the standard matrix.
    let x = 0.412_456_4 * red + 0.357_576_1 * green + 0.180_437_5 * blue;
    let y = 0.212_672_9 * red + 0.715_152_2 * green + 0.072_175_0 * blue;
    let z = 0.019_333_9 * red + 0.119_192_0 * green + 0.950_304_1 * blue;
    let fx = lab_curve(x / WHITE[0]);
    let fy = lab_curve(y / WHITE[1]);
    let fz = lab_curve(z / WHITE[2]);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Parses `#rrggbb` (the `#` optional) to its three bytes.
#[must_use]
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// `#rrggbb` of three bytes.
#[must_use]
pub fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The CIE 1976 colour difference (`ΔE*ab`): the straight distance between two CIELAB colours.
#[must_use]
pub fn delta_e(p: [f64; 3], q: [f64; 3]) -> f64 {
    let [dl, da, db] = [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
    (dl * dl + da * da + db * db).sqrt()
}

/// The CIE 1976 companding curve of one XYZ ratio to its white.
fn lab_curve(ratio: f64) -> f64 {
    if ratio > EPSILON { libm::cbrt(ratio) } else { (KAPPA * ratio + 16.0) / 116.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Within 0.01 of the reference values (CIE 1976, D65): the tolerance the design asks for, and well inside what a
    /// measurement can tell apart.
    fn assert_lab(rgb: [u8; 3], expected: [f64; 3]) {
        let got = lab(rgb);
        for (axis, (g, e)) in got.iter().zip(expected).enumerate() {
            assert!((g - e).abs() < 0.01, "{rgb:?} axis {axis}: got {g}, expected {e}");
        }
    }

    #[test]
    fn white_is_the_lightest_neutral() {
        assert_lab([255, 255, 255], [100.0, 0.0, 0.0]);
    }

    #[test]
    fn black_is_zero() {
        assert_lab([0, 0, 0], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn pure_red_matches_the_reference() {
        assert_lab([255, 0, 0], [53.24, 80.09, 67.20]);
    }

    /// Published sRGB to CIELAB (D65, 2 degree observer) test vectors for the primaries and secondaries, as in the
    /// scikit-image colour-conversion tests (`rgb2lab`, `test_colorconv.py`) and Bruce Lindbloom's calculator (sRGB,
    /// D65, no adaptation), to the two decimals they print. The 0.01 tolerance is theirs.
    #[test]
    fn the_primaries_and_secondaries_match_the_published_vectors() {
        let published: [([u8; 3], [f64; 3]); 6] = [
            ([255, 0, 0], [53.24, 80.09, 67.20]),
            ([0, 255, 0], [87.73, -86.18, 83.18]),
            ([0, 0, 255], [32.30, 79.19, -107.86]),
            ([255, 255, 0], [97.14, -21.55, 94.48]),
            ([0, 255, 255], [91.11, -48.08, -14.13]),
            ([255, 0, 255], [60.32, 98.23, -60.82]),
        ];
        for (rgb, expected) in published {
            assert_lab(rgb, expected);
        }
    }

    #[test]
    fn greys_have_no_chroma() {
        for v in [1, 17, 128, 200, 254] {
            let [_, a, b] = lab([v, v, v]);
            // The matrix's rows sum to the white point only to about 1e-5, so a grey's chroma is that small, not zero.
            assert!(a.abs() < 1e-3 && b.abs() < 1e-3, "grey {v}: a {a}, b {b}");
        }
    }

    #[test]
    fn lightness_rises_with_the_byte() {
        let mut last = -1.0;
        for v in 0..=255_u8 {
            let [l, _, _] = lab([v, v, v]);
            assert!(l > last, "L* must rise: {v} gave {l} after {last}");
            last = l;
        }
    }

    #[test]
    fn hex_colours_parse_and_print() {
        assert_eq!(parse_hex("#f9d3c5"), Some([249, 211, 197]));
        assert_eq!(parse_hex("f9d3c5").map(hex), Some("#f9d3c5".to_owned()));
        assert_eq!(parse_hex("#f9d3c"), None);
        assert_eq!(parse_hex("#gg0000"), None);
        assert!((delta_e(lab([0, 0, 0]), lab([255, 255, 255])) - 100.0).abs() < 0.01);
    }

    #[test]
    fn the_linear_segment_joins_the_power_curve() {
        // Just either side of the sRGB knee (byte 10 is below it, byte 11 above) the lightness must not jump.
        let [below, _, _] = lab([10, 10, 10]);
        let [above, _, _] = lab([11, 11, 11]);
        assert!((above - below).abs() < 0.5, "{below} vs {above}");
    }
}
