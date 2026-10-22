//! The GPU face rasteriser's strokes against the CPU one (#318): caps, joins and widths from 1 to 40 px, the canvas
//! probes' strokes, and hairlines, drawn by `VelloRasterizer` and by `SkiaRasterizer`, then compared.
//!
//! The rule for a stroke: away from its anti-aliased edge the two rasters agree (no pixel whose 3 x 3 neighbourhood is
//! the same on the CPU raster differs by more than 8 levels), the ink (summed alpha) agrees within 1 %, and the PSNR is
//! at least 38 dB. On the edge pixels themselves they may differ: the CPU raster snaps edges to quarter rows as Chrome
//! does, and the GPU path covers them exactly on purpose, so a thin line's ink doesn't swing with its sub-pixel position
//! (#888, ruling (a); `face_gpu_subpixel.rs`). That is why a stroke-heavy probe has 1 to 3.5 % "bad" pixels under the
//! fills' rule, all of them on edges. A hairline is a one-pixel line with its alpha scaled by the width, as Skia draws
//! it; it must keep at least 0.9 of the CPU raster's coverage (and gain no more than a tenth). Without an adapter (no
//! lavapipe, no GPU) the tests print why and pass.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;

use common::{Gpu, cpu_pixels, device, gpu_pixels, image};
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face_api::{Cap, DrawList, Join};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, compare_region, write_side_by_side};
use omuzaki_test_face::probes::{
    HEIGHT, PX_PER_M, Probe, WIDTH, hairline, miter_join, polyline, rgba, round_capped_stroke, spec, stroke,
    stroke_widths, v,
};

/// The fills' per-channel rule, reported for each probe (`face_gpu_fills.rs`).
const RULE: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.01 };
/// The most a pixel off the edge may differ, per channel.
const OFF_EDGE: u8 = 8;
/// The most the summed alpha may differ, as a fraction.
const MAX_INK_DIFFERENCE: f64 = 0.01;
/// The same for hairlines (see [`PROBES`]), as in [`a_hairline_keeps_its_coverage_and_never_vanishes`].
const MAX_HAIRLINE_INK_DIFFERENCE: f64 = 0.1;
const MIN_PSNR_DB: f64 = 38.0;

/// The probes, with the most their ink may differ. Skia's hairline puts one pixel of coverage per step along the line's
/// major axis, so a diagonal or curved hairline has `cos θ` of a true one-pixel line's ink; the GPU draws the true line,
/// a few percent more ink on the hairline probe's slopes and curve.
const PROBES: [(&str, Probe, f64); 4] = [
    ("stroke-widths", stroke_widths, MAX_INK_DIFFERENCE),
    ("round-capped-stroke", round_capped_stroke, MAX_INK_DIFFERENCE),
    ("miter-join", miter_join, MAX_INK_DIFFERENCE),
    ("hairline", hairline, MAX_HAIRLINE_INK_DIFFERENCE),
];

fn pixels(gpu: &Gpu, rasterizer: &mut VelloRasterizer, probe: Probe) -> (Vec<u8>, Vec<u8>) {
    let mut list = DrawList::new();
    probe(&mut list);
    (gpu_pixels(gpu, rasterizer, &spec(), &list), cpu_pixels(&spec(), &list))
}

