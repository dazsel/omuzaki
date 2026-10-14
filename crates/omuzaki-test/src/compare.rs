//! Tolerant image comparison: the core of every golden-render test.
//!
//! Two images match when few enough pixels differ by more than a per-channel threshold. Splitting the tolerance in two
//! lets a test forgive both kinds of harmless noise: tiny rounding differences everywhere (`per_channel`) and a handful
//! of edge pixels that land differently between GPU drivers (`max_bad_fraction`), while a real change still fails.

use std::fmt;

use image::RgbaImage;
use serde::{Deserialize, Serialize};

/// How far two images may drift and still count as the same picture.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Tolerance {
    /// A pixel is *bad* when any of its premultiplied R, G, B or its A value differs by more than this (0 to 255).
    /// See [`compare`] for why colour is compared premultiplied.
    pub per_channel: u8,
    /// The images match when at most this fraction of the pixels is bad (0.0 to 1.0).
    pub max_bad_fraction: f64,
}

impl Tolerance {
    /// No drift allowed: every pixel must be identical.
    pub const EXACT: Self = Self { per_channel: 0, max_bad_fraction: 0.0 };

    /// Checks that `max_bad_fraction` is a number from 0.0 to 1.0.
    ///
    /// [`compare`] does this itself; callers that may not reach `compare` (a store check with no golden yet) call it
    /// first so a nonsense tolerance is never silently accepted.
    ///
    /// # Errors
    /// [`CompareError::InvalidTolerance`] when `max_bad_fraction` is outside 0.0..=1.0 or not a number.
    pub fn validate(self) -> Result<(), CompareError> {
        if (0.0..=1.0).contains(&self.max_bad_fraction) {
            Ok(())
        } else {
            // Also catches NaN, which is in no range.
            Err(CompareError::InvalidTolerance { max_bad_fraction: self.max_bad_fraction })
        }
    }
}

impl Default for Tolerance {
    fn default() -> Self {
        Self::EXACT
    }
}

/// The numbers from one comparison. Small and serialisable, so it can be stored next to the images it describes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// How many pixels differ by more than the tolerance's `per_channel`.
    pub bad_pixels: u64,
    /// `bad_pixels` divided by the pixel count (0.0 for an empty image).
    pub bad_fraction: f64,
    /// The largest single-channel difference anywhere in the image (0 to 255), bad pixel or not.
    pub max_delta: u8,
    /// Peak signal-to-noise ratio in decibels over all four channels. `None` means the images are identical, where
    /// the ratio is infinite (JSON has no infinity).
    pub psnr_db: Option<f64>,
    /// Whether `bad_fraction` is within the tolerance's `max_bad_fraction`.
    pub passed: bool,
}

impl fmt::Display for Stats {
    /// One line for logs and PR comments, with fixed rounding so repeated runs print identical text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:.3}% bad ({} of {} px), max delta {}, ",
            self.bad_fraction * 100.0,
            self.bad_pixels,
            u64::from(self.width) * u64::from(self.height),
            self.max_delta,
        )?;
        match self.psnr_db {
            Some(db) => write!(f, "PSNR {db:.2} dB"),
            None => write!(f, "identical"),
        }
    }
}

/// The result of [`compare`]: the numbers plus a picture of where the images differ.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The numbers.
    pub stats: Stats,
    /// Bad pixels in solid red over a faded grayscale copy of the expected image, so a reviewer sees at a glance
    /// where the change is.
    pub diff: RgbaImage,
}

/// Why two images could not be compared at all.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CompareError {
    /// The images have different dimensions, so there is no pixel-to-pixel comparison.
    #[error("size mismatch: actual is {}x{}, expected is {}x{}", actual.0, actual.1, expected.0, expected.1)]
    SizeMismatch {
        /// Width and height of the image under test.
        actual: (u32, u32),
        /// Width and height of the golden.
        expected: (u32, u32),
    },
    /// `max_bad_fraction` is not a number from 0.0 to 1.0.
    #[error("max_bad_fraction must be between 0.0 and 1.0 (got {max_bad_fraction})")]
    InvalidTolerance {
        /// The rejected value.
        max_bad_fraction: f64,
    },
}

const RED: [u8; 4] = [255, 0, 0, 255];

