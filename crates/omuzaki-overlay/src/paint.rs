//! The CPU painter: each widget is drawn into a [`Canvas`] of its own (premultiplied RGBA8) only when what it shows
//! changes, then blended into the program frame every frame with an opacity and an offset, so an alert sliding in or a
//! chat line fading out costs a blend, not a re-raster.
//!
//! Text is Inter `SemiBold` for body and names and Bricolage Grotesque at weight 800 for titles and digits, both SIL OFL
//! 1.1 and already bundled by the stage and the shorts (`NOTICES.md` beside this crate's manifest). Latin only for now:
//! a glyph the face lacks is skipped rather than drawn as a box.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, VariableFont, point};
use omuzaki_render_api::{AlphaEncoding, Image};

static BODY: &[u8] = include_bytes!("../../omuzaki-stage/assets/fonts/Inter-SemiBold.subset.ttf");
static DISPLAY: &[u8] = include_bytes!("../../omuzaki-short/assets/fonts/BricolageGrotesque.ttf");

/// The display face's weight.
const DISPLAY_WEIGHT: f32 = 800.0;

/// A straight sRGB colour with an opacity, as the style gives it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rgba {
    /// Red, green, blue, `0..=1`, straight.
    pub rgb: [f32; 3],
    /// Opacity, `0..=1`.
    pub a: f32,
}

impl Rgba {
    /// `rgb` bytes at opacity `a`.
    pub fn bytes(rgb: [u8; 3], a: f32) -> Self {
        Self { rgb: [f32::from(rgb[0]) / 255.0, f32::from(rgb[1]) / 255.0, f32::from(rgb[2]) / 255.0], a }
    }

    /// The same colour at `a` times its opacity.
    #[must_use]
    pub fn fade(self, a: f32) -> Self {
        Self { a: self.a * a, ..self }
    }
}

/// Which face text is set in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Face {
    /// Inter `SemiBold`: names, messages, labels.
    Body,
    /// Bricolage Grotesque 800: titles and the timer's digits.
    Display,
}

/// How text is set: its face, letter height, colour, and whether it gets a dark rim to read over any picture.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pen {
    /// The face.
    pub face: Face,
    /// Letter height in pixels.
    pub px: f32,
    /// Its colour.
    pub colour: Rgba,
    /// Draw a dark rim first.
    pub outline: bool,
}

/// The two faces, loaded once.
pub struct Fonts {
    body: FontRef<'static>,
    display: FontRef<'static>,
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fonts")
    }
}

impl Fonts {
    /// The bundled faces.
    ///
    /// # Errors
    /// A message if a bundled face doesn't load (a build fault).
    pub fn new() -> Result<Self, String> {
        let body = FontRef::try_from_slice(BODY).map_err(|e| format!("the body face: {e}"))?;
        let mut display = FontRef::try_from_slice(DISPLAY).map_err(|e| format!("the display face: {e}"))?;
        display.set_variation(b"wght", DISPLAY_WEIGHT);
        Ok(Self { body, display })
    }

    fn face(&self, face: Face) -> &FontRef<'static> {
        match face {
            Face::Body => &self.body,
            Face::Display => &self.display,
        }
    }

    /// The width of `text` at letter height `px`.
    pub fn width(&self, face: Face, text: &str, px: f32) -> f32 {
        let font = self.face(face).as_scaled(PxScale::from(px));
        let mut width = 0.0;
        let mut last = None;
        for ch in text.chars() {
            let id = font.glyph_id(ch);
            if let Some(prev) = last {
                width += font.kern(prev, id);
            }
            width += font.h_advance(id);
            last = Some(id);
        }
        width
    }

    /// `text` broken into lines no wider than `max` at `px`, on spaces; a word wider than a line is cut by character.
    pub fn wrap(&self, face: Face, text: &str, px: f32, max: f32) -> Vec<String> {
        self.wrap_after(face, text, px, max, max)
    }

    /// [`wrap`](Self::wrap), with the first line `first` wide and the rest `max` (for text that follows a name).
    pub fn wrap_after(&self, face: Face, text: &str, px: f32, first: f32, max: f32) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut line = String::new();
        let room = |lines: &Vec<String>| if lines.is_empty() { first } else { max };
        for word in text.split_whitespace() {
            let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
            if self.width(face, &candidate, px) <= room(&lines) {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                if self.width(face, word, px) <= room(&lines) {
                    word.clone_into(&mut line);
                    continue;
                }
            }
            // The word alone is too wide: cut it where it overflows.
            for ch in word.chars() {
                line.push(ch);
                if self.width(face, &line, px) > room(&lines) && line.chars().count() > 1 {
                    line.pop();
                    lines.push(std::mem::take(&mut line));
                    line.push(ch);
                }
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }

    /// `text` shortened with an ellipsis to fit `max` at `px`.
    pub fn fit(&self, face: Face, text: &str, px: f32, max: f32) -> String {
        if self.width(face, text, px) <= max {
            return text.to_owned();
        }
        let mut out: String = text.to_owned();
        while !out.is_empty() && self.width(face, &format!("{out}\u{2026}"), px) > max {
            out.pop();
        }
        format!("{}\u{2026}", out.trim_end())
    }

    /// The face's ascent at `px`: how far the baseline sits below a line's top.
    pub fn ascent(&self, face: Face, px: f32) -> f32 {
        self.face(face).as_scaled(PxScale::from(px)).ascent()
    }
}

