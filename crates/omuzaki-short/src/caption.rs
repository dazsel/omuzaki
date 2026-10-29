//! Burned-in captions: a phrase in heavy white letters with a dark outline and a soft drop shadow, centred at the
//! caption line, popping in slightly larger and settling over [`POP_S`], as short-form clips caption speech.
//!
//! The face is Bricolage Grotesque (SIL OFL 1.1, `assets/fonts/`), a variable font drawn at weight [`WEIGHT`]. Glyphs
//! are laid out on one line with the font's advances and kerning (no shaping: the captions are the script's own words)
//! and shrunk to fit [`MAX_WIDTH_SHARE`] of the frame.

use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont, VariableFont, point};

use crate::def::CaptionStyle;
use crate::error::ShortError;
use crate::timeline::CaptionSpan;

static FACE: &[u8] = include_bytes!("../assets/fonts/BricolageGrotesque.ttf");

/// The weight the variable face is drawn at.
pub const WEIGHT: f32 = 800.0;
/// The widest a caption may be, as a share of the frame width.
pub const MAX_WIDTH_SHARE: f32 = 0.9;
/// How long a caption takes to settle from its pop-in size, seconds.
pub const POP_S: f32 = 0.09;
/// How much larger a caption starts.
const POP_SCALE: f32 = 1.12;
/// The outline's width, as a share of the letter height.
const OUTLINE_SHARE: f32 = 0.13;
/// The shadow's drop, as a share of the letter height.
const SHADOW_SHARE: f32 = 0.06;
const FILL: [f32; 3] = [1.0, 1.0, 1.0];
const LINE: [f32; 3] = [0.06, 0.05, 0.07];

/// Draws captions onto frames.
pub struct Captioner {
    font: FontRef<'static>,
}

impl std::fmt::Debug for Captioner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Captioner")
    }
}

/// A caption's letters as coverage, with where it goes.
struct Mask {
    width: usize,
    height: usize,
    /// Fill coverage, `0..1`.
    fill: Vec<f32>,
    /// Outline coverage (the fill grown by the outline width), `0..1`.
    line: Vec<f32>,
    /// Shadow drop, pixels.
    drop: usize,
}

impl Captioner {
    /// A captioner with the bundled face.
    ///
    /// # Errors
    /// [`ShortError::Render`] if the bundled face doesn't load (a build fault).
    pub fn new() -> Result<Self, ShortError> {
        let mut font = FontRef::try_from_slice(FACE).map_err(|e| ShortError::Render(format!("the caption face: {e}")))?;
        font.set_variation(b"wght", WEIGHT);
        Ok(Self { font })
    }

    /// The letter height of `style` on a frame `width` pixels wide, at caption size `size` (share of the width).
    fn height(style: CaptionStyle, width: u32, size: f32) -> f32 {
        #[allow(clippy::cast_precision_loss, reason = "frame sides are at most 4096")]
        let base = width as f32 * size;
        base * match style {
            CaptionStyle::Shout => 1.3,
            CaptionStyle::Aside => 0.72,
            CaptionStyle::Big => 2.4,
            CaptionStyle::Normal | CaptionStyle::None => 1.0,
        }
    }

    fn layout(&self, text: &str, height: f32) -> (Vec<Glyph>, f32) {
        let scale = PxScale::from(height);
        let scaled = self.font.as_scaled(scale);
        let mut x = 0.0;
        let mut last = None;
        let mut glyphs = Vec::new();
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if let Some(prev) = last {
                x += scaled.kern(prev, id);
            }
            glyphs.push(id.with_scale_and_position(scale, point(x, scaled.ascent())));
            x += scaled.h_advance(id);
            last = Some(id);
        }
        (glyphs, x)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    fn mask(&self, text: &str, height: f32, max_width: f32) -> Mask {
        let (_, natural) = self.layout(text, height);
        let height = if natural > max_width { height * max_width / natural } else { height };
        let (glyphs, advance) = self.layout(text, height);
        let scaled = self.font.as_scaled(PxScale::from(height));
        let line = (height * OUTLINE_SHARE).max(1.0);
        let pad = line.ceil() as usize + 2;
        let drop = (height * SHADOW_SHARE).round() as usize;
        let text_h = (scaled.ascent() - scaled.descent()).ceil() as usize;
        let width = advance.ceil() as usize + 2 * pad;
        let rows = text_h + 2 * pad + drop;
        let mut fill = vec![0.0f32; width * rows];
        for glyph in glyphs {
            if let Some(outlined) = self.font.outline_glyph(glyph) {
                let b = outlined.px_bounds();
                outlined.draw(|gx, gy, c| {
                    let x = b.min.x as i64 + i64::from(gx) + pad as i64;
                    let y = b.min.y as i64 + i64::from(gy) + pad as i64;
                    if x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < rows {
                        let i = y as usize * width + x as usize;
                        fill[i] = (fill[i] + c).min(1.0);
                    }
                });
            }
        }
        Mask { width, height: rows, line: grow(&fill, width, rows, line), fill, drop }
    }

