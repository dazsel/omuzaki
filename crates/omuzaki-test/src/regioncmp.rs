//! Image comparison inside a pixel rectangle, for judging one part of a render at a time (the face, an eye, the hair).
//!
//! A whole-frame [`compare`] hides a wrong eye behind a thousand right pixels of background. Here each region of
//! interest is compared by itself against its own tolerance, and a table and a picture say which regions are wrong.
//!
//! The numbers are exactly [`compare`]'s, taken on the cropped region, so a region covering the whole image reproduces
//! `compare()` on that image: colour compared premultiplied, alpha as a channel, transparent pixels matching whatever
//! colour hides under them. Only the pictures composite over mid-grey, because a viewer shows transparency as black or
//! white and either hides half the faint pixels.

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};

use crate::compare::{CompareError, Stats, Tolerance, compare};

/// The wave-3 parity rule for face and close-up renders (starting values; the first parity PR measures the noise
/// floor): per channel 12, at most 1 % bad pixels. The third part, PSNR, is [`T_FACE_MIN_PSNR_DB`].
pub const T_FACE: Tolerance = Tolerance { per_channel: 12, max_bad_fraction: 0.01 };

/// The PSNR a region must reach under the T-FACE rule, in decibels.
pub const T_FACE_MIN_PSNR_DB: f64 = 34.0;

/// The grey that pictures composite over: not black, not white, so both dark and light pixels show.
pub const MID_GREY: u8 = 128;

/// Width of the dark band between panels of a side-by-side picture, in output pixels.
const GAP: u32 = 4;