/// A widget's picture: premultiplied RGBA8, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Canvas {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, premultiplied.
    pub rgba8: Vec<u8>,
}

impl Canvas {
    /// A transparent canvas of `width` by `height` (each at least 1).
    pub fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Self { width, height, rgba8: vec![0; width as usize * height as usize * 4] }
    }

    /// Lays `colour` over the pixel at `(x, y)` with coverage `c` (`0..=1`).
    #[inline]
    #[allow(clippy::many_single_char_names, reason = "pixel maths: position, colour, coverage and alpha")]
    fn over(&mut self, x: i64, y: i64, colour: Rgba, c: f32) {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return;
        }
        let a = (colour.a * c).clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation, reason = "checked to be inside the canvas")]
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let Some(px) = self.rgba8.get_mut(i..i + 4) else { return };
        let keep = 1.0 - a;
        for (channel, value) in px.iter_mut().zip(colour.rgb) {
            *channel = to_byte(value * a + f32::from(*channel) / 255.0 * keep);
        }
        px[3] = to_byte(a + f32::from(px[3]) / 255.0 * keep);
    }

    /// Fills a rectangle with corners rounded to `r` pixels, anti-aliased.
    #[allow(clippy::cast_possible_truncation, reason = "canvas coordinates are small")]
    #[allow(clippy::many_single_char_names, reason = "a box's x, y, width, height and radius")]
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, colour: Rgba) {
        if w <= 0.0 || h <= 0.0 || colour.a <= 0.0 {
            return;
        }
        let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
        let (x0, y0, x1, y1) = (x.floor() as i64, y.floor() as i64, (x + w).ceil() as i64, (y + h).ceil() as i64);
        let (cx, cy, hx, hy) = (x + w * 0.5, y + h * 0.5, w * 0.5 - r, h * 0.5 - r);
        for py in y0..y1 {
            for px in x0..x1 {
                #[allow(clippy::cast_precision_loss, reason = "canvas coordinates are small")]
                let (sx, sy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                // The signed distance to the rounded box, and coverage from it over one pixel.
                let (qx, qy) = (sx.abs() - hx, sy.abs() - hy);
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let d = outside + qx.max(qy).min(0.0) - r;
                self.over(px, py, colour, (0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// Sets `text` with `pen`, its baseline's left end at `(x, y)`; returns where it ends.
    pub fn text(&mut self, fonts: &Fonts, pen: Pen, text: &str, x: f32, y: f32) -> f32 {
        if pen.outline {
            let rim = Pen { colour: Rgba { rgb: [0.05, 0.04, 0.07], a: pen.colour.a * 0.9 }, ..pen };
            let d = (pen.px * 0.07).max(1.0);
            for (ox, oy) in [(-d, 0.0), (d, 0.0), (0.0, -d), (0.0, d), (-d, -d), (d, d), (-d, d), (d, -d)] {
                self.glyphs(fonts, rim, text, x + ox, y + oy);
            }
        }
        self.glyphs(fonts, pen, text, x, y)
    }

    #[allow(clippy::cast_possible_truncation, reason = "canvas coordinates are small")]
    fn glyphs(&mut self, fonts: &Fonts, pen: Pen, text: &str, x: f32, y: f32) -> f32 {
        let (px, colour) = (pen.px, pen.colour);
        let font = fonts.face(pen.face);
        let scaled = font.as_scaled(PxScale::from(px));
        let mut caret = x;
        let mut last = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(prev) = last {
                caret += scaled.kern(prev, id);
            }
            let advance = scaled.h_advance(id);
            if id.0 != 0
                && let Some(outlined) = font.outline_glyph(id.with_scale_and_position(px, point(caret, y)))
            {
                let bounds = outlined.px_bounds();
                let (bx, by) = (bounds.min.x as i64, bounds.min.y as i64);
                outlined.draw(|gx, gy, c| self.over(bx + i64::from(gx), by + i64::from(gy), colour, c));
            }
            caret += advance;
            last = Some(id);
        }
        caret
    }

    /// Blends this picture into `frame` with its top left at `(x, y)` (which may be off the frame), at `opacity`.
    pub fn blend_into(&self, frame: &mut Image, x: i64, y: i64, opacity: f32) {
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return;
        }
        let (fw, fh) = (i64::from(frame.size.x), i64::from(frame.size.y));
        let straight = frame.alpha == AlphaEncoding::Straight;
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + i64::from(self.width)).min(fw), (y + i64::from(self.height)).min(fh));
        for fy in y0..y1 {
            for fx in x0..x1 {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation, reason = "inside both images")]
                let (si, di) = (
                    ((fy - y) as usize * self.width as usize + (fx - x) as usize) * 4,
                    (fy as usize * fw as usize + fx as usize) * 4,
                );
                let (Some(src), Some(dst)) = (self.rgba8.get(si..si + 4), frame.rgba8.get_mut(di..di + 4)) else {
                    continue;
                };
                let sa = f32::from(src[3]) / 255.0 * opacity;
                if sa <= 0.0 {
                    continue;
                }
                let da = f32::from(dst[3]) / 255.0;
                let out_a = sa + da * (1.0 - sa);
                for k in 0..3 {
                    let s = f32::from(src[k]) / 255.0 * opacity;
                    let d = f32::from(dst[k]) / 255.0;
                    let d_pre = if straight { d * da } else { d };
                    let pre = s + d_pre * (1.0 - sa);
                    dst[k] = to_byte(if straight { if out_a > 0.0 { pre / out_a } else { 0.0 } } else { pre });
                }
                dst[3] = to_byte(out_a);
            }
        }
    }
}

