//! Text: the embedded Inter subset, shaped with kerning and tracking, rasterised with sub-pixel positioning.
//!
//! `ab_glyph` outlines and rasterises but does not shape, so the layout does the little shaping a label needs:
//! advance widths, the font's pair kerning (flattened into a `kern` table when the font was subset, see
//! `assets/fonts/NOTICES.md`), optional tracking and tabular digits. Glyphs are placed at fractional x and on whole-pixel
//! baselines, which keeps horizontal stems crisp.
//!
//! Text the Latin subset cannot draw takes the mixed path: each character picks the embedded font or a
//! [fallback](super::fallback) face, runs are put in visual order (`unicode-bidi`), and the fallback runs are shaped by
//! `harfrust` so Arabic joins, Devanagari conjuncts and Thai marks come out right. Latin-only text never leaves the
//! first path, so its pixels are the same as before.

use std::ops::Range;
use std::sync::Arc;

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, point};
use omuzaki_core::colour::Colour;
use unicode_bidi::{BidiClass, BidiInfo, bidi_class};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::canonical_combining_class;

use super::LayoutError;
use super::canvas::{Canvas, premultiply};
use super::fallback::{Face, Fallback, FontConfig, FontReport, is_default_ignorable, is_neutral};

/// The embedded Regular cut (SIL OFL 1.1; `assets/fonts/NOTICES.md`).
static REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.subset.ttf");
/// The embedded `SemiBold` cut.
static SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.subset.ttf");

/// The private-use code point that holds Inter's tabular `0`; `1`..`9` follow it (see `make_subset.py`).
const TABULAR_ZERO: u32 = 0xE030;
const ELLIPSIS: char = '\u{2026}';

/// Which cut of the font.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Weight {
    /// Body text and values.
    Regular,
    /// Names, headings and labels.
    SemiBold,
}

/// How a run of text looks. Sizes are **nominal points on an A4 sheet**; the layout scales them with the page
/// ([`Page::type_scale`](super::Page::type_scale)) so a sheet reads the same at any size.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Size in points.
    pub size_pt: f32,
    /// The cut.
    pub weight: Weight,
    /// The ink.
    pub colour: Colour,
    /// Extra space after every glyph, in ems. Small caps labels take about 0.1; large titles a little negative.
    pub tracking: f32,
    /// Use the fixed-width digits, so numbers line up in columns.
    pub tabular: bool,
    /// Draw lower case as capitals.
    pub caps: bool,
}

impl TextStyle {
    /// Regular text of `size_pt` in `colour`.
    pub const fn new(size_pt: f32, weight: Weight, colour: Colour) -> Self {
        Self { size_pt, weight, colour, tracking: 0.0, tabular: false, caps: false }
    }

    /// The same style with `tracking` ems of letter-spacing.
    #[must_use]
    pub const fn tracked(mut self, tracking: f32) -> Self {
        self.tracking = tracking;
        self
    }

    /// The same style with fixed-width digits.
    #[must_use]
    pub const fn tabular(mut self) -> Self {
        self.tabular = true;
        self
    }

    /// The same style in capitals.
    #[must_use]
    pub const fn caps(mut self) -> Self {
        self.caps = true;
        self
    }
}

/// Where the text's anchor x sits along the run.
///
/// **The rule for right-to-left text:** `Left`, `Centre` and `Right` are physical and never move with the script, and
/// the sheet's own layout (title block, headings, captions, rulers) is left to right whatever language a name is in:
/// an Arabic name is placed exactly where an English one would be, with its letters running right to left inside the
/// run. A caller laying out an RTL sheet or label asks for `Start` or `End`, which follow the text's own paragraph
/// direction (the first strong letter, UAX #9): for an RTL run `Start` is the right edge.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// The run starts at x.
    #[default]
    Left,
    /// The run is centred on x.
    Centre,
    /// The run ends at x.
    Right,
    /// The run begins at x in its own reading direction: the left edge for left-to-right text, the right edge for
    /// right-to-left text.
    Start,
    /// The run ends at x in its own reading direction: the right edge for left-to-right text, the left edge for
    /// right-to-left text.
    End,
}

