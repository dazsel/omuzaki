//! What a receiver's blend mode does to a premultiplied frame, measured against ground truth.
//!
//! Spout carries no alpha-mode flag: the receiver is told how to read the pixels by its user. OBS's Spout2 source offers
//! several "composite modes", so a frame that is right for one looks wrong in another. This module reproduces those
//! blends on the CPU over a known background and compares each with a reference the GPU rendered directly over that
//! background, giving numbers for "premultiplied, read as straight" (dark halo) and "straight, read as premultiplied"
//! (bright fringe).

use serde::{Deserialize, Serialize};

/// How the producer encoded the pixels it sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sent {
    /// Colour already multiplied by alpha (what the renderer produces; `AlphaEncoding::Premultiplied`).
    Premultiplied,
    /// Colour divided back by alpha (`AlphaEncoding::Straight`, what PNG and NDI BGRA carry).
    Straight,
}

/// How the receiver blends the pixels over what is behind them. Names follow OBS's Spout2 source composite modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blend {
    /// "Default": source-alpha blend, so the colour is read as straight.
    Straight,
    /// "Premultiplied Alpha": `ONE, INV_SRC_ALPHA`, so the colour is read as already multiplied.
    Premultiplied,
    /// "Alpha": divide by alpha first, then source-alpha blend (OBS's premultiplied-alpha effect).
    UnpremultiplyThenStraight,
}

/// Error of one (sent, blend) pairing against the ground truth, in 8-bit levels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Case {
    /// Encoding of the frame the receiver got.
    pub sent: Sent,
    /// Blend the receiver used.
    pub blend: Blend,
    /// Largest per-channel difference from the ground truth.
    pub max_error: f64,
    /// Mean per-channel difference over the compared pixels.
    pub mean_error: f64,
    /// Pixels where some channel is off by more than 8 levels (visible as a fringe).
    pub visible_pixels: usize,
}

/// Rounds a 0..255 float to a byte, saturating.
#[must_use]
pub fn quantise(value: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 first
    let byte = value.round().clamp(0.0, 255.0) as u8;
    byte
}

/// Converts a premultiplied BGRA pixel to straight alpha in 8 bits, as a producer that sends straight alpha would.
#[must_use]
pub fn unpremultiply(px: [u8; 4]) -> [u8; 4] {
    let a = f32::from(px[3]);
    if px[3] == 0 {
        return [0, 0, 0, 0];
    }
    let up = |c: u8| quantise(f32::from(c) * 255.0 / a);
    [up(px[0]), up(px[1]), up(px[2]), px[3]]
}

/// Blends one BGRA source pixel over an opaque background colour (RGB, 0..255) in `blend` mode; returns RGB.
#[must_use]
pub fn blend_over(px: [u8; 4], bg_rgb: [f32; 3], blend: Blend) -> [f32; 3] {
    let a = f32::from(px[3]) / 255.0;
    let src = [f32::from(px[2]), f32::from(px[1]), f32::from(px[0])];
    let straight = match blend {
        Blend::Premultiplied => [src[0], src[1], src[2]],
        Blend::Straight => [src[0] * a, src[1] * a, src[2] * a],
        Blend::UnpremultiplyThenStraight => {
            // Divide then multiply by the same alpha: a no-op in float, which is the point of the mode. Kept explicit so
            // the three modes read alike.
            let unit = |c: f32| if a > 0.0 { (c / a).min(255.0) * a } else { 0.0 };
            [unit(src[0]), unit(src[1]), unit(src[2])]
        }
    };
    std::array::from_fn(|i| (straight[i] + bg_rgb[i] * (1.0 - a)).clamp(0.0, 255.0))
}

