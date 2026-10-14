//! The character-sheet layout engine (spec v3 EX-10, §32): page grid, labels, palette swatches, proportion rulers and a
//! title block, composed as **pure images**. It needs no renderer: the turnaround and expression sheets
//! hand it the pictures they rendered and the numbers they measured, and export writes what it returns.
//!
//! # How a sheet is built
//!
//! [`SheetLayout`] is a canvas with a design system attached. A caller picks a [`Page`] and a [`Theme`], then works
//! top-down in whole pixels:
//!
//! 1. [`title_block`](SheetLayout::title_block) takes the header and leaves the rest of the page as
//!    [`content`](SheetLayout::content);
//! 2. [`Rect`] splits that area into rows, columns and grids ([`Rect::rows`], [`Rect::columns`], [`Rect::grid`]);
//! 3. [`section_heading`](SheetLayout::section_heading) titles an area, [`place`](SheetLayout::place) puts a [`Cell`]
//!    (a picture and its caption) in a grid cell, [`swatches`](SheetLayout::swatches) draws the palette and
//!    [`rulers`](SheetLayout::rulers) lays the head-unit rulers over a turnaround;
//! 4. [`finish`](SheetLayout::finish) returns the picture.
//!
//! Drawing is immediate: whatever is drawn later is on top, so rulers drawn *before* the figures sit behind them and
//! never cut across the art.
//!
//! # Type
//!
//! Text is Inter (SIL OFL 1.1, a 107 KB Latin subset embedded in the crate, `assets/fonts/NOTICES.md`) rasterised by
//! `ab_glyph` (Apache-2.0). Sizes are nominal points on an A4 sheet and scale with the page ([`Page::type_scale`]), so
//! the same sheet reads alike at 150 and 300 dpi and on a board. Labels are positioned by the top of their capitals
//! rather than by line box, which is what makes the grid look aligned.
//!
//! Names and labels in other scripts (CJK, Arabic, Devanagari, Thai, emoji) use Noto fallback fonts that are downloaded
//! on first need, checked against a pinned SHA-256 and cached (`FontConfig`, [`SheetLayout::with_fonts`]); the Han forms
//! follow the sheet's language ([`HanVariant`]), and [`SheetLayout::font_report`] lists the fonts used and any
//! character no font could draw. Nothing is bundled: the CJK files alone are 4-8 MB each.
//!
//! # Determinism
//!
//! Compositing is source-over on premultiplied sRGB bytes in integer maths ([`canvas`]); resampling and text use only
//! `+ - * /`, `sqrt` and `floor`. The same inputs give the same bytes. The title block's date comes from the shot clock
//! ([`SheetDate::from_clock`]); nothing here reads the wall clock.

// Pixel maths: a page is at most 16384 px a side, so every i32/u32/usize/f32 conversion below is exact, and the f32 and
// f64 to integer ones are deliberate roundings of values clamped to the page or to a byte.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

mod canvas;
mod fallback;
mod geometry;
mod page;
mod proportions;
mod swatch;
mod text;
mod theme;
mod title;

pub use fallback::{
    CATALOG, CachedDownloader, DirSource, FaceUsed, Family, Fetcher, FontConfig, FontError, FontReport, FontSource,
    FontSpec, HanVariant, MissingGlyph, sha256_hex,
};
pub use geometry::{Anchor, Rect};
pub use page::{Board, Dpi, Orientation, Page, Paper};
pub use proportions::{Proportions, Ruler, RulerSpan};
pub use swatch::{Swatch, SwatchReport, SwatchRole, WcagGrade, contrast_ratio};
pub use text::{Align, TextMetrics, TextStyle, Weight};
pub use theme::Theme;
pub use title::{SheetDate, TitleBlock};

use image::RgbaImage;
use omuzaki_core::colour::Colour;

use canvas::{Canvas, resample_premultiplied};
use text::Fonts;

/// Why a sheet could not be laid out. Every variant says what to fix.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum LayoutError {
    /// The embedded font failed to parse: a build problem, never caused by a caller.
    #[error("the embedded sheet font could not be read: {0}")]
    Font(String),
    /// A page side the layout cannot make.
    #[error("a {width} x {height} px page is not usable: give sides from 1 to 16384 px")]
    Page {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// A page resolution outside what type and spacing can be laid out at.
    #[error("page dpi {dpi} is not usable: give a number from 72 to 1200")]
    Dpi {
        /// Requested resolution.
        dpi: f32,
    },
    /// The body does not report a joint the proportions need.
    #[error("the body has no '{joint}' joint, so the proportions cannot be measured")]
    MissingJoint {
        /// The VRM bone name.
        joint: &'static str,
    },
    /// Numbers that cannot describe a figure (zero height, non-finite, upside down).
    #[error("the proportions cannot be measured: {reason}")]
    Proportions {
        /// What is wrong.
        reason: String,
    },
}