/// What a style measures in pixels on a given page.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextMetrics {
    /// Height of a capital letter: the box labels are aligned to.
    pub cap_height: f32,
    /// Height of a lower-case `x`.
    pub x_height: f32,
    /// The em square.
    pub em: f32,
}

/// The two embedded cuts, and the fallback faces behind them.
#[derive(Clone)]
pub(super) struct Fonts {
    regular: FontRef<'static>,
    semibold: FontRef<'static>,
    fallback: Arc<Fallback>,
}

/// A glyph placed on a run: where it goes, relative to the run's origin and baseline.
struct Placed {
    /// The fallback face it comes from; `None` is the embedded font.
    face: Option<Arc<Face>>,
    id: GlyphId,
    x: f32,
    /// Raised above the baseline by this many pixels (Thai and Devanagari marks).
    y: f32,
}

/// A shaped run: glyphs with their offsets, the run's width and whether its paragraph reads right to left.
struct Run {
    glyphs: Vec<Placed>,
    width: f32,
    rtl: bool,
}

/// The face a character is drawn with; `None` is the embedded font.
type Choice = Option<Arc<Face>>;

/// A stretch of text in one face and one direction.
struct Segment {
    face: Choice,
    level: u8,
    chars: Range<usize>,
}

impl Fonts {
    /// Parses the embedded fonts; `config` says what may be used beyond them.
    pub(super) fn load(config: FontConfig) -> Result<Self, LayoutError> {
        let parse = |data: &'static [u8]| FontRef::try_from_slice(data).map_err(|e| LayoutError::Font(e.to_string()));
        Ok(Self { regular: parse(REGULAR)?, semibold: parse(SEMIBOLD)?, fallback: Arc::new(Fallback::new(config)) })
    }

    /// The fonts used and the characters no font could draw so far.
    pub(super) fn report(&self) -> FontReport {
        self.fallback.report()
    }

