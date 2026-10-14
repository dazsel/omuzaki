//! Region comparison: the numbers are `compare()`'s on the cropped region, with hand-computed cases for the PSNR gate,
//! the table and the picture.

#![allow(clippy::unwrap_used)]

use image::{Rgba, RgbaImage};
use omuzaki_test::compare::{CompareError, Tolerance, compare};
use omuzaki_test::regioncmp::{
    MID_GREY, Rect, RegionError, T_FACE, T_FACE_MIN_PSNR_DB, compare_region, markdown_table, side_by_side,
    write_side_by_side,
};

fn flat(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba(color))
}

/// Smooth in x and y, with a few translucent and transparent pixels so premultiplication matters.
fn busy(width: u32, height: u32, seed: u32) -> RgbaImage {
    let byte = |v: u32| u8::try_from(v % 256).unwrap();
    RgbaImage::from_fn(width, height, |x, y| {
        let alpha = match (x * 7 + y * 3 + seed) % 11 {
            0 => 0,
            1 => 90,
            _ => 255,
        };
        Rgba([byte(x * 3 + seed), byte(y * 5 + seed * 2), byte(x + y + seed * 3), alpha])
    })
}

#[test]
fn the_full_image_region_reproduces_compare() {
    let (expected, actual) = (busy(48, 40, 1), busy(48, 40, 2));
    for tolerance in [Tolerance::EXACT, T_FACE, Tolerance { per_channel: 40, max_bad_fraction: 0.5 }] {
        let whole = compare(&actual, &expected, tolerance).unwrap();
        let region = compare_region(&actual, &expected, Rect::full(48, 40), tolerance).unwrap();
        assert_eq!(region.stats, whole.stats);
        assert_eq!(region.diff, whole.diff);
    }
}

#[test]
fn only_the_pixels_inside_the_rectangle_count() {
    let expected = flat(20, 10, [100, 100, 100, 255]);
    let mut actual = expected.clone();
    actual.put_pixel(2, 2, Rgba([200, 100, 100, 255])); // outside the rectangle below
    actual.put_pixel(12, 4, Rgba([200, 100, 100, 255])); // inside
    let rect = Rect { x: 10, y: 2, width: 6, height: 4 };

    let report = compare_region(&actual, &expected, rect, Tolerance::EXACT).unwrap();
    // One bad pixel of 6 x 4 = 24; the other changed pixel is not looked at.
    assert_eq!(report.stats.bad_pixels, 1);
    assert!((report.stats.bad_fraction - 1.0 / 24.0).abs() < 1e-12);
    assert_eq!(report.stats.max_delta, 100);
    assert_eq!((report.stats.width, report.stats.height), (6, 4));
    assert_eq!(report.diff.get_pixel(2, 2).0, [255, 0, 0, 255]); // (12, 4) in image coordinates
}

#[test]
fn the_psnr_gate_catches_what_the_bad_pixel_fraction_forgives() {
    // Every pixel's red is off by d. The mean square error over R, G, B, A is d^2 / 4, so PSNR = 10 log10(255^2 * 4 / d^2):
    // d = 10 gives 34.15 dB (passes 34), d = 11 gives 33.32 dB (fails). Both are inside the per-channel limit of 12.
    let expected = flat(8, 8, [100, 100, 100, 255]);
    let rect = Rect::full(8, 8);

    let ten = compare_region(&flat(8, 8, [110, 100, 100, 255]), &expected, rect, T_FACE).unwrap();
    assert!((ten.stats.psnr_db.unwrap() - 34.15).abs() < 0.01);
    assert!(ten.stats.passed && ten.passes(T_FACE_MIN_PSNR_DB));

    let eleven = compare_region(&flat(8, 8, [111, 100, 100, 255]), &expected, rect, T_FACE).unwrap();
    assert!((eleven.stats.psnr_db.unwrap() - 33.32).abs() < 0.01);
    assert!(eleven.stats.passed, "every pixel is within 12 per channel");
    assert!(!eleven.passes(T_FACE_MIN_PSNR_DB));
}

#[test]
fn identical_regions_pass_any_psnr_and_the_face_rule_is_the_documented_one() {
    let img = busy(16, 16, 3);
    let report = compare_region(&img, &img, Rect::full(16, 16), Tolerance::EXACT).unwrap();
    assert_eq!(report.stats.psnr_db, None);
    assert!(report.passes(f64::MAX));
    assert_eq!((T_FACE.per_channel, T_FACE.max_bad_fraction, T_FACE_MIN_PSNR_DB), (12, 0.01, 34.0));
}