/// Compares every (sent, blend) pairing over the rows `rows` of two same-size BGRA frames: `premultiplied` is what the
/// renderer produced, `truth` the same picture rendered over `bg_rgb` (opaque).
#[must_use]
pub fn composite_cases(
    premultiplied: &[u8],
    truth: &[u8],
    width: u32,
    rows: std::ops::Range<u32>,
    bg_rgb: [f32; 3],
) -> Vec<Case> {
    let pairs = [
        (Sent::Premultiplied, Blend::Premultiplied),
        (Sent::Premultiplied, Blend::Straight),
        (Sent::Premultiplied, Blend::UnpremultiplyThenStraight),
        (Sent::Straight, Blend::Straight),
        (Sent::Straight, Blend::Premultiplied),
    ];
    pairs
        .into_iter()
        .map(|(sent, blend)| {
            let (mut max_error, mut total, mut visible, mut count) = (0.0f64, 0.0f64, 0usize, 0usize);
            for y in rows.clone() {
                let start = y as usize * width as usize * 4;
                let row = start..start + width as usize * 4;
                let (sources, references) =
                    (premultiplied[row.clone()].as_chunks::<4>().0, truth[row].as_chunks::<4>().0);
                for (&src, reference) in sources.iter().zip(references) {
                    let sent_px = if sent == Sent::Straight { unpremultiply(src) } else { src };
                    let got = blend_over(sent_px, bg_rgb, blend);
                    let want = [f32::from(reference[2]), f32::from(reference[1]), f32::from(reference[0])];
                    let worst = got.iter().zip(want).map(|(g, w)| f64::from((g - w).abs())).fold(0.0, f64::max);
                    max_error = max_error.max(worst);
                    total += got.iter().zip(want).map(|(g, w)| f64::from((g - w).abs())).sum::<f64>();
                    visible += usize::from(worst > 8.0);
                    count += 3;
                }
            }
            #[allow(clippy::cast_precision_loss)]
            let mean_error = if count == 0 { 0.0 } else { total / count as f64 };
            Case { sent, blend, max_error, mean_error, visible_pixels: visible }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpremultiply_inverts_premultiply_for_opaque_and_clear() {
        assert_eq!(unpremultiply([10, 20, 30, 255]), [10, 20, 30, 255]);
        assert_eq!(unpremultiply([0, 0, 0, 0]), [0, 0, 0, 0]);
        // Half-covered white: stored 128 (premultiplied) reads back as full white at alpha 128.
        assert_eq!(unpremultiply([128, 128, 128, 128]), [255, 255, 255, 128]);
    }

    #[test]
    fn half_white_over_black_in_each_mode() {
        let premult_px = [128, 128, 128, 128];
        let bg = [0.0; 3];
        let right = blend_over(premult_px, bg, Blend::Premultiplied);
        assert!((right[0] - 128.0).abs() < 1e-3);
        // Read as straight, the half-covered white comes out about a quarter bright: the dark halo.
        let dark = blend_over(premult_px, bg, Blend::Straight);
        assert!((dark[0] - 64.25).abs() < 0.1);
        let converted = blend_over(premult_px, bg, Blend::UnpremultiplyThenStraight);
        assert!((converted[0] - 128.0).abs() < 1e-3);
        // The same pixel sent straight (white, alpha 128) but read as premultiplied: the bright fringe.
        let bright = blend_over([255, 255, 255, 128], bg, Blend::Premultiplied);
        assert!((bright[0] - 255.0).abs() < 1e-3);
    }

    #[test]
    fn matching_pairs_are_close_and_mismatches_are_not() {
        // One pixel: 50% white over a mid grey background; truth is the correct blend, quantised.
        let premult = vec![128u8, 128, 128, 128];
        let truth = vec![191u8, 191, 191, 255]; // 128 + 128 * (1 - 128/255) = 191.75 -> 192; allow the 1-level slack
        let cases = composite_cases(&premult, &truth, 1, 0..1, [128.0; 3]);
        let find = |s, b| cases.iter().find(|c| c.sent == s && c.blend == b).copied().unwrap();
        assert!(find(Sent::Premultiplied, Blend::Premultiplied).max_error < 2.0);
        assert!(find(Sent::Premultiplied, Blend::UnpremultiplyThenStraight).max_error < 2.0);
        assert!(find(Sent::Premultiplied, Blend::Straight).max_error > 30.0);
        assert!(find(Sent::Straight, Blend::Straight).max_error < 2.0);
        assert!(find(Sent::Straight, Blend::Premultiplied).max_error > 30.0);
        assert_eq!(find(Sent::Premultiplied, Blend::Straight).visible_pixels, 1);
    }
}