    fn cut(&self, weight: Weight) -> &FontRef<'static> {
        match weight {
            Weight::Regular => &self.regular,
            Weight::SemiBold => &self.semibold,
        }
    }

    /// Cap height, x-height and em of `style` at `em_px` pixels to the em.
    pub(super) fn metrics(&self, weight: Weight, em_px: f32) -> TextMetrics {
        let font = self.cut(weight);
        let per_unit = em_px / font.units_per_em().unwrap_or(1000.0);
        let height_of = |c: char| {
            // `ab_glyph` outlines are y-down, so the bounds' height is only its magnitude.
            font.outline(font.glyph_id(c)).map_or(0.0, |o| (o.bounds.max.y - o.bounds.min.y).abs() * per_unit)
        };
        TextMetrics { cap_height: height_of('H'), x_height: height_of('x'), em: em_px }
    }

    /// The width of `text` in pixels.
    pub(super) fn width(&self, text: &str, style: &TextStyle, em_px: f32) -> f32 {
        self.shape(text, style, em_px).width
    }

    /// `text` cut to at most `max_width` pixels, ending in an ellipsis when something was removed.
    pub(super) fn fit(&self, text: &str, style: &TextStyle, em_px: f32, max_width: f32) -> String {
        if self.width(text, style, em_px) <= max_width {
            return text.to_owned();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let candidate: String = chars.iter().collect::<String>().trim_end().to_owned() + &ELLIPSIS.to_string();
            if self.width(&candidate, style, em_px) <= max_width {
                return candidate;
            }
        }
        ELLIPSIS.to_string()
    }

    /// Draws `text` with its anchor at `x` and its baseline on row `baseline`. Returns the run's width.
    pub(super) fn draw(
        &self,
        canvas: &mut Canvas,
        text: &str,
        style: &TextStyle,
        em_px: f32,
        (x, baseline): (f32, i32),
        align: Align,
    ) -> f32 {
        let primary = self.cut(style.weight);
        let run = self.shape(text, style, em_px);
        let start = match (align, run.rtl) {
            (Align::Left | Align::Start, false) | (Align::Left | Align::End, true) => x,
            (Align::Centre, _) => x - run.width / 2.0,
            (Align::Right | Align::End, false) | (Align::Right | Align::Start, true) => x - run.width,
        };
        let ink = premultiply(style.colour);
        for placed in run.glyphs {
            let at = (start + placed.x, baseline as f32 - placed.y);
            match &placed.face {
                Some(face) => blit(canvas, &face.outlines, placed.id, em_px, at, ink),
                None => blit(canvas, primary, placed.id, em_px, at, ink),
            }
        }
        run.width
    }

    fn shape(&self, text: &str, style: &TextStyle, em_px: f32) -> Run {
        let primary = self.cut(style.weight);
        // Composed form first, so a letter and its accent are one character wherever a font has the precomposed glyph.
        let composed: String = text.nfc().collect();
        let chars: Vec<char> = composed
            .chars()
            .flat_map(|c| decompose_if_embedded_has_the_parts(primary, c))
            .flat_map(|c| if style.caps { c.to_uppercase().collect::<Vec<_>>() } else { vec![c] })
            .collect();
        if chars.iter().all(|&c| has_glyph(primary, tabular(c, style.tabular))) {
            self.shape_embedded(&chars, style, em_px)
        } else {
            self.shape_mixed(&chars, style, em_px)
        }
    }

    /// Text the embedded font covers entirely: advances, pair kerning and tracking.
    fn shape_embedded(&self, chars: &[char], style: &TextStyle, em_px: f32) -> Run {
        let font = self.cut(style.weight);
        let tracking = style.tracking * em_px;
        let mut pen = Pen::new(tracking);
        pen.embedded(font, chars.iter().copied(), false, style, em_px);
        pen.finish(false)
    }

    /// Text that needs a fallback face somewhere (or has a character nothing draws).
    fn shape_mixed(&self, chars: &[char], style: &TextStyle, em_px: f32) -> Run {
        let font = self.cut(style.weight);
        let chosen = self.choose_faces(chars, style);
        let text: Vec<char> = chosen.iter().map(|(c, _)| *c).collect();
        let (levels, rtl) = bidi_levels(&text);
        let segments = split_segments(&chosen, &levels);
        let order = visual_order(&segments.iter().map(|s| s.level).collect::<Vec<_>>());
        let mut pen = Pen::new(style.tracking * em_px);
        for segment in order.into_iter().map(|i| &segments[i]) {
            let run = &text[segment.chars.clone()];
            let reversed = segment.level % 2 == 1;
            match &segment.face {
                None if reversed => pen.embedded(font, clusters_reversed(run).into_iter(), true, style, em_px),
                None => pen.embedded(font, run.iter().copied(), false, style, em_px),
                Some(face) => {
                    if !pen.shaped(face, run, reversed, em_px) {
                        for &c in run {
                            self.fallback.note_missing(c, "the font failed to shape it".to_owned(), false);
                        }
                    }
                }
            }
        }
        pen.finish(rtl)
    }

    /// Picks a face for every character. Spaces and brackets stay in the embedded font; other punctuation and joiners
    /// follow the text beside them; combining marks follow their base; a joiner no face
    /// can draw is dropped (it only shapes its neighbours); anything else no font covers is recorded as missing and
    /// left to the embedded font's missing-glyph box.
    fn choose_faces(&self, chars: &[char], style: &TextStyle) -> Vec<(char, Choice)> {
        let primary = self.cut(style.weight);
        let mut out: Vec<(char, Choice)> = Vec::with_capacity(chars.len());
        let mut previous: Choice = None;
        for &c in chars {
            let in_primary = has_glyph(primary, tabular(c, style.tabular));
            // A space is the embedded font's: the Latin gap looks the same beside every script (a CJK or emoji font's
            // space is much wider).
            let choice = if (c.is_whitespace() || mirrored(c).is_some()) && in_primary {
                None
            } else if is_combining(c) && !out.is_empty() {
                // A mark stays with its base: in the base's face when that has it, else the embedded font's.
                match &previous {
                    Some(face) if face.covers(c) => Some(Arc::clone(face)),
                    _ if in_primary => None,
                    _ => self.fallback.face_for(c, style.weight),
                }
            } else if is_neutral(c) {
                match &previous {
                    Some(face) if face.covers(c) => Some(Arc::clone(face)),
                    _ if in_primary => None,
                    _ if is_default_ignorable(c) => continue,
                    _ => {
                        self.fallback.note_missing(c, "no font has a glyph for it".to_owned(), false);
                        None
                    }
                }
            } else if in_primary {
                None
            } else {
                self.fallback.face_for(c, style.weight)
            };
            previous.clone_from(&choice);
            out.push((c, choice));
        }
        out
    }
}

