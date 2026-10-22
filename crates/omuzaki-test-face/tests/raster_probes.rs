//! Chrome-canvas parity for the CPU face rasteriser (CB-24): eight synthetic probes, drawn by headless Chromium's Canvas 2D
//! (a Chromium probe script saves them as `fixtures/raster/*.png`) and again here from `DrawList`s with `SkiaRasterizer`.
//!
//! Each probe is a 256 x 160 px image of a 0.256 x 0.160 m face-plane rect at 1000 px/m. The drawings below are the same
//! eight as in the script, command for command; change one and change the other, then regenerate the PNGs.
//!
//! The rule is the issue's, on every probe: per channel 6, at most 0.5 % bad pixels, PSNR of at least 38 dB. It is tight on
//! purpose: the Rust-against-JavaScript face goldens inherit it, and a loose bar would hide real face-painting errors. It is
//! only reachable because the rasteriser repeats what Skia's software pipeline does to a path (see `raster::scan`): Chrome
//! snaps every edge end to a quarter pixel and flattens curves in its own way, so the pixels on a shape's edge only agree
//! when those steps do.
//!
//! What each probe pins down:
//! - `overlapping-fills`: the non-zero winding rule, opposite-wound holes and edges at fractional pixels;
//! - `clipped-radial-gradient`: straight-colour gradient interpolation, nested anti-aliased clips, a linear gradient;
//! - `round-capped-stroke`: the stroker's offset curves, round caps and joins, a translucent stroke over another;
//! - `miter-join`: miter versus the canvas's limit of 10 (a 25 degree corner is mitred; Skia's default limit of 4 would bevel it);
//! - `translucent-stack`: 8-bit premultiplied blending, paint alpha and command opacity together;
//! - `tiny-ellipse`: shapes a few pixels across, where coverage is all there is;
//! - `hairline`: strokes under a pixel (Skia's modulated one-pixel lines) and a 1.2 px line that is outlined instead;
//! - `nested-transforms`: rotation, non-uniform scale, strokes that scale with the matrix, gradients in local space.
//!
//! Set `OMUZAKI_RASTER_EVIDENCE=<dir>` to also write a Chrome / Rust / diff picture per probe (the diff marks pixels outside
//! the rule) and a table of the numbers: the PR's evidence.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use image::RgbaImage;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{DrawList, FaceRasterizer, Raster};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, RegionReport, compare_region, write_side_by_side};
use omuzaki_test_face::probes::{
    HEIGHT, PX_PER_M, Probe, WIDTH, clipped_radial_gradient, hairline, miter_join, nested_transforms,
    overlapping_fills, round_capped_stroke, spec, straight_image, tiny_ellipse, translucent_stack,
};

/// What the issue asks of every probe (with [`MIN_PSNR_DB`]).
const RULE: Tolerance = Tolerance { per_channel: 6, max_bad_fraction: 0.005 };
/// The PSNR every probe must reach.
const MIN_PSNR_DB: f64 = 38.0;

/// The probes, in the order and under the names of the probe script.
const PROBES: [(&str, Probe); 8] = [
    ("overlapping-fills", overlapping_fills),
    ("clipped-radial-gradient", clipped_radial_gradient),
    ("round-capped-stroke", round_capped_stroke),
    ("miter-join", miter_join),
    ("translucent-stack", translucent_stack),
    ("tiny-ellipse", tiny_ellipse),
    ("hairline", hairline),
    ("nested-transforms", nested_transforms),
];

// ---------------------------------------------------------------------------------------------- rasterising and comparing

fn rust_image(probe: Probe) -> RgbaImage {
    let mut list = DrawList::new();
    probe(&mut list);
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec(), &list, PX_PER_M, &mut raster).unwrap();
    assert_eq!((raster.width, raster.height), (WIDTH, HEIGHT));
    straight_image(raster.width, raster.height, &raster.pixels).unwrap()
}

fn chrome_image(name: &str) -> RgbaImage {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/raster").join(format!("{name}.png"));
    image::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).to_rgba8()
}

/// The comparison of one probe under [`RULE`].
fn compare_probe(chrome: &RgbaImage, rust: &RgbaImage) -> RegionReport {
    compare_region(rust, chrome, Rect::full(WIDTH, HEIGHT), RULE).unwrap()
}

#[test]
fn every_probe_matches_chrome_canvas() {
    let mut failures = String::new();
    for (name, probe) in PROBES {
        let report = compare_probe(&chrome_image(name), &rust_image(probe));
        if !report.passes(MIN_PSNR_DB) {
            let _ = writeln!(failures, "{name}: {}", report.stats);
        }
    }
    assert!(failures.is_empty(), "probes outside {RULE:?} / {MIN_PSNR_DB} dB:\n{failures}");
}

#[test]
fn the_probes_are_not_blank() {
    // A rasteriser that drew nothing would match a nearly empty reference on the tiny probes; each picture must hold ink.
    for (name, probe) in PROBES {
        let ink = chrome_image(name).pixels().filter(|p| p.0[3] > 0).count();
        assert!(ink > 8, "{name}: the Chrome reference has {ink} inked pixels");
        let ours = rust_image(probe).pixels().filter(|p| p.0[3] > 0).count();
        assert!(ours > 8, "{name}: the rasteriser drew {ours} inked pixels");
    }
}

#[test]
fn a_different_picture_fails_the_rule() {
    // The rule must bite: one probe compared against another's reference is far outside it.
    let rust = rust_image(miter_join);
    let wrong = chrome_image("round-capped-stroke");
    assert!(!compare_probe(&wrong, &rust).passes(MIN_PSNR_DB));
}

#[test]
fn a_small_shift_fails_the_rule() {
    // So does a face feature in the wrong place: the miter probe moved one pixel right is a different picture.
    let rust = rust_image(miter_join);
    let chrome = chrome_image("miter-join");
    let mut shifted = RgbaImage::new(WIDTH, HEIGHT);
    image::imageops::replace(&mut shifted, &rust, 1, 0);
    assert!(!compare_probe(&chrome, &shifted).passes(MIN_PSNR_DB));
}

#[test]
fn evidence() {
    let Some(dir) = std::env::var_os("OMUZAKI_RASTER_EVIDENCE").map(PathBuf::from) else { return };
    let mut table = String::from(
        "| probe | bad pixels | bad fraction | max delta | PSNR | result |\n|---|---:|---:|---:|---:|---|\n",
    );
    for (name, probe) in PROBES {
        let (chrome, rust) = (chrome_image(name), rust_image(probe));
        let report = compare_probe(&chrome, &rust);
        write_side_by_side(&dir.join(format!("{name}.png")), &rust, &chrome, &report, 3).unwrap();
        rust.save(dir.join(format!("{name}.rust.png"))).unwrap();
        let psnr = report.stats.psnr_db.map_or_else(|| "inf".to_owned(), |db| format!("{db:.2} dB"));
        let _ = writeln!(
            table,
            "| {name} | {} | {:.3} % | {} | {psnr} | {} |",
            report.stats.bad_pixels,
            report.stats.bad_fraction * 100.0,
            report.stats.max_delta,
            if report.passes(MIN_PSNR_DB) { "pass" } else { "FAIL" },
        );
    }
    std::fs::write(dir.join("probes.md"), table).unwrap();
}
