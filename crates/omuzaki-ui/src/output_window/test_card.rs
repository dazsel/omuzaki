//! The test card: the picture the output window shows until the viewport hands it the program frame, and the one
//! its checks compare against.
//!
//! It carries what the Spout spike's test picture carries (`spikes/broadcast-out/spout-out/src/scene.wgsl`): every
//! alpha level, soft edges and known-value patches, all premultiplied. Here it is drawn on the CPU, so a test can
//! build it without a GPU.

use egui::{Color32, ColorImage};

/// The known patches: straight colour and alpha, as the Spout spike reads them back.
pub const PATCHES: [([u8; 3], f32); 5] =
    [([255, 128, 0], 0.5), ([0, 128, 255], 0.25), ([255, 255, 255], 0.1), ([255, 0, 0], 1.0), ([0, 0, 0], 0.0)];

/// The feathered disc's straight colour.
const DISC: [u8; 3] = [255, 115, 26];

/// The test card at `width` × `height` pixels, premultiplied: an alpha ramp along the top tenth (every level from 0
/// to 255 across the width), the known patches in a row below it, and a feathered orange disc in the middle.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pixel maths
pub fn test_card(width: usize, height: usize) -> ColorImage {
    let (w, h) = (width as f32, height as f32);
    let ramp_bottom = h * 0.1;
    let (patch_top, patch_bottom) = (h * 0.15, h * 0.3);
    let patch_width = w / PATCHES.len() as f32;
    let (cx, cy, radius, feather) = (w * 0.5, h * 0.65, h * 0.25, h * 0.08);
    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x as f32 + 0.5, y as f32 + 0.5)))
        .map(|(x, y)| {
            if y < ramp_bottom {
                let alpha = (x / w * 255.0).floor() / 255.0;
                return premultiplied([255, 255, 255], alpha);
            }
            if (patch_top..patch_bottom).contains(&y) {
                let index = ((x / patch_width) as usize).min(PATCHES.len() - 1);
                let (colour, alpha) = PATCHES[index];
                return premultiplied(colour, alpha);
            }
            let distance = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            let alpha = ((radius + feather - distance) / feather).clamp(0.0, 1.0);
            premultiplied(DISC, alpha)
        })
        .collect();
    ColorImage::new([width, height], pixels)
}

/// `colour` at `alpha`, premultiplied and rounded.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // values are clamped to 0..=255
fn premultiplied(colour: [u8; 3], alpha: f32) -> Color32 {
    let scale = |c: u8| (f32::from(c) * alpha).round().clamp(0.0, 255.0) as u8;
    let a = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_premultiplied(scale(colour[0]), scale(colour[1]), scale(colour[2]), a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_has_every_alpha_level_soft_edges_and_its_patches() {
        let card = test_card(512, 256);
        assert_eq!(card.size, [512, 256]);
        let mut levels = [false; 256];
        for pixel in &card.pixels {
            levels[usize::from(pixel.a())] = true;
            assert!(pixel.r() <= pixel.a() && pixel.g() <= pixel.a() && pixel.b() <= pixel.a(), "premultiplied");
        }
        assert!(levels.iter().all(|seen| *seen), "every alpha level appears");
        // The middle of each patch is its known value.
        for (i, (colour, alpha)) in PATCHES.iter().enumerate() {
            let x = i * 512 / PATCHES.len() + 512 / PATCHES.len() / 2;
            assert_eq!(card.pixels[56 * 512 + x], premultiplied(*colour, *alpha), "patch {i}");
        }
    }
}