/// The pen of a run being laid out: glyphs so far, the advance, and the letter-spacing still hanging after the last.
struct Pen {
    glyphs: Vec<Placed>,
    x: f32,
    tracking: f32,
    /// Tracking added after the last glyph: space after the run, not part of it.
    trailing: f32,
}

impl Pen {
    fn new(tracking: f32) -> Self {
        Self { glyphs: Vec::new(), x: 0.0, tracking, trailing: 0.0 }
    }

    /// Lays `chars` out in the embedded `font`, kerned within the call. `mirror` swaps brackets for their mirror
    /// images, as a right-to-left run needs (UAX #9 rule L4). A combining mark is centred on the base before it and
    /// takes no advance (Inter's marks are zero-width; its positioning tables are not kept in the subset).
    fn embedded(
        &mut self,
        font: &FontRef<'static>,
        chars: impl Iterator<Item = char>,
        mirror: bool,
        style: &TextStyle,
        em_px: f32,
    ) {
        let scaled = font.as_scaled(px_scale(font, em_px));
        let per_unit = em_px / font.units_per_em().unwrap_or(1000.0);
        let mut previous = None::<GlyphId>;
        // The last base glyph: its id, where it was placed and whether it was a capital.
        let mut base = None::<(GlyphId, f32, bool)>;
        for c in chars {
            let drawn = if mirror { mirrored(c).unwrap_or(c) } else { c };
            let id = font.glyph_id(tabular(drawn, style.tabular));
            if let (true, Some((base_id, base_x, capital))) = (is_combining(c), base) {
                let centre = outline_centre(font, id, per_unit);
                let lift = if capital { cap_lift(font, per_unit) } else { 0.0 };
                let x = base_x + scaled.h_advance(base_id) / 2.0 - centre;
                self.glyphs.push(Placed { face: None, id, x, y: lift });
                continue;
            }
            if let Some(prev) = previous {
                self.x += scaled.kern(prev, id);
            }
            self.glyphs.push(Placed { face: None, id, x: self.x, y: 0.0 });
            base = Some((id, self.x, c.is_uppercase()));
            self.x += scaled.h_advance(id) + self.tracking;
            self.trailing = self.tracking;
            previous = Some(id);
        }
    }

    /// Shapes `chars` in a fallback `face` (right to left when `rtl`) and lays the glyphs out. `false` if the shaper
    /// refused the text.
    fn shaped(&mut self, face: &Arc<Face>, chars: &[char], rtl: bool, em_px: f32) -> bool {
        let font = harfrust::ShaperFont::new(&face.shaper);
        let mut buffer = harfrust::Buffer::new();
        buffer.push_str(&chars.iter().collect::<String>());
        buffer.guess_segment_properties();
        buffer.set_direction(if rtl { harfrust::Direction::RightToLeft } else { harfrust::Direction::LeftToRight });
        if harfrust::shape(&font, &mut buffer, harfrust::ShapeOptions::new()).is_err() {
            return false;
        }
        let per_unit = em_px / f32::from(face.shaper.units_per_em().max(1));
        for (info, position) in buffer.glyph_infos().iter().zip(buffer.glyph_positions()) {
            // Glyph ids are 16 bit in every font the catalog pins.
            let id = GlyphId(u16::try_from(info.glyph_id).unwrap_or(0));
            self.glyphs.push(Placed {
                face: Some(Arc::clone(face)),
                id,
                x: self.x + position.x_offset as f32 * per_unit,
                y: position.y_offset as f32 * per_unit,
            });
            self.x += position.x_advance as f32 * per_unit;
            // Letter-spacing would pull joined letters apart, and marks (no advance) travel with their base.
            self.trailing = 0.0;
            if !face.connected && position.x_advance != 0 {
                self.x += self.tracking;
                self.trailing = self.tracking;
            }
        }
        true
    }

