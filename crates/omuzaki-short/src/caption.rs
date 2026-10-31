//! Burned-in captions: a phrase in white letters with a dark outline and a soft drop shadow, centred at the caption
//! line, as short-form clips caption speech. The look ([`CaptionLook`]) sets the weight, the outline, the shadow and
//! whether a caption pops in slightly larger and settles over [`POP_S`]; the short's `[captions]` can change each.
//!
//! The face is Bricolage Grotesque (SIL OFL 1.1, `assets/fonts/`), a variable font drawn at the look's weight. Glyphs
//! are laid out on one line with the font's advances and kerning (no shaping: the captions are the script's own words)
//! and shrunk to fit the look's width ([`MAX_WIDTH_SHARE`] of the frame unless the short sets its own).

use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont, VariableFont, point};

use crate::def::{CaptionLook, CaptionStyle, CaptionsDef};
use crate::error::ShortError;
use crate::timeline::CaptionSpan;

static FACE: &[u8] = include_bytes!("../assets/fonts/BricolageGrotesque.ttf");

/// The weight the variable face is drawn at by the punchy look.
pub const WEIGHT: f32 = 800.0;
/// The widest a caption may be by default, as a share of the frame width.
pub const MAX_WIDTH_SHARE: f32 = 0.9;
/// How long a caption takes to settle from its pop-in size, seconds.
pub const POP_S: f32 = 0.09;
/// How much larger a caption starts.
const POP_SCALE: f32 = 1.12;
/// The shadow's drop, as a share of the letter height.
const SHADOW_SHARE: f32 = 0.06;
const FILL: [f32; 3] = [1.0, 1.0, 1.0];
const LINE: [f32; 3] = [0.06, 0.05, 0.07];

/// How a short's captions are drawn: its look with the short's changes applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ink {
    /// The weight of the type.
    pub weight: f32,
    /// The outline's width, as a share of the letter height; 0 for none.
    pub outline: f32,
    /// The drop shadow's darkness, `0..=1`.
    pub shadow: f32,
    /// Whether a caption pops in.
    pub pop: bool,
    /// How long a caption fades in, seconds; 0 to appear at once.
    pub fade: f32,
    /// How fast a caption drifts up, as a share of the frame height per second.
    pub rise: f32,
    /// The widest a caption may be, as a share of the frame width; a longer phrase is drawn smaller.
    pub width: f32,
    /// The letters' colour, `0..=1` sRGB.
    pub fill: [f32; 3],
    /// The outline's colour, `0..=1` sRGB.
    pub line: [f32; 3],
}

impl Ink {
    /// The ink of `look` as it stands.
    pub fn of_look(look: CaptionLook) -> Self {
        let (weight, outline, shadow, pop) = match look {
            CaptionLook::Punchy => (WEIGHT, 0.13, 0.45, true),
            CaptionLook::Clean => (700.0, 0.065, 0.25, false),
            CaptionLook::Outline => (600.0, 0.1, 0.0, false),
        };
        Self { weight, outline, shadow, pop, fade: 0.0, rise: 0.0, width: MAX_WIDTH_SHARE, fill: FILL, line: LINE }
    }

    /// The ink of `captions`: its look, with each field it sets in place of the look's.
    pub fn of(captions: &CaptionsDef) -> Self {
        let look = Self::of_look(captions.look);
        let rgb = |c: omuzaki_render_api::Colour| [c.r, c.g, c.b].map(|v| f32::from(v) / 255.0);
        Self {
            weight: captions.weight.unwrap_or(look.weight),
            outline: captions.outline.unwrap_or(look.outline),
            shadow: captions.shadow.unwrap_or(look.shadow),
            pop: captions.pop.unwrap_or(look.pop),
            fade: captions.fade.unwrap_or(look.fade),
            rise: captions.rise.unwrap_or(look.rise),
            width: captions.width.unwrap_or(look.width),
            fill: captions.fill.map_or(look.fill, rgb),
            line: captions.line.map_or(look.line, rgb),
        }
    }
}

impl Default for Ink {
    fn default() -> Self {
        Self::of_look(CaptionLook::default())
    }
}

/// Draws captions onto frames.
pub struct Captioner {
    font: FontRef<'static>,
    ink: Ink,
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
    /// Outline coverage (the fill grown by the outline width; the fill itself when there is no outline), `0..1`.
    line: Vec<f32>,
    /// Shadow drop, pixels.
    drop: usize,
}