#[test]
fn bad_rectangles_and_images_are_errors_not_passes() {
    let img = flat(10, 10, [0; 4]);
    let region = |rect| compare_region(&img, &img, rect, Tolerance::EXACT).unwrap_err();

    let empty = Rect { x: 1, y: 1, width: 0, height: 3 };
    assert_eq!(region(empty), RegionError::Empty(empty));
    let wide = Rect { x: 6, y: 0, width: 5, height: 1 };
    assert_eq!(region(wide), RegionError::OutOfBounds { rect: wide, size: (10, 10) });
    // x + width wraps in u32 arithmetic; it must still be rejected.
    let wrap = Rect { x: u32::MAX, y: 0, width: 2, height: 1 };
    assert_eq!(region(wrap), RegionError::OutOfBounds { rect: wrap, size: (10, 10) });

    let other = flat(10, 11, [0; 4]);
    let err = compare_region(&img, &other, Rect::full(10, 10), Tolerance::EXACT).unwrap_err();
    assert_eq!(err, RegionError::Compare(CompareError::SizeMismatch { actual: (10, 10), expected: (10, 11) }));
    let bad = Tolerance { per_channel: 0, max_bad_fraction: 2.0 };
    let err = compare_region(&img, &img, Rect::full(10, 10), bad).unwrap_err();
    assert!(matches!(err, RegionError::Compare(CompareError::InvalidTolerance { .. })));
}

#[test]
fn the_markdown_table_has_one_row_per_region() {
    let expected = flat(8, 8, [100, 100, 100, 255]);
    let eyes =
        compare_region(&flat(8, 8, [110, 100, 100, 255]), &expected, Rect { x: 0, y: 0, width: 4, height: 4 }, T_FACE)
            .unwrap();
    let mouth =
        compare_region(&flat(8, 8, [140, 100, 100, 255]), &expected, Rect { x: 4, y: 4, width: 4, height: 2 }, T_FACE)
            .unwrap();
    // The mouth's red is off by 40 everywhere: mean square error 40^2 / 4 = 400, PSNR = 10 log10(255^2 / 400) = 22.11 dB.
    let table = markdown_table(&[("eyes", &eyes), ("mouth | lips", &mouth)], T_FACE_MIN_PSNR_DB);
    assert_eq!(
        table,
        "| region | rect | bad pixels | max delta | PSNR | result |\n\
         |---|---|---:|---:|---:|---|\n\
         | eyes | 0,0 4x4 | 0.000% | 10 | 34.15 dB | pass |\n\
         | mouth \\| lips | 4,4 4x2 | 100.000% | 40 | 22.11 dB | **FAIL** |\n"
    );
    let same = compare_region(&expected, &expected, Rect::full(8, 8), T_FACE).unwrap();
    assert!(markdown_table(&[("all", &same)], T_FACE_MIN_PSNR_DB).contains("| 0.000% | 0 | inf | pass |"));
}

#[test]
fn the_picture_is_expected_actual_diff_composited_over_mid_grey() {
    let mut expected = flat(6, 4, [255, 0, 0, 255]);
    expected.put_pixel(0, 0, Rgba([255, 0, 0, 128])); // half transparent red
    expected.put_pixel(1, 0, Rgba([9, 9, 9, 0])); // fully transparent
    let actual = flat(6, 4, [0, 0, 255, 255]);
    let rect = Rect { x: 0, y: 0, width: 3, height: 2 };
    let report = compare_region(&actual, &expected, rect, Tolerance::EXACT).unwrap();

    for scale in [1_u32, 3] {
        let sheet = side_by_side(&actual, &expected, &report, scale).unwrap();
        let (w, h) = (3 * scale, 2 * scale);
        assert_eq!(sheet.dimensions(), (3 * w + 8, h));
        // Half-transparent red over 128 grey: (255 * 128 + 128 * 127 + 127) / 255 = 192 for red, 64 for the others.
        assert_eq!(sheet.get_pixel(0, 0).0, [192, 64, 64, 255]);
        // Fully transparent shows the grey itself.
        assert_eq!(sheet.get_pixel(scale, 0).0, [MID_GREY, MID_GREY, MID_GREY, 255]);
        // The actual panel starts one panel width plus the 4 px gap along; the diff panel after another.
        assert_eq!(sheet.get_pixel(w + 4, 0).0, [0, 0, 255, 255]);
        assert_eq!(sheet.get_pixel(2 * (w + 4), 0).0, [255, 0, 0, 255]); // every pixel differs: solid red
        assert_eq!(sheet.get_pixel(w, 0).0, [32, 32, 32, 255]); // the gap
    }
}

#[test]
fn the_picture_is_written_as_a_png_into_new_folders() {
    let expected = busy(12, 12, 1);
    let actual = busy(12, 12, 2);
    let report = compare_region(&actual, &expected, Rect { x: 2, y: 2, width: 5, height: 4 }, T_FACE).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("evidence").join("pr").join("eyes.png");
    write_side_by_side(&path, &actual, &expected, &report, 2).unwrap();
    let back = image::open(&path).unwrap().to_rgba8();
    assert_eq!(back, side_by_side(&actual, &expected, &report, 2).unwrap());

    // A path that cannot be created is a reported error naming the path.
    let blocked = dir.path().join("evidence").join("pr").join("eyes.png").join("under-a-file.png");
    let err = write_side_by_side(&blocked, &actual, &expected, &report, 1).unwrap_err();
    assert!(matches!(&err, RegionError::Write { path, .. } if *path == blocked), "{err}");
}
