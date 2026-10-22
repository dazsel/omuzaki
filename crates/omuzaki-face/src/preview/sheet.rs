//! Contact sheets: pictures in a grid, each with its label under it, as one PNG for a person or an agent to look at.
//!
//! The labels are drawn with Inter (SIL OFL 1.1), the Latin subset the character sheets already embed: the file and its
//! notices live in `crates/omuzaki-stage/assets/fonts/` and this module compiles the same file in, so the repository
//! holds one copy.

use std::io::Cursor;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use image::{ImageFormat, Rgba, RgbaImage, imageops};

/// Inter Regular, Latin subset (see the module docs for where it comes from and its licence).
static FONT: &[u8] = include_bytes!("../../../omuzaki-stage/assets/fonts/Inter-Regular.subset.ttf");

/// The sheet's ground and the label colour.
const GROUND: Rgba<u8> = Rgba([40, 40, 46, 255]);
const INK: Rgba<u8> = Rgba([236, 236, 240, 255]);
/// A flagged cell's label colour (a dead control, a look-alike pair).
const FLAG: Rgba<u8> = Rgba([255, 120, 100, 255]);
/// Space round each cell, and the label's height, in pixels.
const GAP: u32 = 6;
const LABEL_PX: f32 = 13.0;

/// One cell: its picture, its label, and whether the label is drawn as a warning.
pub struct Cell {
    /// The picture; cells of one sheet may differ in size (each is centred in the widest and tallest).
    pub picture: RgbaImage,
    /// What the cell shows.
    pub label: String,
    /// Draw the label in the warning colour.
    pub flagged: bool,
}

/// Lays `cells` out `columns` wide and draws the labels. An empty list gives a sheet of one gap.
pub fn compose(cells: &[Cell], columns: u32) -> RgbaImage {
    let columns = columns.max(1);
    let cell_w = cells.iter().map(|c| c.picture.width()).max().unwrap_or(0);
    let cell_h = cells.iter().map(|c| c.picture.height()).max().unwrap_or(0);
    // The label row is as tall as the font's line.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small positive number of pixels
    let label_h = (LABEL_PX * 1.4).ceil() as u32;
    let count = u32::try_from(cells.len()).unwrap_or(u32::MAX);
    let rows = count.div_ceil(columns);
    let (pitch_x, pitch_y) = (cell_w + GAP, cell_h + label_h + GAP);
    let mut sheet = RgbaImage::from_pixel(GAP + columns.min(count.max(1)) * pitch_x, GAP + rows * pitch_y, GROUND);
    let font = FontRef::try_from_slice(FONT).ok();
    for (i, cell) in (0u32..).zip(cells) {
        let (x0, y0) = (GAP + (i % columns) * pitch_x, GAP + (i / columns) * pitch_y);
        let (dx, dy) = ((cell_w - cell.picture.width()) / 2, (cell_h - cell.picture.height()) / 2);
        imageops::replace(&mut sheet, &cell.picture, i64::from(x0 + dx), i64::from(y0 + dy));
        if let Some(font) = &font {
            let colour = if cell.flagged { FLAG } else { INK };
            label(&mut sheet, font, &cell.label, x0, y0 + cell_h + 2, cell_w, colour);
        }
    }
    sheet
}

/// Encodes a sheet as PNG bytes.
///
/// # Errors
/// The encoder's message, which for an in-memory RGBA image does not happen in practice.
pub fn png(sheet: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    sheet.write_to(&mut out, ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// Draws `text` from `(x, y)` (top left), clipped to `max_w` pixels: a long name is cut, never wrapped over the cell below.
fn label(sheet: &mut RgbaImage, font: &FontRef<'_>, text: &str, x: u32, y: u32, max_w: u32, colour: Rgba<u8>) {
    let scaled = font.as_scaled(PxScale::from(LABEL_PX));
    #[allow(clippy::cast_precision_loss)] // sheet coordinates are far below 2^24
    let (left, right, top) = (x as f32, (x + max_w) as f32, y as f32 + scaled.ascent());
    let mut caret = left;
    let mut previous = None;
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if let Some(prev) = previous {
            caret += scaled.kern(prev, id);
        }
        let advance = scaled.h_advance(id);
        if caret + advance > right {
            break;
        }
        let glyph = id.with_scale_and_position(PxScale::from(LABEL_PX), point(caret, top));
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked against the sheet below
                let (px, py) = ((bounds.min.x as i64 + i64::from(gx)), (bounds.min.y as i64 + i64::from(gy)));
                let (Ok(px), Ok(py)) = (u32::try_from(px), u32::try_from(py)) else { return };
                if px >= sheet.width() || py >= sheet.height() {
                    return;
                }
                let dst = sheet.get_pixel_mut(px, py);
                let a = coverage.clamp(0.0, 1.0);
                for (d, s) in dst.0[..3].iter_mut().zip(colour.0) {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a blend of two bytes
                    let mixed = (f32::from(s) * a + f32::from(*d) * (1.0 - a)).round() as u8;
                    *d = mixed;
                }
            });
        }
        caret += advance;
        previous = Some(id);
    }
}
