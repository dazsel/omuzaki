//! Page presets: the paper sizes and screen boards a character sheet is laid out on.

use super::LayoutError;

/// A4's short side in millimetres: the reference the type scale grows from.
const A4_SHORT_MM: f32 = 210.0;
const MM_PER_INCH: f32 = 25.4;
/// The largest side a page may have, so a typo cannot ask for a gigapixel canvas.
const MAX_SIDE_PX: u32 = 16_384;
/// The resolutions a page may have: from a screen (72 dpi, where one point is one pixel) to print proofing. Below the
/// range the type and spacing collapse to nothing; above it the canvas is absurd.
const DPI_RANGE: std::ops::RangeInclusive<f32> = 72.0..=1200.0;

/// A paper size.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Paper {
    /// 210 x 297 mm.
    A4,
    /// 297 x 420 mm.
    A3,
    /// 8.5 x 11 in.
    Letter,
}

/// Which way the long side of a page runs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Orientation {
    /// Wider than tall.
    Landscape,
    /// Taller than wide.
    Portrait,
}

/// Print resolution.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Dpi {
    /// 150 dpi: a sheet to share on screen.
    D150,
    /// 300 dpi: print quality.
    D300,
}

/// A 16:9 screen board, laid out like a 10 x 5.625 in slide so a point size means what it does on paper.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Board {
    /// 1920 x 1080 (192 dpi).
    Hd1080,
    /// 3840 x 2160 (384 dpi).
    Uhd4k,
}

/// The size of a sheet in pixels and the resolution that turns points and millimetres into pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Page {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixels per inch.
    pub dpi: f32,
}

impl Page {
    /// A paper page: A4, A3 or Letter, landscape or portrait, at 150 or 300 dpi.
    pub fn paper(paper: Paper, orientation: Orientation, dpi: Dpi) -> Self {
        let (short_mm, long_mm) = match paper {
            Paper::A4 => (210.0, 297.0),
            Paper::A3 => (297.0, 420.0),
            Paper::Letter => (8.5 * MM_PER_INCH, 11.0 * MM_PER_INCH),
        };
        let dpi = match dpi {
            Dpi::D150 => 150.0,
            Dpi::D300 => 300.0,
        };
        let px = |mm: f32| (mm / MM_PER_INCH * dpi).round() as u32;
        let (short, long) = (px(short_mm), px(long_mm));
        match orientation {
            Orientation::Landscape => Self { width: long, height: short, dpi },
            Orientation::Portrait => Self { width: short, height: long, dpi },
        }
    }

    /// A 16:9 board.
    pub fn board(board: Board) -> Self {
        match board {
            Board::Hd1080 => Self { width: 1920, height: 1080, dpi: 192.0 },
            Board::Uhd4k => Self { width: 3840, height: 2160, dpi: 384.0 },
        }
    }

    /// A page of any size.
    ///
    /// # Errors
    /// [`LayoutError::Page`] when a side is zero or above 16384 px; [`LayoutError::Dpi`] when `dpi` is not a number from
    /// 72 to 1200.
    pub fn custom(width: u32, height: u32, dpi: f32) -> Result<Self, LayoutError> {
        if !((1..=MAX_SIDE_PX).contains(&width) && (1..=MAX_SIDE_PX).contains(&height)) {
            return Err(LayoutError::Page { width, height });
        }
        if !DPI_RANGE.contains(&dpi) {
            return Err(LayoutError::Dpi { dpi });
        }
        Ok(Self { width, height, dpi })
    }

    /// Pixels in one typographic point.
    pub fn px_per_pt(&self) -> f32 {
        self.dpi / 72.0
    }

    /// Pixels in one millimetre.
    pub fn px_per_mm(&self) -> f32 {
        self.dpi / MM_PER_INCH
    }

    /// How much bigger than on A4 the sheet's type and spacing are drawn, so an A3 sheet reads like an enlarged A4
    /// one instead of a sparse page of tiny print. It never goes below 1, which keeps a board's 9 pt label at the same
    /// physical size on its 10 in width as on paper.
    pub fn type_scale(&self) -> f32 {
        let short_mm = self.width.min(self.height) as f32 / self.px_per_mm();
        (short_mm / A4_SHORT_MM).clamp(1.0, 1.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_sizes_match_the_standards() {
        let a4 = Page::paper(Paper::A4, Orientation::Landscape, Dpi::D150);
        assert_eq!((a4.width, a4.height), (1754, 1240));
        let a4_300 = Page::paper(Paper::A4, Orientation::Landscape, Dpi::D300);
        assert_eq!((a4_300.width, a4_300.height), (3508, 2480));
        let letter = Page::paper(Paper::Letter, Orientation::Portrait, Dpi::D150);
        assert_eq!((letter.width, letter.height), (1275, 1650));
        let a3 = Page::paper(Paper::A3, Orientation::Portrait, Dpi::D150);
        assert_eq!((a3.width, a3.height), (1754, 2480));
    }

    #[test]
    fn boards_are_16_by_9() {
        for board in [Board::Hd1080, Board::Uhd4k] {
            let p = Page::board(board);
            assert_eq!(p.width * 9, p.height * 16);
        }
    }

    #[test]
    fn type_scale_grows_with_the_paper_but_not_below_a4() {
        assert!((Page::paper(Paper::A4, Orientation::Portrait, Dpi::D150).type_scale() - 1.0).abs() < 1e-3);
        assert!(Page::paper(Paper::A3, Orientation::Landscape, Dpi::D150).type_scale() > 1.4);
        assert!((Page::board(Board::Hd1080).type_scale() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn custom_pages_are_validated() {
        assert!(Page::custom(100, 100, 96.0).is_ok());
        assert_eq!(Page::custom(0, 100, 96.0), Err(LayoutError::Page { width: 0, height: 100 }));
        assert!(matches!(Page::custom(100, 20_000, 96.0), Err(LayoutError::Page { .. })));
        assert_eq!(
            Page::custom(100, 100, f32::NAN).unwrap_err().to_string(),
            LayoutError::Dpi { dpi: f32::NAN }.to_string()
        );
    }

    #[test]
    fn the_dpi_must_be_a_usable_number() {
        for dpi in [0.0, 1.0, 2.8, 71.9, 1200.1, f32::INFINITY, f32::NEG_INFINITY, f32::NAN, -150.0] {
            assert!(matches!(Page::custom(100, 100, dpi), Err(LayoutError::Dpi { .. })), "dpi {dpi}");
        }
        assert!(Page::custom(100, 100, 72.0).is_ok() && Page::custom(100, 100, 1200.0).is_ok());
    }
}