    fn finish(self, rtl: bool) -> Run {
        Run { width: if self.glyphs.is_empty() { 0.0 } else { self.x - self.trailing }, glyphs: self.glyphs, rtl }
    }
}

/// Rasterises glyph `id` of `font` with its origin at `(x, y)` and blends it onto `canvas`.
fn blit<F: Font>(canvas: &mut Canvas, font: &F, id: GlyphId, em_px: f32, (x, y): (f32, f32), ink: [u8; 4]) {
    let glyph = id.with_scale_and_position(px_scale(font, em_px), point(x, y));
    let Some(outlined) = font.outline_glyph(glyph) else { return };
    let origin = outlined.px_bounds().min;
    let (ox, oy) = (origin.x as i32, origin.y as i32);
    outlined.draw(|gx, gy, coverage| {
        canvas.blend(ox + gx as i32, oy + gy as i32, ink, (coverage.clamp(0.0, 1.0) * 255.0 + 0.5) as u32);
    });
}

/// `c` itself, unless the embedded font lacks it but has its letter and marks (a dotted `ọ` the Latin subset has no
/// glyph for): then those parts, which draw as a base with a mark on it.
fn decompose_if_embedded_has_the_parts(font: &FontRef<'_>, c: char) -> Vec<char> {
    if has_glyph(font, c) {
        return vec![c];
    }
    let parts: Vec<char> = std::iter::once(c).nfd().collect();
    if parts.len() > 1 && parts.iter().all(|&p| has_glyph(font, p)) { parts } else { vec![c] }
}

/// Whether `font` has a glyph for `c` (id 0 is the missing-glyph box).
fn has_glyph(font: &FontRef<'_>, c: char) -> bool {
    font.glyph_id(c).0 != 0
}

/// The embedding level of each character (UAX #9: even is left to right) and whether the paragraph reads right to
/// left (decided by its first strong letter). Text with no right-to-left letters is all level 0 without running the
/// algorithm.
fn bidi_levels(chars: &[char]) -> (Vec<u8>, bool) {
    let has_rtl = chars.iter().any(|&c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL));
    if !has_rtl {
        return (vec![0; chars.len()], false);
    }
    let text: String = chars.iter().collect();
    let info = BidiInfo::new(&text, None);
    let levels =
        text.char_indices().map(|(byte, _)| info.levels.get(byte).map_or(0, unicode_bidi::Level::number)).collect();
    (levels, info.paragraphs.first().is_some_and(|p| p.level.is_rtl()))
}

