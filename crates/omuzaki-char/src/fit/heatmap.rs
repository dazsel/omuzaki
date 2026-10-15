//! The difference heatmap: where the fitted body and the reference silhouette still disagree.
//!
//! Red marks pixels the model covers and the target does not, blue the pixels the target covers and the model does
//! not, and agreement is transparent. A legend strip under the picture holds the two swatches and the view's `IoU`,
//! printed in a built-in 3 by 5 pixel font, so the number travels with the image. The result depends only on the two
//! masks.

use super::{FitError, Silhouette};

/// A straight-alpha RGBA picture, row-major from the top-left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 4]>,
}

impl RgbaImage {
    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The pixels, row-major from the top-left.
    pub fn pixels(&self) -> &[[u8; 4]] {
        &self.pixels
    }

    /// The pixel at column `x`, row `y`, if inside.
    pub fn get(&self, x: usize, y: usize) -> Option<[u8; 4]> {
        (x < self.width && y < self.height).then(|| self.pixels[y * self.width + x])
    }

    /// The bytes as `RGBARGBA..`, for an encoder.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.pixels.iter().flatten().copied().collect()
    }
}

/// Where the model is outside the target.
pub const MODEL_ONLY: [u8; 4] = [220, 40, 40, 255];
/// Where the target is outside the model.
pub const TARGET_ONLY: [u8; 4] = [40, 90, 230, 255];
/// The legend strip's background.
pub const LEGEND_BACKGROUND: [u8; 4] = [245, 245, 245, 255];
/// The legend text.
const INK: [u8; 4] = [20, 20, 20, 255];
/// Rows of the legend strip.
pub const LEGEND_HEIGHT: usize = 11;
/// The 3 by 5 font: one row per byte, the low three bits left to right.
const GLYPH: [(char, [u8; 5]); 12] = [
    ('0', [0b111, 0b101, 0b101, 0b101, 0b111]),
    ('1', [0b010, 0b110, 0b010, 0b010, 0b111]),
    ('2', [0b111, 0b001, 0b111, 0b100, 0b111]),
    ('3', [0b111, 0b001, 0b111, 0b001, 0b111]),
    ('4', [0b101, 0b101, 0b111, 0b001, 0b001]),
    ('5', [0b111, 0b100, 0b111, 0b001, 0b111]),
    ('6', [0b111, 0b100, 0b111, 0b101, 0b111]),
    ('7', [0b111, 0b001, 0b001, 0b001, 0b001]),
    ('8', [0b111, 0b101, 0b111, 0b101, 0b111]),
    ('9', [0b111, 0b101, 0b111, 0b001, 0b111]),
    ('.', [0b000, 0b000, 0b000, 0b000, 0b010]),
    ('I', [0b111, 0b010, 0b010, 0b010, 0b111]),
];
/// Extra glyphs of the label `IOU`, kept apart so the digit table above stays a plain 12.
const LABEL: [(char, [u8; 5]); 3] =
    [('O', [0b111, 0b101, 0b101, 0b101, 0b111]), ('U', [0b101, 0b101, 0b101, 0b101, 0b111]), (' ', [0; 5])];

/// The `IoU` of two masks of the same size (`0` when both are empty).
fn iou(model: &Silhouette, target: &Silhouette) -> f64 {
    let (mut inter, mut union) = (0usize, 0usize);
    for (&m, &t) in model.pixels().iter().zip(target.pixels()) {
        inter += usize::from(m && t);
        union += usize::from(m || t);
    }
    if union == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^52
    {
        inter as f64 / union as f64
    }
}

/// The text of the legend for an `IoU`: `IOU 0.973`, rounded to three places and never above `1.000`.
pub fn legend_text(iou: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=1000
    let thousandths = (iou.clamp(0.0, 1.0) * 1000.0).round() as u32;
    format!("IOU {}.{:03}", thousandths / 1000, thousandths % 1000)
}

fn glyph(c: char) -> [u8; 5] {
    GLYPH.iter().chain(&LABEL).find(|(g, _)| *g == c).map_or([0; 5], |(_, rows)| *rows)
}