impl Captioner {
    /// A captioner with the bundled face, drawing in `ink`.
    ///
    /// # Errors
    /// [`ShortError::Render`] if the bundled face doesn't load (a build fault).
    pub fn new(ink: Ink) -> Result<Self, ShortError> {
        let mut font =
            FontRef::try_from_slice(FACE).map_err(|e| ShortError::Render(format!("the caption face: {e}")))?;
        font.set_variation(b"wght", ink.weight);
        Ok(Self { font, ink })
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

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        clippy::cast_possible_wrap
    )]
    fn mask(&self, text: &str, height: f32, max_width: f32) -> Mask {
        let (_, natural) = self.layout(text, height);
        let height = if natural > max_width { height * max_width / natural } else { height };
        let (glyphs, advance) = self.layout(text, height);
        let scaled = self.font.as_scaled(PxScale::from(height));
        let line = height * self.ink.outline;
        let line = if line > 0.0 { line.max(1.0) } else { 0.0 };
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
        let line = if line > 0.0 { grow(&fill, width, rows, line) } else { fill.clone() };
        Mask { width, height: rows, line, fill, drop }
    }

    /// Draws `caption` onto `rgba` (straight alpha, `size` pixels) at `t` seconds, its middle at `y` (share of the
    /// height) unless the caption sets its own, its letters `letter` of the width tall. It fades in over the ink's
    /// `fade` and drifts up at its `rise` from where it starts.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        clippy::cast_possible_wrap,
        reason = "pixel positions within frames of at most 4096 a side"
    )]
    pub fn draw(&self, rgba: &mut [u8], size: [u32; 2], caption: &CaptionSpan, t: f32, y: f32, letter: f32) {
        let [width, height] = size;
        let pop = if self.ink.pop { ((t - caption.start) / POP_S).clamp(0.0, 1.0) } else { 1.0 };
        let scale = POP_SCALE + (1.0 - POP_SCALE) * (1.0 - (1.0 - pop) * (1.0 - pop));
        let letter = Self::height(caption.style, width, letter) * scale;
        let mask = self.mask(&caption.text, letter, width as f32 * self.ink.width);
        let shown = (t - caption.start).max(0.0);
        let strength = if self.ink.fade > 0.0 { (shown / self.ink.fade).min(1.0) } else { 1.0 };
        let y = caption.y.unwrap_or(y) - self.ink.rise * shown;
        let left = (i64::from(width) - mask.width as i64) / 2;
        let top = (height as f32 * y) as i64 - (mask.height as i64) / 2;
        let at = |mx: usize, my: usize| -> Option<usize> {
            let x = left + mx as i64;
            let y = top + my as i64;
            (x >= 0 && y >= 0 && x < i64::from(width) && y < i64::from(height))
                .then(|| (y as usize * width as usize + x as usize) * 4)
        };
        let blend = |rgba: &mut [u8], i: usize, colour: [f32; 3], a: f32| {
            let a = a * strength;
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
                if shadow > 0.0 && self.ink.shadow > 0.0 {
                    blend(rgba, i, [0.0, 0.0, 0.0], shadow * self.ink.shadow);
                }
                let k = my * mask.width + mx;
                if mask.line[k] > 0.0 && self.ink.outline > 0.0 {
                    blend(rgba, i, self.ink.line, mask.line[k]);
                }
                if mask.fill[k] > 0.0 {
                    blend(rgba, i, self.ink.fill, mask.fill[k]);
                }
            }
        }
    }
}