/// Pixel sizes derived from the page: the spacing every block shares, so margins and gaps are consistent.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Metrics {
    /// The blank border around the sheet.
    pub margin: i32,
    /// The space between neighbouring blocks and cells.
    pub gap: i32,
    /// The thickness of a hairline rule.
    pub hairline: i32,
    /// The corner radius of tiles and chips.
    pub radius: f32,
    /// The width the ruler numerals need left of a turnaround.
    pub ruler_gutter: i32,
    px_per_pt: f32,
}

/// The type scale of a sheet. Every size is nominal points on A4; see [`TextStyle`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Styles {
    /// The character's name.
    pub title: TextStyle,
    /// Small capitals above a title and over a metadata value.
    pub kicker: TextStyle,
    /// A section heading.
    pub heading: TextStyle,
    /// A view label under a figure (`FRONT`).
    pub label: TextStyle,
    /// Body text and captions.
    pub caption: TextStyle,
    /// Numbers that line up: values, hex codes, ratios.
    pub value: TextStyle,
    /// Secondary figures: ruler numerals, units.
    pub small: TextStyle,
}

/// What goes in one grid cell: a picture, a caption and a tile behind them.
#[derive(Clone, Debug, Default)]
pub struct Cell {
    image: Option<(RgbaImage, bool)>,
    caption: Option<(String, bool)>,
    panel: bool,
    anchor: Anchor,
}

/// Where a [`Cell`] put its parts, in page pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CellGeometry {
    /// The picture, scaled to fit and positioned.
    pub image: Rect,
    /// The row the caption's baseline sits on, when there is a caption.
    pub caption_baseline: Option<i32>,
}

/// The areas [`SheetLayout::section_heading`] hands back.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    /// The row to right-align the aside in: the heading's own row when the aside fits beside the title, else a
    /// second row under it.
    pub aside: Rect,
    /// The baseline the aside's text sits on.
    pub aside_baseline: i32,
    /// What is left of the area below the heading (and its aside row, if it took one).
    pub body: Rect,
}

/// A character sheet being laid out.
pub struct SheetLayout {
    page: Page,
    theme: Theme,
    metrics: Metrics,
    canvas: Canvas,
    fonts: Fonts,
    content: Rect,
}

impl Cell {
    /// A cell showing `image`, which is straight-alpha RGBA (a PNG, an `image` buffer).
    pub fn image(image: RgbaImage) -> Self {
        Self { image: Some((image, false)), ..Self::default() }
    }

    /// A cell showing `image`, whose colour is already multiplied by alpha (renderer output, spec BR-01).
    pub fn premultiplied(image: RgbaImage) -> Self {
        Self { image: Some((image, true)), ..Self::default() }
    }

    /// An empty cell: only its tile and caption.
    pub fn blank() -> Self {
        Self::default()
    }

    /// Captions the cell with a small-capitals view label (`FRONT`).
    #[must_use]
    pub fn label(mut self, text: impl Into<String>) -> Self {
        self.caption = Some((text.into(), true));
        self
    }

    /// Captions the cell with plain text (an expression's name).
    #[must_use]
    pub fn caption(mut self, text: impl Into<String>) -> Self {
        self.caption = Some((text.into(), false));
        self
    }

    /// Puts a quiet rounded tile behind the cell.
    #[must_use]
    pub fn panel(mut self) -> Self {
        self.panel = true;
        self
    }

    /// Stands the picture on the bottom of its area instead of centring it: figures in a row share one ground line.
    #[must_use]
    pub fn on_ground(mut self) -> Self {
        self.anchor = Anchor::Bottom;
        self
    }
}

impl SheetLayout {
    /// A blank sheet of `page` in `theme`, with the margins applied.
    ///
    /// # Errors
    /// [`LayoutError::Font`] if the embedded font cannot be parsed (a build problem), [`LayoutError::Page`] or
    /// [`LayoutError::Dpi`] if the page size or resolution is not one the layout can make (see [`Page::custom`]).
    pub fn new(page: Page, theme: Theme) -> Result<Self, LayoutError> {
        Self::with_fonts(page, theme, FontConfig::latin_only())
    }