/// The difference of `model` and `target`: red where the model is outside the target, blue where the target is outside
/// the model, transparent where they agree, with a legend strip of [`LEGEND_HEIGHT`] rows under it holding the swatches
/// and the `IoU` of the two masks.
///
/// # Errors
/// [`FitError::BadMask`] if the masks differ in size.
pub fn heatmap(model: &Silhouette, target: &Silhouette) -> Result<RgbaImage, FitError> {
    let (w, h) = (model.width(), model.height());
    if (w, h) != (target.width(), target.height()) {
        return Err(FitError::BadMask(format!(
            "the model mask is {w} x {h} and the target {} x {}",
            target.width(),
            target.height()
        )));
    }
    let mut pixels: Vec<[u8; 4]> = model
        .pixels()
        .iter()
        .zip(target.pixels())
        .map(|(&m, &t)| match (m, t) {
            (true, false) => MODEL_ONLY,
            (false, true) => TARGET_ONLY,
            _ => [0, 0, 0, 0],
        })
        .collect();
    let mut legend = LegendCanvas { w, pixels: vec![LEGEND_BACKGROUND; w * LEGEND_HEIGHT] };
    legend.fill(1, 3, 5, MODEL_ONLY);
    legend.fill(7, 3, 5, TARGET_ONLY);
    let mut x = 14;
    for c in legend_text(iou(model, target)).chars() {
        legend.draw(x, 3, glyph(c));
        x += 4;
    }
    pixels.extend(legend.pixels);
    Ok(RgbaImage { width: w, height: h + LEGEND_HEIGHT, pixels })
}

/// The legend strip being drawn; anything past its right edge is dropped.
struct LegendCanvas {
    w: usize,
    pixels: Vec<[u8; 4]>,
}

impl LegendCanvas {
    fn put(&mut self, x: usize, y: usize, colour: [u8; 4]) {
        if x < self.w && y < LEGEND_HEIGHT {
            self.pixels[y * self.w + x] = colour;
        }
    }

    /// A `size` by `size` square with its top-left corner at `(x, y)`.
    fn fill(&mut self, x: usize, y: usize, size: usize, colour: [u8; 4]) {
        for dy in 0..size {
            for dx in 0..size {
                self.put(x + dx, y + dy, colour);
            }
        }
    }

    fn draw(&mut self, x: usize, y: usize, rows: [u8; 5]) {
        for (dy, row) in rows.iter().enumerate() {
            for dx in 0..3 {
                if row >> (2 - dx) & 1 == 1 {
                    self.put(x + dx, y + dy, INK);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mask of `w` by `h` with the columns `from..to` on.
    fn band(w: usize, h: usize, from: usize, to: usize) -> Silhouette {
        Silhouette::new(w, h, (0..w * h).map(|i| (from..to).contains(&(i % w))).collect()).unwrap()
    }

    #[test]
    fn a_wider_model_is_a_red_band_on_both_sides_and_no_blue() {
        let (model, target) = (band(40, 20, 6, 34), band(40, 20, 8, 32));
        let map = heatmap(&model, &target).unwrap();
        assert_eq!((map.width(), map.height()), (40, 20 + LEGEND_HEIGHT));
        let row: Vec<_> = (0..40).map(|x| map.get(x, 5).unwrap()).collect();
        assert!(row[6..8].iter().chain(&row[32..34]).all(|p| *p == MODEL_ONLY));
        assert!(row[8..32].iter().all(|p| p[3] == 0), "agreement is transparent");
        assert!(map.pixels()[..40 * 20].iter().all(|p| *p != TARGET_ONLY));
    }

    #[test]
    fn a_narrower_model_is_blue() {
        let map = heatmap(&band(40, 20, 8, 32), &band(40, 20, 6, 34)).unwrap();
        assert_eq!(map.get(6, 0), Some(TARGET_ONLY));
        assert!(map.pixels()[..40 * 20].iter().all(|p| *p != MODEL_ONLY));
    }

    #[test]
    fn identical_masks_are_transparent_with_a_perfect_iou_in_the_legend() {
        let m = band(40, 20, 8, 32);
        let map = heatmap(&m, &m).unwrap();
        assert!(map.pixels()[..40 * 20].iter().all(|p| p[3] == 0));
        assert_eq!(legend_text(iou(&m, &m)), "IOU 1.000");
        assert!(map.pixels()[40 * 20..].contains(&INK), "the legend text is drawn");
    }

    #[test]
    fn the_heatmap_is_deterministic_and_the_legend_text_rounds() {
        let (a, b) = (band(40, 20, 6, 34), band(40, 20, 8, 32));
        assert_eq!(heatmap(&a, &b).unwrap(), heatmap(&a, &b).unwrap());
        assert_eq!(legend_text(0.9734), "IOU 0.973");
        assert_eq!(legend_text(1.7), "IOU 1.000");
        assert_eq!(legend_text(f64::NAN.max(-1.0)), "IOU 0.000");
    }

    #[test]
    fn masks_of_different_sizes_are_refused() {
        let err = heatmap(&band(40, 20, 8, 32), &band(30, 20, 8, 20)).unwrap_err();
        assert!(matches!(err, FitError::BadMask(_)));
    }
}