/// Whether `c` combines with the character before it (accents and other marks that have no width of their own).
fn is_combining(c: char) -> bool {
    canonical_combining_class(c) != 0
        || matches!(u32::from(c), 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// The character that draws `c` mirrored, for the brackets a right-to-left run reverses (UAX #9 rule L4).
fn mirrored(c: char) -> Option<char> {
    const PAIRS: [(char, char); 7] = [
        ('(', ')'),
        ('[', ']'),
        ('{', '}'),
        ('<', '>'),
        ('\u{AB}', '\u{BB}'),
        ('\u{2039}', '\u{203A}'),
        ('\u{2264}', '\u{2265}'),
    ];
    PAIRS.iter().find_map(|&(open, close)| match c {
        _ if c == open => Some(close),
        _ if c == close => Some(open),
        _ => None,
    })
}

/// `run` in reverse reading order for a right-to-left stretch, with each base keeping its marks behind it.
fn clusters_reversed(run: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(run.len());
    let mut end = run.len();
    while end > 0 {
        let mut start = end - 1;
        while start > 0 && is_combining(run[start]) {
            start -= 1;
        }
        out.extend_from_slice(&run[start..end]);
        end = start;
    }
    out
}

/// The horizontal centre of glyph `id`'s outline in pixels, relative to its origin.
fn outline_centre(font: &FontRef<'_>, id: GlyphId, per_unit: f32) -> f32 {
    font.outline(id).map_or(0.0, |o| f32::midpoint(o.bounds.min.x, o.bounds.max.x) * per_unit)
}

/// How far a mark over a capital sits above the position designed for lower case: cap height minus x-height.
fn cap_lift(font: &FontRef<'_>, per_unit: f32) -> f32 {
    let height_of =
        |c: char| font.outline(font.glyph_id(c)).map_or(0.0, |o| (o.bounds.max.y - o.bounds.min.y).abs() * per_unit);
    height_of('H') - height_of('x')
}

/// Groups neighbouring characters with the same face and level.
fn split_segments(chosen: &[(char, Choice)], levels: &[u8]) -> Vec<Segment> {
    let same = |a: &Choice, b: &Choice| match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
        _ => false,
    };
    let mut segments: Vec<Segment> = Vec::new();
    for (i, ((_, face), &level)) in chosen.iter().zip(levels).enumerate() {
        match segments.last_mut() {
            Some(last) if last.level == level && same(&last.face, face) => last.chars.end = i + 1,
            _ => segments.push(Segment { face: face.clone(), level, chars: i..i + 1 }),
        }
    }
    segments
}

/// Segment indices in left-to-right display order (UAX #9 rule L2): from the highest level down to the lowest odd one,
/// every run of segments at that level or deeper is reversed.
fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let Some(&highest) = levels.iter().max() else { return order };
    let Some(lowest_odd) = levels.iter().copied().filter(|l| l % 2 == 1).min() else { return order };
    for level in (lowest_odd..=highest).rev() {
        let mut i = 0;
        while i < order.len() {
            if levels[order[i]] < level {
                i += 1;
                continue;
            }
            let end = (i..order.len()).find(|&j| levels[order[j]] < level).unwrap_or(order.len());
            order[i..end].reverse();
            i = end;
        }
    }
    order
}

/// `ab_glyph` sizes a font by its whole line height; this is the scale that makes the em `em_px` pixels.
fn px_scale<F: Font + ?Sized>(font: &F, em_px: f32) -> PxScale {
    PxScale::from(em_px * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0))
}