/// `cover` grown by a disc of `radius` pixels (a max filter), with an anti-aliased edge.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
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
        let captioner = Captioner::new(Ink::default()).unwrap();
        let (w, h) = (360u32, 640u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for px in rgba.chunks_mut(4) {
            px.copy_from_slice(&[90, 120, 200, 255]);
        }
        let caption =
            CaptionSpan { start: 0.0, end: 1.0, text: "Hello there!".into(), style: CaptionStyle::Normal, y: None };
        captioner.draw(&mut rgba, [w, h], &caption, 0.5, 0.5, 0.06);
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
        let captioner = Captioner::new(Ink::default()).unwrap();
        let mask = captioner.mask("A VERY LONG SHOUTED CAPTION THAT GOES ON", 80.0, 300.0);
        assert!(mask.width <= 300 + 40, "{}", mask.width);
    }

    #[test]
    fn a_short_narrows_the_widest_caption() {
        // The columns a long caption's letters cover on a black frame.
        let lit = |ink: Ink| {
            let captioner = Captioner::new(ink).unwrap();
            let (w, h) = (360u32, 640u32);
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            let caption = CaptionSpan {
                start: 0.0,
                end: 1.0,
                text: "A long phrase that has to shrink".into(),
                style: CaptionStyle::Normal,
                y: None,
            };
            captioner.draw(&mut rgba, [w, h], &caption, 0.5, 0.5, 0.12);
            let cols: Vec<u32> = (0..w).filter(|&x| (0..h).any(|y| rgba[((y * w + x) * 4) as usize] > 200)).collect();
            cols.last().unwrap() - cols.first().unwrap()
        };
        let clean = Ink::of_look(CaptionLook::Clean);
        let wide = lit(clean);
        let narrow = lit(Ink { width: 0.6, ..clean });
        assert!(wide > 300 && wide <= 324, "the default width, 0.9 of 360: {wide}");
        assert!(narrow > 190 && narrow <= 216, "0.6 of 360: {narrow}");
    }

    #[test]
    fn a_look_changes_the_weight_the_outline_and_the_pop() {
        let draw = |ink: Ink, t: f32| {
            let captioner = Captioner::new(ink).unwrap();
            let (w, h) = (360u32, 640u32);
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            for px in rgba.chunks_mut(4) {
                px.copy_from_slice(&[90, 120, 200, 255]);
            }
            let caption =
                CaptionSpan { start: 0.0, end: 1.0, text: "Hello there!".into(), style: CaptionStyle::Normal, y: None };
            captioner.draw(&mut rgba, [w, h], &caption, t, 0.5, 0.06);
            let white = rgba.chunks(4).filter(|p| p[0] > 245 && p[1] > 245 && p[2] > 245).count();
            let dark = rgba.chunks(4).filter(|p| p[0] < 30 && p[1] < 30 && p[2] < 30).count();
            (white, dark)
        };
        let (punchy_white, punchy_dark) = draw(Ink::of_look(CaptionLook::Punchy), 0.5);
        let (clean_white, clean_dark) = draw(Ink::of_look(CaptionLook::Clean), 0.5);
        assert!(clean_white < punchy_white, "lighter type: {clean_white} vs {punchy_white}");
        assert!(clean_dark * 2 < punchy_dark, "a thin edge: {clean_dark} vs {punchy_dark}");
        let bare = Ink { outline: 0.0, shadow: 0.0, ..Ink::of_look(CaptionLook::Clean) };
        assert_eq!(draw(bare, 0.5).1, 0, "no outline and no shadow leave nothing dark");
        // A steady look draws the same at its first frame as later; the punchy one starts larger.
        assert_eq!(draw(Ink::of_look(CaptionLook::Clean), 0.0), draw(Ink::of_look(CaptionLook::Clean), 0.5));
        assert!(draw(Ink::of_look(CaptionLook::Punchy), 0.0).0 > punchy_white, "it pops in larger");
    }

    #[test]
    fn a_short_changes_one_thing_about_its_look() {
        let captions = CaptionsDef {
            look: CaptionLook::Clean,
            outline: Some(0.0),
            fill: Some(omuzaki_render_api::Colour::rgb(255, 240, 200)),
            ..CaptionsDef::default()
        };
        let ink = Ink::of(&captions);
        assert!((ink.weight - 700.0).abs() < 1e-6, "the look's weight stays");
        assert!(ink.outline.abs() < 1e-6);
        assert!((ink.fill[2] - 200.0 / 255.0).abs() < 1e-6);
        assert_eq!(Ink::of(&CaptionsDef::default()), Ink::default());
    }

    #[test]
    #[allow(clippy::cast_precision_loss, reason = "row numbers")]
    fn a_caption_fades_in_drifts_up_and_sits_at_its_own_height() {
        // The rows a caption's letters cover and how bright its brightest letter pixel is, at `t`.
        let draw = |ink: Ink, y: Option<f32>, t: f32| {
            let captioner = Captioner::new(ink).unwrap();
            let (w, h) = (360u32, 640u32);
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            for px in rgba.chunks_mut(4) {
                px.copy_from_slice(&[0, 0, 0, 255]);
            }
            let caption =
                CaptionSpan { start: 1.0, end: 3.0, text: "Hello there!".into(), style: CaptionStyle::Normal, y };
            captioner.draw(&mut rgba, [w, h], &caption, t, 0.5, 0.06);
            let lit: Vec<u32> = (0..h)
                .filter(|&row| rgba[(row * w * 4) as usize..((row + 1) * w * 4) as usize].chunks(4).any(|p| p[0] > 60))
                .collect();
            let peak = rgba.chunks(4).map(|p| p[0]).max().unwrap();
            (lit.iter().sum::<u32>() as f32 / lit.len().max(1) as f32, peak)
        };
        let steady = Ink::of_look(CaptionLook::Clean);
        let (middle, full) = draw(steady, None, 1.5);
        assert!((middle - 320.0).abs() < 12.0, "the short's caption line: {middle}");
        assert_eq!(full, 255);
        // The line's own height wins over the short's.
        let (own, _) = draw(steady, Some(0.25), 1.5);
        assert!((own - 160.0).abs() < 12.0, "{own}");
        // A fade starts faint and reaches full strength once its time is up.
        let fading = Ink { fade: 0.2, ..steady };
        let (_, first) = draw(fading, None, 1.0);
        let (_, halfway) = draw(fading, None, 1.1);
        let (_, done) = draw(fading, None, 1.3);
        assert!(first < 10, "nothing at the first instant: {first}");
        assert!((110..=150).contains(&halfway), "half strength halfway: {halfway}");
        assert_eq!(done, 255);
        // A rise lifts it by its rate times the time it has shown: 0.1 of 640 rows a second, 32 rows after 0.5 s.
        let rising = Ink { rise: 0.1, ..steady };
        let (start, _) = draw(rising, None, 1.0);
        let (later, _) = draw(rising, None, 1.5);
        assert!((start - middle).abs() < 1.0, "it starts on its line: {start} against {middle}");
        assert!((start - later - 32.0).abs() < 2.0, "{start} -> {later}");
    }
}