/// Compares `actual` with `expected` under `tolerance`.
///
/// Colour is compared **premultiplied**: each of R, G, B is multiplied by alpha (rounded) before the per-channel
/// difference is taken, and alpha is compared as it is. Transparent output is premultiplied (spec BR-01), and the
/// colour hidden under a low alpha is invisible, so renderers are free to leave anything in it: two fully transparent
/// pixels always match, and a colour difference counts in proportion to how visible the pixel is.
///
/// The result depends only on the pixels, never on timing, threads or machine state, so repeated runs give identical
/// reports and diff images.
///
/// # Errors
/// [`CompareError::SizeMismatch`] when the dimensions differ; [`CompareError::InvalidTolerance`] when
/// `max_bad_fraction` is outside 0.0..=1.0.
pub fn compare(actual: &RgbaImage, expected: &RgbaImage, tolerance: Tolerance) -> Result<Report, CompareError> {
    tolerance.validate()?;
    if actual.dimensions() != expected.dimensions() {
        return Err(CompareError::SizeMismatch { actual: actual.dimensions(), expected: expected.dimensions() });
    }
    let (width, height) = actual.dimensions();
    let mut diff = RgbaImage::new(width, height);
    let mut bad_pixels = 0_u64;
    let mut max_delta = 0_u8;
    let mut sum_squares = 0_u64;

    let pixels = actual.as_raw().as_chunks::<4>().0.iter().zip(expected.as_raw().as_chunks::<4>().0);
    for ((a, e), out) in pixels.zip(diff.as_chunks_mut::<4>().0) {
        let deltas = channel_deltas(*a, *e);
        let worst = deltas.iter().fold(0, |m, &d| m.max(d));
        max_delta = max_delta.max(worst);
        sum_squares += deltas.iter().map(|&d| u64::from(d) * u64::from(d)).sum::<u64>();
        if worst > tolerance.per_channel {
            bad_pixels += 1;
            *out = RED;
        } else {
            let gray = faded_luma(*e);
            *out = [gray, gray, gray, 255];
        }
    }

    let total = u64::from(width) * u64::from(height);
    let bad_fraction = ratio(bad_pixels, total);
    let stats = Stats {
        width,
        height,
        bad_pixels,
        bad_fraction,
        max_delta,
        psnr_db: psnr_db(sum_squares, total),
        passed: bad_fraction <= tolerance.max_bad_fraction,
    };
    Ok(Report { stats, diff })
}

/// Per-channel absolute differences of two RGBA pixels (straight alpha in, compared premultiplied).
fn channel_deltas(a: [u8; 4], e: [u8; 4]) -> [u8; 4] {
    let (a, e) = (premultiply(a), premultiply(e));
    [a[0].abs_diff(e[0]), a[1].abs_diff(e[1]), a[2].abs_diff(e[2]), a[3].abs_diff(e[3])]
}

/// Multiplies R, G, B by alpha (rounded to nearest), leaving alpha as it is. Integer maths keeps it deterministic.
fn premultiply([r, g, b, a]: [u8; 4]) -> [u8; 4] {
    let scale = |c: u8| u8::try_from((u32::from(c) * u32::from(a) + 127) / 255).unwrap_or(u8::MAX);
    [scale(r), scale(g), scale(b), a]
}

/// A washed-out gray (170..=255) of an RGBA pixel over white: pale enough that red marks stand out.
/// Integer maths only, so the diff image is bit-identical on every machine.
fn faded_luma(pixel: [u8; 4]) -> u8 {
    let [r, g, b, a] = pixel.map(u32::from);
    let luma = (299 * r + 587 * g + 114 * b) / 1000;
    let over_white = (luma * a + 255 * (255 - a)) / 255;
    u8::try_from((over_white + 2 * 255) / 3).unwrap_or(u8::MAX)
}

#[allow(clippy::cast_precision_loss)] // pixel counts stay far below 2^52, where f64 is still exact
fn ratio(part: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { part as f64 / total as f64 }
}

#[allow(clippy::cast_precision_loss)] // see `ratio`; the squared-error sum is likewise well within f64's exact range
fn psnr_db(sum_squares: u64, pixels: u64) -> Option<f64> {
    if sum_squares == 0 {
        return None;
    }
    let mean_square_error = sum_squares as f64 / (pixels * 4) as f64;
    Some(10.0 * (255.0_f64 * 255.0 / mean_square_error).log10())
}