    /// Draws `caption` onto `rgba` (straight alpha, `width` x `height`) at `t` seconds, its middle at `y` (share of the
    /// height), its letters `size` of the width tall.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    pub fn draw(&self, rgba: &mut [u8], width: u32, height: u32, caption: &CaptionSpan, t: f32, y: f32, size: f32) {
        let pop = ((t - caption.start) / POP_S).clamp(0.0, 1.0);
        let scale = POP_SCALE + (1.0 - POP_SCALE) * (1.0 - (1.0 - pop) * (1.0 - pop));
        let letter = Self::height(caption.style, width, size) * scale;
        let mask = self.mask(&caption.text, letter, width as f32 * MAX_WIDTH_SHARE);
        let left = (width as i64 - mask.width as i64) / 2;
        let top = (height as f32 * y) as i64 - (mask.height as i64) / 2;
        let at = |mx: usize, my: usize| -> Option<usize> {
            let x = left + mx as i64;
            let y = top + my as i64;
            (x >= 0 && y >= 0 && x < i64::from(width) && y < i64::from(height)).then(|| (y as usize * width as usize + x as usize) * 4)
        };
        let blend = |rgba: &mut [u8], i: usize, colour: [f32; 3], a: f32| {
            for (k, c) in colour.iter().enumerate() {
                let under = f32::from(rgba[i + k]) / 255.0;
                rgba[i + k] = ((under + (c - under) * a) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            let under = f32::from(rgba[i + 3]) / 255.0;
            rgba[i + 3] = ((under + (1.0 - under) * a) * 255.0).round().clamp(0.0, 255.0) as u8;
        };
        for my in 0..mask.height {
            for mx in 0..mask.width {
                let Some(i) = at(mx, my) else { continue };
                let shadow = my.checked_sub(mask.drop).map_or(0.0, |sy| mask.line[sy * mask.width + mx]);
                if shadow > 0.0 {
                    blend(rgba, i, [0.0, 0.0, 0.0], shadow * 0.45);
                }
                let k = my * mask.width + mx;
                if mask.line[k] > 0.0 {
                    blend(rgba, i, LINE, mask.line[k]);
                }
                if mask.fill[k] > 0.0 {
                    blend(rgba, i, FILL, mask.fill[k]);
                }
            }
        }
    }
}

/// `cover` grown by a disc of `radius` pixels (a max filter), with an anti-aliased edge.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss, clippy::cast_possible_wrap)]
fn grow(cover: &[f32], width: usize, height: usize, radius: f32) -> Vec<f32> {
    let r = radius.ceil() as i64;
    let mut offsets = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            let w = (radius + 0.5 - d).clamp(0.0, 1.0);
            if w > 0.0 {
                offsets.push((dx, dy, w));
            }
        }
    }
    let mut out = vec![0.0f32; cover.len()];
    for y in 0..height as i64 {
        for x in 0..width as i64 {
            let c = cover[y as usize * width + x as usize];
            if c <= 0.0 {
                continue;
            }
            for &(dx, dy, w) in &offsets {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= width as i64 || ny >= height as i64 {
                    continue;
                }
                let o = &mut out[ny as usize * width + nx as usize];
                *o = o.max(c * w);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caption_draws_white_letters_with_a_dark_line_inside_the_frame() {
        let captioner = Captioner::new().unwrap();
        let (w, h) = (360u32, 640u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for px in rgba.chunks_mut(4) {
            px.copy_from_slice(&[90, 120, 200, 255]);
        }
        let caption = CaptionSpan { start: 0.0, end: 1.0, text: "Hello there!".into(), style: CaptionStyle::Normal };
        captioner.draw(&mut rgba, w, h, &caption, 0.5, 0.5, 0.06);
        let white = rgba.chunks(4).filter(|p| p[0] > 245 && p[1] > 245 && p[2] > 245).count();
        let dark = rgba.chunks(4).filter(|p| p[0] < 30 && p[1] < 30 && p[2] < 30).count();
        assert!(white > 200, "letters: {white}");
        assert!(dark > 200, "outline: {dark}");
        // Nothing far from the caption line changed.
        let row = |y: u32| &rgba[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
        assert!(row(10).chunks(4).all(|p| p == [90, 120, 200, 255]));
    }

    #[test]
    fn a_long_caption_shrinks_to_fit_the_frame() {
        let captioner = Captioner::new().unwrap();
        let mask = captioner.mask("A VERY LONG SHOUTED CAPTION THAT GOES ON", 80.0, 300.0);
        assert!(mask.width <= 300 + 40, "{}", mask.width);
    }
}
