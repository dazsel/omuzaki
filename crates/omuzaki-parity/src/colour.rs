//! sRGB bytes to CIELAB (D65) and back, and `#rrggbb`: the colour space every parity measure compares in.

use std::sync::LazyLock;

/// The D65 white in XYZ.
const WHITE: [f64; 3] = [0.950_47, 1.0, 1.088_83];
/// Below this ratio the CIE curve is a straight line.
const EPSILON: f64 = 216.0 / 24_389.0;
/// The straight part's slope.
const KAPPA: f64 = 24_389.0 / 27.0;

/// Each byte's linear light (the sRGB transfer function, inverted).
static LINEAR: LazyLock<[f64; 256]> = LazyLock::new(|| {
    let mut table = [0.0; 256];
    for (slot, value) in table.iter_mut().zip(0_u8..=u8::MAX) {
        let encoded = f64::from(value) / 255.0;
        *slot = if encoded <= 0.040_45 { encoded / 12.92 } else { ((encoded + 0.055) / 1.055).powf(2.4) };
    }
    table
});

/// CIELAB of an sRGB colour.
#[must_use]
pub fn lab(rgb: [u8; 3]) -> [f64; 3] {
    let linear = LINEAR.as_ref();
    let [r, g, b] = rgb.map(|c| linear[usize::from(c)]);
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    let curve = |t: f64| if t > EPSILON { t.cbrt() } else { (KAPPA * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (curve(x / WHITE[0]), curve(y / WHITE[1]), curve(z / WHITE[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Parses `#rrggbb` (the `#` optional).
#[must_use]
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// `#rrggbb`.
#[must_use]
pub fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_black_and_red_match_the_reference_values() {
        let w = lab([255, 255, 255]);
        assert!((w[0] - 100.0).abs() < 0.01 && w[1].abs() < 0.01 && w[2].abs() < 0.01, "{w:?}");
        assert!(lab([0, 0, 0])[0].abs() < 1e-9);
        let r = lab([255, 0, 0]);
        assert!((r[0] - 53.24).abs() < 0.01 && (r[1] - 80.09).abs() < 0.02 && (r[2] - 67.20).abs() < 0.02, "{r:?}");
        assert_eq!(parse_hex("#0a1B2c"), Some([10, 27, 44]));
        assert_eq!(hex([10, 27, 44]), "#0a1b2c");
    }
}