#[cfg(test)]
mod tests {
    use image::Rgba;

    use super::*;

    const SIZE: u32 = 64;
    const PIXELS: u64 = 64 * 64;

    fn flat(color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(SIZE, SIZE, Rgba(color))
    }

    /// A smooth gradient, so tests exercise many different pixel values, not one flat colour.
    fn gradient() -> RgbaImage {
        let byte = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
        RgbaImage::from_fn(SIZE, SIZE, |x, y| Rgba([byte(x * 4), byte(y * 4), byte((x + y) * 2), 255]))
    }

    fn red_pixels(report: &Report) -> usize {
        report.diff.pixels().filter(|p| p.0 == RED).count()
    }

    #[test]
    fn identical_images_pass_with_no_bad_pixels_and_infinite_psnr() {
        let img = gradient();
        let report = compare(&img, &img, Tolerance::EXACT).unwrap();
        assert!(report.stats.passed);
        assert_eq!((report.stats.bad_pixels, report.stats.max_delta), (0, 0));
        assert_eq!(report.stats.psnr_db, None);
        assert_eq!(red_pixels(&report), 0);
    }

    #[test]
    fn one_changed_pixel_passes_under_tolerance_and_fails_exact() {
        let expected = gradient();
        let mut actual = expected.clone();
        actual.put_pixel(10, 20, Rgba([255, 255, 255, 255]));
        let lenient = Tolerance { per_channel: 2, max_bad_fraction: 0.001 };

        let report = compare(&actual, &expected, lenient).unwrap();
        assert!(report.stats.passed, "{}", report.stats);
        assert_eq!(report.stats.bad_pixels, 1);
        assert_eq!(red_pixels(&report), 1);
        assert_eq!(report.diff.get_pixel(10, 20).0, RED);

        assert!(!compare(&actual, &expected, Tolerance::EXACT).unwrap().stats.passed);
    }

    #[test]
    fn five_percent_change_fails_with_a_red_diff() {
        let expected = gradient();
        let mut actual = expected.clone();
        let changed = PIXELS / 20 + 1; // just over 5%
        for i in 0..changed {
            let i = u32::try_from(i).unwrap();
            actual.put_pixel(i % SIZE, i / SIZE, Rgba([255; 4])); // differs by 3+ from every gradient pixel
        }
        let tolerance = Tolerance { per_channel: 2, max_bad_fraction: 0.01 };

        let report = compare(&actual, &expected, tolerance).unwrap();
        assert!(!report.stats.passed);
        assert!(report.stats.bad_fraction > 0.05);
        assert_eq!(report.stats.bad_pixels, changed);
        assert_eq!(red_pixels(&report) as u64, changed);
    }

    #[test]
    fn per_channel_threshold_is_inclusive() {
        let tolerance = Tolerance { per_channel: 5, max_bad_fraction: 0.0 };
        let at_limit = compare(&flat([105, 0, 0, 255]), &flat([100, 0, 0, 255]), tolerance).unwrap();
        assert!(at_limit.stats.passed);
        assert_eq!((at_limit.stats.bad_pixels, at_limit.stats.max_delta), (0, 5));

        let over = compare(&flat([106, 0, 0, 255]), &flat([100, 0, 0, 255]), tolerance).unwrap();
        assert!(!over.stats.passed);
        assert_eq!(over.stats.bad_pixels, PIXELS);
    }

    #[test]
    fn a_difference_in_any_channel_counts_including_alpha() {
        let base = flat([60, 70, 80, 255]);
        for channel in 0..4 {
            let mut other = base.clone();
            other.get_pixel_mut(0, 0).0[channel] -= 50; // lowering alpha also dims the colour, by less than 50
            let stats = compare(&other, &base, Tolerance::EXACT).unwrap().stats;
            assert_eq!((stats.bad_pixels, stats.max_delta), (1, 50), "channel {channel}");
        }
    }

