//! The key composite: the program frame over a solid key colour, as the window shows it and as an OS capture of it
//! must read back.
//!
//! The program frame is premultiplied, so the composite is `colour + key × (1 − alpha)` per channel, in the 8-bit
//! encoded values (egui blends premultiplied textures the same way, in the same space, so the window's pixels and
//! this reference agree; `tests/output_window.rs` renders both and compares them).

use egui::Color32;

/// One premultiplied pixel `[r, g, b, a]` over the opaque `key`, rounded to the nearest level.
#[allow(clippy::cast_possible_truncation)] // the sum is clamped to 255 first
pub fn over_key(pixel: [u8; 4], key: Color32) -> [u8; 3] {
    let cover = 255 - u32::from(pixel[3]);
    let channel = |colour: u8, key: u8| {
        let value = u32::from(colour) + (u32::from(key) * cover + 127) / 255;
        value.min(255) as u8
    };
    [channel(pixel[0], key.r()), channel(pixel[1], key.g()), channel(pixel[2], key.b())]
}

/// A whole premultiplied RGBA8 image over `key`, as opaque RGB8 rows.
pub fn image_over_key(rgba: &[u8], key: Color32) -> Vec<u8> {
    rgba.as_chunks::<4>().0.iter().flat_map(|pixel| over_key(*pixel, key)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact composite in floating point.
    fn reference(pixel: [u8; 4], key: Color32) -> [f64; 3] {
        let a = f64::from(pixel[3]) / 255.0;
        let k = [key.r(), key.g(), key.b()];
        std::array::from_fn(|i| (f64::from(pixel[i]) + f64::from(k[i]) * (1.0 - a)).min(255.0))
    }

    #[test]
    fn every_alpha_level_is_within_half_a_level_of_the_exact_composite() {
        let key = Color32::from_rgb(0, 177, 64);
        for alpha in 0..=255_u8 {
            // A premultiplied colour never exceeds its alpha.
            for colour in [0, alpha / 3, alpha / 2, alpha] {
                let pixel = [colour, alpha / 2, alpha, alpha];
                let got = over_key(pixel, key);
                for (g, want) in got.iter().zip(reference(pixel, key)) {
                    assert!((f64::from(*g) - want).abs() <= 0.5, "{pixel:?}: {got:?} against {want}");
                }
            }
        }
    }

    #[test]
    fn opaque_shows_the_frame_and_clear_shows_the_key() {
        let key = Color32::from_rgb(10, 200, 30);
        assert_eq!(over_key([1, 2, 3, 255], key), [1, 2, 3]);
        assert_eq!(over_key([0, 0, 0, 0], key), [10, 200, 30]);
        assert_eq!(image_over_key(&[1, 2, 3, 255, 0, 0, 0, 0], key), vec![1, 2, 3, 10, 200, 30]);
    }
}