#[inline]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255 first")]
fn to_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use glam::UVec2;

    use super::*;

    #[test]
    fn the_faces_load_and_measure() {
        let fonts = Fonts::new().unwrap();
        let w = fonts.width(Face::Body, "Hello", 20.0);
        assert!(w > 30.0 && w < 80.0, "{w}");
        assert!(fonts.width(Face::Display, "00:00", 40.0) > fonts.width(Face::Display, "0", 40.0));
    }

    #[test]
    fn wrapping_keeps_every_line_inside_the_width() {
        let fonts = Fonts::new().unwrap();
        let text = "the quick brown fox jumps over the lazy dog and keeps running supercalifragilisticexpialidocious";
        let lines = fonts.wrap(Face::Body, text, 16.0, 120.0);
        assert!(lines.len() > 3, "{lines:?}");
        for line in &lines {
            assert!(fonts.width(Face::Body, line, 16.0) <= 120.0, "{line}");
        }
        assert_eq!(lines.join(" ").replace(' ', ""), text.replace(' ', ""), "no letter is lost");
    }

    #[test]
    fn fitting_adds_an_ellipsis_only_when_needed() {
        let fonts = Fonts::new().unwrap();
        assert_eq!(fonts.fit(Face::Body, "short", 16.0, 200.0), "short");
        let cut = fonts.fit(Face::Body, "a much longer name than fits", 16.0, 60.0);
        assert!(cut.ends_with('\u{2026}') && fonts.width(Face::Body, &cut, 16.0) <= 60.0, "{cut}");
    }

    #[test]
    fn a_rounded_rect_is_solid_inside_and_soft_at_the_corner() {
        let mut c = Canvas::new(40, 20);
        c.round_rect(0.0, 0.0, 40.0, 20.0, 8.0, Rgba::bytes([255, 0, 0], 1.0));
        assert_eq!(&c.rgba8[(10 * 40 + 20) * 4..][..4], &[255, 0, 0, 255]);
        assert_eq!(c.rgba8[3], 0, "the very corner is outside the rounding");
    }

    #[test]
    fn text_draws_inside_its_line() {
        let fonts = Fonts::new().unwrap();
        let mut c = Canvas::new(120, 30);
        let pen = Pen { face: Face::Body, px: 20.0, colour: Rgba::bytes([255, 255, 255], 1.0), outline: false };
        let end = c.text(&fonts, pen, "Hi chat", 4.0, 22.0);
        assert!(end > 40.0);
        let lit = c.rgba8.chunks(4).filter(|p| p[3] > 128).count();
        assert!(lit > 50, "{lit} pixels");
    }

    #[test]
    fn blending_respects_both_alpha_encodings() {
        let mut picture = Canvas::new(1, 1);
        picture.rgba8.copy_from_slice(&[128, 0, 0, 128]); // red at half alpha, premultiplied
        for (alpha, expect) in
            [(AlphaEncoding::Straight, [255, 0, 0, 128]), (AlphaEncoding::Premultiplied, [128, 0, 0, 128])]
        {
            let mut frame = Image::new(UVec2::new(1, 1), alpha, vec![0, 0, 0, 0]).unwrap();
            picture.blend_into(&mut frame, 0, 0, 1.0);
            assert_eq!(frame.pixel(0, 0), Some(expect), "{alpha:?}");
        }
        // Over an opaque white frame, half red.
        let mut frame = Image::new(UVec2::new(1, 1), AlphaEncoding::Straight, vec![255; 4]).unwrap();
        picture.blend_into(&mut frame, 0, 0, 1.0);
        assert_eq!(frame.pixel(0, 0), Some([255, 127, 127, 255]));
    }

    #[test]
    fn a_picture_hanging_off_the_frame_is_clipped() {
        let mut picture = Canvas::new(4, 4);
        picture.rgba8.fill(255);
        let mut frame = Image::new(UVec2::new(2, 2), AlphaEncoding::Straight, vec![0; 16]).unwrap();
        picture.blend_into(&mut frame, -3, 1, 1.0);
        assert_eq!(frame.pixel(0, 1), Some([255; 4]));
        assert_eq!(frame.pixel(1, 1), Some([0; 4]));
        assert_eq!(frame.pixel(0, 0), Some([0; 4]));
    }
}