    #[test]
    fn fully_transparent_pixels_ignore_their_colour() {
        let stats = compare(&flat([255, 0, 0, 0]), &flat([0, 0, 255, 0]), Tolerance::EXACT).unwrap().stats;
        assert!(stats.passed && stats.bad_pixels == 0);
        // ...but transparent against visible is a real difference.
        assert!(!compare(&flat([0, 0, 0, 0]), &flat([0, 0, 0, 1]), Tolerance::EXACT).unwrap().stats.passed);
    }

    #[test]
    fn colour_is_compared_premultiplied() {
        // A colour change of 200 under alpha 10 shows as (200 * 10 + 127) / 255 = 8 premultiplied...
        let faint = compare(&flat([200, 0, 0, 10]), &flat([0, 0, 0, 10]), Tolerance::EXACT).unwrap().stats;
        assert_eq!((faint.bad_pixels, faint.max_delta), (PIXELS, 8));
        // ...so a tolerance of 8 forgives what is barely visible, while the same change at full alpha is a full 200.
        let tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.0 };
        assert!(compare(&flat([200, 0, 0, 10]), &flat([0, 0, 0, 10]), tolerance).unwrap().stats.passed);
        let opaque = compare(&flat([200, 0, 0, 255]), &flat([0, 0, 0, 255]), tolerance).unwrap().stats;
        assert_eq!((opaque.passed, opaque.max_delta), (false, 200));
    }

    #[test]
    fn psnr_matches_the_formula() {
        // Every channel off by one: mean square error 1, so PSNR = 10 * log10(255^2) = 48.13 dB.
        let stats = compare(&flat([1, 1, 1, 255]), &flat([0, 0, 0, 254]), Tolerance::EXACT).unwrap().stats;
        let db = stats.psnr_db.unwrap();
        assert!((db - 48.1308).abs() < 1e-3, "{db}");
    }

    #[test]
    fn diff_shows_good_pixels_as_pale_gray() {
        let report = compare(&flat([0, 0, 0, 255]), &flat([0, 0, 0, 255]), Tolerance::EXACT).unwrap();
        assert_eq!(report.diff.get_pixel(0, 0).0, [170, 170, 170, 255]); // black fades to 170, not to black
        let white = compare(&flat([255; 4]), &flat([255; 4]), Tolerance::EXACT).unwrap();
        assert_eq!(white.diff.get_pixel(0, 0).0, [255, 255, 255, 255]);
        let clear = compare(&flat([0; 4]), &flat([0; 4]), Tolerance::EXACT).unwrap();
        assert_eq!(clear.diff.get_pixel(0, 0).0, [255, 255, 255, 255]); // transparent reads as the white page
    }

    #[test]
    fn size_mismatch_is_an_error_not_a_failed_report() {
        let err = compare(&RgbaImage::new(2, 3), &RgbaImage::new(3, 2), Tolerance::EXACT).unwrap_err();
        assert_eq!(err, CompareError::SizeMismatch { actual: (2, 3), expected: (3, 2) });
    }

    #[test]
    fn nonsense_tolerance_is_rejected() {
        let img = flat([0; 4]);
        for bad in [-0.1, 1.5, f64::NAN, f64::INFINITY] {
            let result = compare(&img, &img, Tolerance { per_channel: 0, max_bad_fraction: bad });
            assert!(matches!(result, Err(CompareError::InvalidTolerance { .. })), "{bad}");
        }
    }

    #[test]
    fn repeated_comparisons_are_identical() {
        let expected = gradient();
        let mut actual = expected.clone();
        actual.put_pixel(3, 3, Rgba([9, 9, 9, 9]));
        let tolerance = Tolerance { per_channel: 1, max_bad_fraction: 0.5 };
        let first = compare(&actual, &expected, tolerance).unwrap();
        for _ in 0..3 {
            assert_eq!(compare(&actual, &expected, tolerance).unwrap(), first);
        }
    }

    #[test]
    fn stats_display_is_one_stable_line() {
        let stats = compare(&flat([1, 0, 0, 255]), &flat([0, 0, 0, 255]), Tolerance::EXACT).unwrap().stats;
        assert_eq!(stats.to_string(), "100.000% bad (4096 of 4096 px), max delta 1, PSNR 54.15 dB");
        assert_eq!(
            compare(&flat([0; 4]), &flat([0; 4]), Tolerance::EXACT).unwrap().stats.to_string(),
            "0.000% bad (0 of 4096 px), max delta 0, identical"
        );
    }
}