/// The tabular form of a digit when asked for, else the character itself.
fn tabular(c: char, on: bool) -> char {
    match (on, c.to_digit(10)) {
        (true, Some(d)) => char::from_u32(TABULAR_ZERO + d).unwrap_or(c),
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts() -> Fonts {
        Fonts::load(FontConfig::latin_only()).unwrap()
    }

    #[test]
    fn both_cuts_are_embedded_and_small() {
        assert!(REGULAR.len() + SEMIBOLD.len() < 300 * 1024, "the font must stay small");
        fonts();
    }

    #[test]
    fn metrics_scale_with_the_em() {
        let f = fonts();
        let m = f.metrics(Weight::Regular, 100.0);
        // Inter: cap height 1490 / 2048, x-height 1118 / 2048.
        assert!((m.cap_height - 72.7).abs() < 0.5, "{m:?}");
        assert!((m.x_height - 54.6).abs() < 0.5, "{m:?}");
    }

    #[test]
    fn kerning_pulls_av_together_and_tracking_spreads() {
        let f = fonts();
        let style = TextStyle::new(10.0, Weight::Regular, Colour::BLACK);
        let separate = f.width("A", &style, 100.0) + f.width("V", &style, 100.0);
        assert!(f.width("AV", &style, 100.0) < separate - 3.0, "kern pair AV");
        let tracked = f.width("AV", &style.tracked(0.1), 100.0);
        assert!((tracked - f.width("AV", &style, 100.0) - 10.0).abs() < 0.01, "one gap of 0.1 em");
    }

    #[test]
    fn tabular_digits_share_a_width_and_proportional_ones_do_not() {
        let f = fonts();
        let plain = TextStyle::new(10.0, Weight::Regular, Colour::BLACK);
        assert!((f.width("1", &plain.tabular(), 100.0) - f.width("0", &plain.tabular(), 100.0)).abs() < 0.01);
        assert!((f.width("1", &plain, 100.0) - f.width("0", &plain, 100.0)).abs() > 10.0);
    }

    #[test]
    fn fit_ends_in_an_ellipsis_and_respects_the_width() {
        let f = fonts();
        let style = TextStyle::new(10.0, Weight::Regular, Colour::BLACK);
        let full = f.width("Hello wide world", &style, 40.0);
        assert_eq!(f.fit("Hello wide world", &style, 40.0, full), "Hello wide world");
        let cut = f.fit("Hello wide world", &style, 40.0, full / 2.0);
        assert!(cut.ends_with(ELLIPSIS) && f.width(&cut, &style, 40.0) <= full / 2.0, "{cut}");
    }

    #[test]
    fn drawing_inks_the_pixels_under_the_glyph_only() {
        let f = fonts();
        let mut canvas = Canvas::new(80, 40, Colour::WHITE);
        let style = TextStyle::new(10.0, Weight::SemiBold, Colour::BLACK);
        let width = f.draw(&mut canvas, "H", &style, 30.0, (10.0, 30), Align::Left);
        let image = canvas.into_rgba();
        let inked = |x: u32, y: u32| image.get_pixel(x, y).0[0] < 128;
        assert!(width > 15.0 && inked(14, 20), "the stem of the H is dark");
        assert!(!inked(60, 20) && !inked(14, 35), "nothing outside the glyph");
    }

    #[test]
    fn visual_order_reverses_right_to_left_runs_and_keeps_numbers_in_reading_order() {
        assert_eq!(visual_order(&[0, 0]), vec![0, 1]);
        assert_eq!(visual_order(&[1, 1, 1]), vec![2, 1, 0]);
        // Latin, Arabic, Latin: the Arabic run stays between them.
        assert_eq!(visual_order(&[0, 1, 0]), vec![0, 1, 2]);
        // A number (level 2) inside Arabic (level 1): the pair flips as a block, the number's own order survives.
        assert_eq!(visual_order(&[1, 2, 1]), vec![2, 1, 0]);
        assert_eq!(visual_order(&[1, 2, 2, 1]), vec![3, 1, 2, 0]);
        assert_eq!(visual_order(&[]), Vec::<usize>::new());
    }

    #[test]
    fn bidi_levels_are_zero_without_right_to_left_letters() {
        assert_eq!(bidi_levels(&['a', 'b']), (vec![0, 0], false));
        assert_eq!(bidi_levels(&['a', ' ', 'م']), (vec![0, 0, 1], false));
        assert_eq!(bidi_levels(&['م', ' ', 'a']), (vec![1, 1, 2], true));
    }

    /// The ids of the embedded-font glyphs of `text`, left to right.
    fn embedded_ids(f: &Fonts, text: &str) -> Vec<u16> {
        let style = TextStyle::new(10.0, Weight::Regular, Colour::BLACK);
        f.shape(text, &style, 40.0).glyphs.iter().filter(|g| g.face.is_none()).map(|g| g.id.0).collect()
    }

    #[test]
    fn brackets_in_a_right_to_left_run_are_mirrored() {
        let f = Fonts::load(FontConfig::latin_only()).unwrap();
        let id = |c: char| f.cut(Weight::Regular).glyph_id(c).0;
        let (open, close) = (id('('), id(')'));
        // "م (A)" in an Arabic paragraph shows as "(A) م": the first bracket of the logical text is the right-hand one
        // and takes the closing shape; unmirrored it showed ")A(".
        let ids = embedded_ids(&f, "\u{645} (A)");
        let shown: Vec<u16> = ids.into_iter().filter(|g| [open, close, id('A')].contains(g)).collect();
        assert_eq!(shown, vec![open, id('A'), close]);
        // In a left-to-right paragraph the same brackets keep their shapes.
        assert_eq!(embedded_ids(&f, "(A)"), vec![open, id('A'), close]);
        assert_eq!(embedded_ids(&f, "A (\u{645})").iter().filter(|g| **g == open || **g == close).count(), 2);
    }

    #[test]
    fn clusters_keep_their_marks_when_a_run_is_reversed() {
        let run: Vec<char> = "ab\u{301}c".chars().collect();
        assert_eq!(clusters_reversed(&run), vec!['c', 'b', '\u{301}', 'a']);
        assert!(is_combining('\u{301}') && !is_combining('e'));
    }
}