/// A rectangle of pixels; `x` and `y` are the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Rect {
    /// The rectangle covering a whole `width` by `height` image.
    pub const fn full(width: u32, height: u32) -> Self {
        Self { x: 0, y: 0, width, height }
    }

    fn fits(self, (width, height): (u32, u32)) -> bool {
        self.x.checked_add(self.width).is_some_and(|right| right <= width)
            && self.y.checked_add(self.height).is_some_and(|bottom| bottom <= height)
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

/// Why a region could not be compared at all.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RegionError {
    /// The rectangle has no pixels, so any tolerance would pass vacuously.
    #[error("region {0} is empty")]
    Empty(Rect),
    /// The rectangle reaches outside the image.
    #[error("region {rect} does not fit a {}x{} image", size.0, size.1)]
    OutOfBounds {
        /// The rejected rectangle.
        rect: Rect,
        /// Width and height of the image.
        size: (u32, u32),
    },
    /// The images or the tolerance are unusable ([`compare`]'s errors).
    #[error(transparent)]
    Compare(#[from] CompareError),
    /// A picture could not be written.
    #[error("cannot write {}: {message}", path.display())]
    Write {
        /// The file that failed.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
}

/// The result of [`compare_region`].
#[derive(Debug, Clone, PartialEq)]
pub struct RegionReport {
    /// The region that was compared.
    pub rect: Rect,
    /// [`compare`]'s numbers for the region. `stats.passed` covers the bad-pixel fraction only.
    pub stats: Stats,
    /// [`compare`]'s diff picture for the region: bad pixels red over a pale copy of the expected region.
    pub diff: RgbaImage,
}

impl RegionReport {
    /// True when the region is within the bad-pixel tolerance **and** reaches `min_psnr_db` (identical regions, whose
    /// PSNR is infinite, always do). Use [`T_FACE_MIN_PSNR_DB`] for the face rule.
    pub fn passes(&self, min_psnr_db: f64) -> bool {
        self.stats.passed && self.stats.psnr_db.is_none_or(|db| db >= min_psnr_db)
    }
}

/// Compares the pixels of `rect` in `actual` and `expected` under `tolerance`.
///
/// # Errors
/// [`RegionError::Empty`] or [`RegionError::OutOfBounds`] for a bad rectangle; [`RegionError::Compare`] when the images
/// differ in size or the tolerance is invalid.
pub fn compare_region(
    actual: &RgbaImage,
    expected: &RgbaImage,
    rect: Rect,
    tolerance: Tolerance,
) -> Result<RegionReport, RegionError> {
    let (actual_crop, expected_crop) = crops(actual, expected, rect)?;
    let report = compare(&actual_crop, &expected_crop, tolerance)?;
    Ok(RegionReport { rect, stats: report.stats, diff: report.diff })
}

/// Both images cropped to `rect`, after checking that they are the same size and that `rect` fits.
fn crops(actual: &RgbaImage, expected: &RgbaImage, rect: Rect) -> Result<(RgbaImage, RgbaImage), RegionError> {
    if actual.dimensions() != expected.dimensions() {
        return Err(CompareError::SizeMismatch { actual: actual.dimensions(), expected: expected.dimensions() }.into());
    }
    if rect.width == 0 || rect.height == 0 {
        return Err(RegionError::Empty(rect));
    }
    if !rect.fits(actual.dimensions()) {
        return Err(RegionError::OutOfBounds { rect, size: actual.dimensions() });
    }
    let crop = |img| imageops::crop_imm(img, rect.x, rect.y, rect.width, rect.height).to_image();
    Ok((crop(actual), crop(expected)))
}

/// A markdown table with one row per named region: where it is, how many pixels are bad, the largest channel
/// difference, the PSNR, and whether it [`passes`](RegionReport::passes) at `min_psnr_db`.
pub fn markdown_table(rows: &[(&str, &RegionReport)], min_psnr_db: f64) -> String {
    let mut table =
        String::from("| region | rect | bad pixels | max delta | PSNR | result |\n|---|---|---:|---:|---:|---|\n");
    for (name, report) in rows {
        let stats = &report.stats;
        let psnr = stats.psnr_db.map_or_else(|| "inf".to_owned(), |db| format!("{db:.2} dB"));
        let result = if report.passes(min_psnr_db) { "pass" } else { "**FAIL**" };
        // Writing to a String cannot fail.
        let _ = writeln!(
            table,
            "| {} | {} | {:.3}% | {} | {psnr} | {result} |",
            name.replace('|', "\\|"),
            report.rect,
            stats.bad_fraction * 100.0,
            stats.max_delta,
        );
    }
    table
}

/// One picture of a region for a reviewer: the expected region, the actual region and the diff, left to right,
/// enlarged `scale` times (nearest neighbour, so pixels stay visible). The first two composite over [`MID_GREY`].
///
/// # Errors
/// The same bad-rectangle and size errors as [`compare_region`].
pub fn side_by_side(
    actual: &RgbaImage,
    expected: &RgbaImage,
    report: &RegionReport,
    scale: u32,
) -> Result<RgbaImage, RegionError> {
    let (actual_crop, expected_crop) = crops(actual, expected, report.rect)?;
    let scale = scale.max(1);
    let panels = [over_grey(&expected_crop), over_grey(&actual_crop), report.diff.clone()].map(|panel| {
        if scale == 1 {
            panel
        } else {
            imageops::resize(&panel, panel.width() * scale, panel.height() * scale, FilterType::Nearest)
        }
    });
    let (width, height) = panels[0].dimensions();
    let mut sheet = RgbaImage::from_pixel(3 * width + 2 * GAP, height, Rgba([32, 32, 32, 255]));
    for (i, panel) in (0..).zip(&panels) {
        imageops::replace(&mut sheet, panel, i64::from(i * (width + GAP)), 0);
    }
    Ok(sheet)
}

/// [`side_by_side`], saved as a PNG at `path` (parent folders are created). Meant for PR evidence in a local assets folder.
///
/// # Errors
/// [`side_by_side`]'s errors, or [`RegionError::Write`] when the file cannot be written.
pub fn write_side_by_side(
    path: &Path,
    actual: &RgbaImage,
    expected: &RgbaImage,
    report: &RegionReport,
    scale: u32,
) -> Result<(), RegionError> {
    let sheet = side_by_side(actual, expected, report, scale)?;
    let write = |message: String| RegionError::Write { path: path.to_path_buf(), message };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| write(e.to_string()))?;
    }
    sheet.save(path).map_err(|e| write(e.to_string()))
}

/// Straight-alpha RGBA composited over [`MID_GREY`], rounded to nearest, fully opaque. Integer maths only.
fn over_grey(img: &RgbaImage) -> RgbaImage {
    let mut out = img.clone();
    for pixel in out.pixels_mut() {
        let alpha = u32::from(pixel.0[3]);
        for channel in &mut pixel.0[..3] {
            let mixed = (u32::from(*channel) * alpha + u32::from(MID_GREY) * (255 - alpha) + 127) / 255;
            *channel = u8::try_from(mixed).unwrap_or(u8::MAX);
        }
        pixel.0[3] = 255;
    }
    out
}