    /// Like [`new`](Self::new), with fallback fonts for the scripts the embedded Latin font lacks (CJK, Arabic,
    /// Devanagari, Thai, emoji): `fonts` says which Han forms to draw and where the fonts come from. They are loaded
    /// the first time a character needs them.
    ///
    /// # Errors
    /// The same as [`new`](Self::new). A fallback font that cannot be loaded is not an error here: its characters are
    /// listed by [`font_report`](Self::font_report).
    pub fn with_fonts(page: Page, theme: Theme, fonts: FontConfig) -> Result<Self, LayoutError> {
        let page = Page::custom(page.width, page.height, page.dpi)?;
        let scale = page.type_scale();
        let px_per_pt = page.px_per_pt() * scale;
        let px = |mm: f32| (mm * page.px_per_mm() * scale).round() as i32;
        let short = page.width.min(page.height) as f32;
        let metrics = Metrics {
            margin: (short * 0.055).round() as i32,
            gap: px(4.5),
            hairline: ((page.dpi / 150.0 * 0.8).round() as i32).max(1),
            radius: px(1.6) as f32,
            ruler_gutter: px(8.5),
            px_per_pt,
        };
        let content = Rect::new(0, 0, page.width as i32, page.height as i32).inset(metrics.margin, metrics.margin);
        Ok(Self {
            canvas: Canvas::new(page.width, page.height, theme.background),
            fonts: Fonts::load(fonts)?,
            page,
            theme,
            metrics,
            content,
        })
    }

    /// Which fallback fonts the sheet has loaded (with the SHA-256 of the bytes used) and which characters no font
    /// could draw. Record the faces in the sheet's provenance; warn on `missing` before export.
    pub fn font_report(&self) -> FontReport {
        self.fonts.report()
    }

    /// The page.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// The colours.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The spacing derived from the page.
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// The area inside the margins that no title block has taken yet.
    pub fn content(&self) -> Rect {
        self.content
    }

    /// The type scale in the theme's colours.
    pub fn styles(&self) -> Styles {
        let t = &self.theme;
        let semibold = |pt, colour| TextStyle::new(pt, Weight::SemiBold, colour);
        Styles {
            title: semibold(26.0, t.ink).tracked(-0.015),
            kicker: semibold(6.5, t.muted).tracked(0.14).caps(),
            heading: semibold(7.0, t.muted).tracked(0.14).caps(),
            label: semibold(7.5, t.ink).tracked(0.12).caps(),
            caption: TextStyle::new(9.0, Weight::Regular, t.ink),
            value: TextStyle::new(9.0, Weight::Regular, t.ink).tabular(),
            small: TextStyle::new(7.0, Weight::Regular, t.muted).tabular(),
        }
    }

    /// Pixels to the em for a style.
    pub fn em_px(&self, style: &TextStyle) -> f32 {
        style.size_pt * self.metrics.px_per_pt
    }

    /// The pixel measures of a style on this page: its cap height is the box labels align to.
    pub fn text_metrics(&self, style: &TextStyle) -> TextMetrics {
        self.fonts.metrics(style.weight, self.em_px(style))
    }

    /// The width `text` takes in `style`, in pixels.
    pub fn text_width(&self, text: &str, style: &TextStyle) -> f32 {
        self.fonts.width(text, style, self.em_px(style))
    }

    /// Draws `text` with the top of its capitals on row `cap_top` and its anchor at `x`. A `max_width` cuts it with an
    /// ellipsis rather than let it run into a neighbour. Returns the width drawn.
    pub fn text(
        &mut self,
        text: &str,
        style: &TextStyle,
        (x, cap_top): (f32, i32),
        align: Align,
        max_width: Option<f32>,
    ) -> f32 {
        let cap = self.text_metrics(style).cap_height.round() as i32;
        self.text_on_baseline(text, style, (x, cap_top + cap), align, max_width)
    }

