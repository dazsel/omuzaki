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
use omuzaki_face_api::{Cap, DrawList, FaceRasterizer, Join, Raster};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, RegionReport, compare_region, write_side_by_side};
use omuzaki_test_face::probes::{
    HEIGHT, PX_PER_M, Probe, WIDTH, clipped_radial_gradient, nested_transforms, overlapping_fills, polygon, polyline,
    rgba, spec, straight_image, stroke, tiny_ellipse, translucent_stack, v,
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

// ---------------------------------------------------------------------------------------------- the eight probes

/// A wide round-capped stroke along a curve and a corner, a translucent second stroke over it, and a butt-capped bar.
fn round_capped_stroke(d: &mut DrawList) {
    let curve = d
        .path()
        .move_to(v(-0.1, -0.04))
        .cubic_to(v(-0.06, 0.08), v(0.0, -0.08), v(0.04, 0.03))
        .line_to(v(0.1, 0.05))
        .finish();
    stroke(d, curve, rgba(90, 63, 192, 255), 0.022, Cap::Round, Join::Round, 1.0);
    let zig = polyline(d, &[[-0.09, 0.02], [0.0, -0.03], [0.09, -0.005]]);
    stroke(d, zig, rgba(255, 255, 255, 255), 0.008, Cap::Round, Join::Round, 0.6);
    let bar = polyline(d, &[[-0.1, 0.065], [0.1, 0.065]]);
    stroke(d, bar, rgba(224, 122, 31, 255), 0.012, Cap::Butt, Join::Miter, 1.0);
}

/// A 25 degree corner (miter ratio 4.6: mitred under Canvas's limit of 10, bevelled under Skia's default of 4), a closed
/// diamond and a round-joined V.
fn miter_join(d: &mut DrawList) {
    let sharp = polyline(d, &[[-0.1, -0.04], [0.09, 0.02], [-0.07, 0.0411]]);
    stroke(d, sharp, rgba(200, 60, 40, 255), 0.01, Cap::Butt, Join::Miter, 1.0);
    let diamond = polygon(d, &[[0.025, -0.05], [0.0, -0.025], [-0.025, -0.05], [0.0, -0.075]]);
    stroke(d, diamond, rgba(43, 122, 93, 255), 0.008, Cap::Butt, Join::Miter, 1.0);
    let vee = polyline(d, &[[0.06, -0.03], [0.09, -0.07], [0.12, -0.03]]);
    stroke(d, vee, rgba(50, 70, 160, 255), 0.012, Cap::Butt, Join::Round, 1.0);
}

/// Strokes from 0.05 to 1.2 px across, diagonal, axis-aligned and curved: Skia draws them as modulated hairlines.
fn hairline(d: &mut DrawList) {
    let ink = rgba(20, 20, 20, 255);
    for (points, width, cap, join) in [
        ([[-0.11, 0.06], [0.11, 0.045]], 0.0003, Cap::Round, Join::Round),
        ([[-0.11, 0.035], [0.11, 0.0]], 0.0008, Cap::Round, Join::Round),
        ([[-0.11, -0.005], [0.11, -0.04]], 0.0012, Cap::Round, Join::Round),
        ([[-0.11, -0.0625], [0.11, -0.0625]], 0.0005, Cap::Butt, Join::Miter),
        ([[-0.0951, -0.07], [-0.0951, 0.07]], 0.0005, Cap::Butt, Join::Miter),
    ] {
        let p = polyline(d, &points);
        stroke(d, p, ink, width, cap, join, 1.0);
    }
    let curve = d.path().move_to(v(-0.08, -0.06)).cubic_to(v(-0.04, 0.05), v(0.04, -0.05), v(0.08, 0.06)).finish();
    stroke(d, curve, rgba(200, 30, 30, 255), 0.0004, Cap::Round, Join::Round, 1.0);
    let faint = polyline(d, &[[-0.11, -0.03], [0.11, -0.05]]);
    stroke(d, faint, ink, 0.00005, Cap::Round, Join::Round, 1.0);
}

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