#[test]
fn the_stroke_probes_match_the_cpu_raster() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let (mut table, mut failures) = (String::new(), String::new());
    for (name, probe, max_ink) in PROBES {
        let (g, c) = pixels(&gpu, &mut rasterizer, probe);
        let ink = g.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
        assert!(ink > 8, "{name}: the GPU drew {ink} inked pixels");
        let report =
            compare_region(&image((WIDTH, HEIGHT), &g), &image((WIDTH, HEIGHT), &c), Rect::full(WIDTH, HEIGHT), RULE)
                .unwrap();
        let _ = writeln!(
            table,
            "{name}: bad fraction {:.3} %, max difference {}, PSNR {:?} dB",
            report.stats.bad_fraction * 100.0,
            report.stats.max_delta,
            report.stats.psnr_db
        );
        // With the evidence folder set, each probe is written as GPU | CPU | difference, four times enlarged.
        if let Some(dir) = std::env::var_os("OMUZAKI_FACE_EVIDENCE_DIR") {
            let path = std::path::Path::new(&dir).join(format!("gpu-stroke-{name}.png"));
            write_side_by_side(&path, &image((WIDTH, HEIGHT), &g), &image((WIDTH, HEIGHT), &c), &report, 4).unwrap();
        }
        let off_edge = off_edge_differences(&g, &c);
        let ink = coverage(&g) / coverage(&c) - 1.0;
        let _ = writeln!(
            table,
            "  off the edge: {off_edge} pixels differ by more than {OFF_EDGE}; ink {:+.2} %",
            ink * 100.0
        );
        if off_edge > 0 || ink.abs() > max_ink || report.stats.psnr_db.is_none_or(|db| db < MIN_PSNR_DB) {
            let _ =
                writeln!(failures, "{name}: {off_edge} off-edge pixels, ink {:+.2} %, {}", ink * 100.0, report.stats);
        }
    }
    eprintln!("{table}");
    assert!(failures.is_empty(), "stroke probes outside the rule:\n{failures}");
}

/// The pixels that differ by more than [`OFF_EDGE`] in a channel although the CPU raster's 3 x 3 neighbourhood around
/// them is uniform (all the same pixel): a difference off the anti-aliased edge.
fn off_edge_differences(gpu: &[u8], cpu: &[u8]) -> usize {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let px = |x: usize, y: usize| &cpu[(y * w + x) * 4..(y * w + x) * 4 + 4];
    let mut count = 0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let uniform = (y - 1..=y + 1).all(|yy| (x - 1..=x + 1).all(|xx| px(xx, yy) == px(x, y)));
            let i = (y * w + x) * 4;
            if uniform && (0..4).any(|k| gpu[i + k].abs_diff(cpu[i + k]) > OFF_EDGE) {
                count += 1;
            }
        }
    }
    count
}

/// The summed alpha of a raster.
fn coverage(pixels: &[u8]) -> f64 {
    pixels.as_chunks::<4>().0.iter().map(|p| f64::from(p[3])).sum()
}

#[test]
fn a_hairline_keeps_its_coverage_and_never_vanishes() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    for width_px in [0.3_f32, 0.05, 0.6, 0.95] {
        let probe = |d: &mut DrawList, width: f32| {
            let ink = rgba(20, 20, 20, 255);
            for (points, cap) in [
                ([[-0.11, 0.06], [0.11, 0.045]], Cap::Round),
                ([[-0.11, -0.0625], [0.11, -0.0625]], Cap::Butt),
                ([[-0.0951, -0.07], [-0.0951, 0.07]], Cap::Butt),
            ] {
                let p = polyline(d, &points);
                stroke(d, p, ink, width, cap, Join::Round, 1.0);
            }
            let curve =
                d.path().move_to(v(-0.08, -0.06)).cubic_to(v(-0.04, 0.05), v(0.04, -0.05), v(0.08, 0.06)).finish();
            stroke(d, curve, ink, width, Cap::Round, Join::Round, 1.0);
        };
        let mut list = DrawList::new();
        probe(&mut list, width_px / PX_PER_M);
        let (g, c) = (gpu_pixels(&gpu, &mut rasterizer, &spec(), &list), cpu_pixels(&spec(), &list));
        let (cg, cc) = (coverage(&g), coverage(&c));
        let kept = cg / cc;
        eprintln!("{width_px} px hairline: GPU keeps {kept:.3} of the CPU raster's coverage");
        assert!(cc > 0.0, "{width_px} px: the CPU raster drew nothing");
        assert!((0.9..=1.1).contains(&kept), "{width_px} px: the GPU kept {kept:.3} of the coverage");
    }
}