    /// Like [`text`](Self::text), placed by baseline: for text of different sizes that must share a line.
    pub fn text_on_baseline(
        &mut self,
        text: &str,
        style: &TextStyle,
        (x, baseline): (f32, i32),
        align: Align,
        max_width: Option<f32>,
    ) -> f32 {
        let em = self.em_px(style);
        let shown = match max_width {
            Some(max) => self.fonts.fit(text, style, em, max),
            None => text.to_owned(),
        };
        self.fonts.draw(&mut self.canvas, &shown, style, em, (x, baseline), align)
    }

    /// A horizontal hairline in `colour` from column `x0` to `x1` on row `y` (the row of its top edge).
    pub fn hline(&mut self, (x0, x1): (i32, i32), y: i32, colour: Colour) {
        self.canvas.fill_rect(Rect::new(x0, y, x1 - x0, self.metrics.hairline), colour);
    }

    /// The height of a [`section_heading`](Self::section_heading) row, for sizing the area that holds it.
    pub fn section_heading_height(&self) -> i32 {
        let value = self.styles().value;
        self.text_metrics(&value).cap_height.round() as i32 + (self.em_px(&value) * 1.35).round() as i32
    }

    /// Titles `area` with a small-capitals heading and a hairline running from it to the right edge.
    ///
    /// `aside_width` is the width of something that belongs on the heading's right (the proportions readout). When
    /// it fits beside the title the rule stops short of it and [`Heading::aside`] is the heading's own row; when it
    /// does not, the aside gets a second row under the heading, so a narrow column never makes the two collide.
    pub fn section_heading(&mut self, area: Rect, title: &str, aside_width: i32) -> Heading {
        let styles = self.styles();
        let cap = self.text_metrics(&styles.value).cap_height.round() as i32;
        let row_h = self.section_heading_height();
        let (row, below) = area.take_top(row_h);
        let baseline = row.y + cap;
        let width = self.text_on_baseline(title, &styles.heading, (row.x as f32, baseline), Align::Left, None);
        let rule_from = row.x + width.round() as i32 + self.metrics.gap / 2;
        let fits_beside = aside_width > 0 && rule_from + self.metrics.gap + aside_width <= row.right();
        let (aside, aside_baseline, body) = if aside_width == 0 || fits_beside {
            (row, baseline, below)
        } else {
            let (second, rest) = below.take_top(row_h);
            (second, second.y + cap, rest)
        };
        let rule_to = if fits_beside { row.right() - aside_width - self.metrics.gap / 2 } else { row.right() };
        let heading_cap = self.text_metrics(&styles.heading).cap_height.round() as i32;
        let hairline = self.theme.hairline;
        self.hline((rule_from, rule_to), baseline - heading_cap / 2, hairline);
        Heading { aside, aside_baseline, body }
    }

    /// Where `cell` would put its picture and caption inside `rect`, without drawing: the turnaround sheet uses it to
    /// find the figure's top and soles before it draws the rulers behind the art.
    pub fn cell_geometry(&self, rect: Rect, cell: &Cell) -> CellGeometry {
        let pad = if cell.panel { self.metrics.gap / 2 } else { 0 };
        let inner = rect.inset(pad, pad);
        let (picture_area, caption_baseline) = match &cell.caption {
            Some((_, label)) => {
                let style = if *label { self.styles().label } else { self.styles().caption };
                let (cap, em) = (self.text_metrics(&style).cap_height, self.em_px(&style));
                let band = (cap + em * 1.1).round() as i32;
                let (above, band_area) = inner.take_bottom(band);
                (above, Some(band_area.y + (em * 0.55).round() as i32 + cap.round() as i32))
            }
            None => (inner, None),
        };
        let image = match &cell.image {
            Some((img, _)) => picture_area.contain(img.width(), img.height(), cell.anchor),
            None => picture_area,
        };
        CellGeometry { image, caption_baseline }
    }

    /// Draws `cell` in `rect`: its tile, its picture scaled to fit, its caption centred underneath.
    pub fn place(&mut self, rect: Rect, cell: &Cell) -> CellGeometry {
        let geometry = self.cell_geometry(rect, cell);
        if cell.panel {
            let panel = self.theme.panel;
            self.canvas.fill_round_rect(rect, self.metrics.radius, panel);
        }
        if let Some((img, premultiplied)) = &cell.image {
            let g = geometry.image;
            if !g.is_empty() {
                let pixels = resample_premultiplied(img, *premultiplied, g.w as u32, g.h as u32);
                self.canvas.draw_premultiplied(g.x, g.y, g.w as u32, &pixels);
            }
        }
        if let (Some((text, label)), Some(baseline)) = (&cell.caption, geometry.caption_baseline) {
            let style = if *label { self.styles().label } else { self.styles().caption };
            let max = (rect.w - self.metrics.gap / 4) as f32;
            self.text_on_baseline(text, &style, (rect.centre_x(), baseline), Align::Centre, Some(max));
        }
        geometry
    }

    /// The finished sheet as straight-alpha RGBA (opaque unless the theme's background is transparent).
    pub fn finish(self) -> RgbaImage {
        self.canvas.into_rgba()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> SheetLayout {
        SheetLayout::new(Page::paper(Paper::A4, Orientation::Landscape, Dpi::D150), Theme::light()).unwrap()
    }

    fn title() -> TitleBlock {
        TitleBlock {
            kicker: "Character sheet".into(),
            name: "A Name".into(),
            template: "t".into(),
            variant: "v".into(),
            build: "abc1234".into(),
            date: Some(SheetDate { year: 2026, month: 10, day: 12 }),
        }
    }

    #[test]
    fn the_content_area_sits_inside_equal_margins() {
        let s = sheet();
        let (page, m) = (s.page(), s.metrics());
        let content = s.content();
        assert_eq!((content.x, content.y), (m.margin, m.margin));
        assert_eq!(content.right(), page.width as i32 - m.margin);
        assert_eq!(content.bottom(), page.height as i32 - m.margin);
    }

    #[test]
    fn the_title_block_takes_the_top_and_leaves_the_rest() {
        let mut s = sheet();
        let before = s.content();
        let below = s.title_block(&title());
        assert_eq!(s.content(), below);
        assert!(below.y > before.y + 80 && below.bottom() == before.bottom(), "{before:?} -> {below:?}");
        assert_eq!((below.x, below.w), (before.x, before.w));
    }

    #[test]
    fn an_aside_that_does_not_fit_beside_the_heading_gets_its_own_row() {
        let mut s = sheet();
        let wide = Rect::new(100, 100, 1000, 500);
        let beside = s.section_heading(wide, "Turnaround", 400);
        assert_eq!(beside.aside.y, 100, "fits: shares the title row");
        let narrow = Rect::new(100, 100, 300, 500);
        let below = s.section_heading(narrow, "Turnaround", 400);
        assert!(below.aside.y > 100 && below.body.y > below.aside.y, "{below:?}");
    }

    #[test]
    fn a_cell_stays_inside_its_rect_and_puts_the_caption_under_the_picture() {
        let s = sheet();
        let rect = Rect::new(200, 300, 240, 400);
        let cell = Cell::image(RgbaImage::new(100, 300)).caption("Happy").panel();
        let g = s.cell_geometry(rect, &cell);
        assert!(g.image.x >= rect.x && g.image.right() <= rect.right() && g.image.y >= rect.y);
        assert!(g.caption_baseline.unwrap() > g.image.bottom() && g.caption_baseline.unwrap() < rect.bottom());
        assert!(g.image.w * 3 - g.image.h <= 3, "the 1:3 aspect is kept: {:?}", g.image);
    }

    #[test]
    fn nothing_is_drawn_outside_the_cell() {
        let mut s = sheet();
        let rect = Rect::new(300, 300, 200, 300);
        let background = s.theme().background;
        s.place(
            rect,
            &Cell::image(RgbaImage::from_pixel(50, 80, image::Rgba([10, 20, 30, 255]))).label("Front").panel(),
        );
        let img = s.finish();
        for (x, y, p) in img.enumerate_pixels() {
            let inside = (rect.x..rect.right()).contains(&(x as i32)) && (rect.y..rect.bottom()).contains(&(y as i32));
            if !inside {
                assert_eq!(p.0, [background.r, background.g, background.b, 255], "stray pixel at {x},{y}");
            }
        }
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis_inside_its_width() {
        let mut s = sheet();
        let style = s.styles().caption;
        let limit = 200.0_f32;
        let drawn =
            s.text("A very long character name that cannot fit", &style, (100.0, 100), Align::Left, Some(limit));
        assert!(drawn <= limit, "{drawn}");
        let img = s.finish();
        let rightmost =
            img.enumerate_pixels().filter(|(_, y, p)| (100..140).contains(y) && p.0[0] < 150).map(|(x, ..)| x).max();
        assert!(rightmost.unwrap() as f32 <= 100.0 + limit + 1.0, "{rightmost:?}");
    }
}
