//! The GPU face rasteriser against the CPU one (CB-24, #1433 A3): the fill probes of `raster_probes.rs` drawn by
//! `VelloRasterizer` into a texture shaped like the renderer's face texture, and by `SkiaRasterizer`, then compared.
//!
//! The rule is the issue's: per channel 8, at most 1 % bad pixels, PSNR of at least 34 dB (looser than the Chrome probes':
//! Vello and Skia anti-alias differently). The radial gradient is also within max 8 and p99 5 levels on its interior
//! (decision 7.5; #2002 allowed 6 there, and the half-pixel gradient shift in the encoder made 5 reachable). Without an
//! adapter (no lavapipe, no GPU) the tests print why and pass. The stroke probes are #318's.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;

use common::{Gpu, cpu_pixels, device, face_texture, gpu_pixels, image, paint, read_back};
use image::RgbaImage;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face_api::{DrawList, FaceLayerSpec};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, compare_region};
use omuzaki_test_face::probes::{
    HEIGHT, Probe, WIDTH, clipped_radial_gradient, nested_transforms, overlapping_fills, spec, tiny_ellipse,
    translucent_stack,
};

/// What the issue asks of every fill probe (with [`MIN_PSNR_DB`]).
const RULE: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.01 };
const MIN_PSNR_DB: f64 = 34.0;
/// The most the radial gradient may differ, and the most 99 % of its pixels may (decision 7.5, measured inside the shape).
const GRADIENT_MAX: u8 = 8;
const GRADIENT_P99: u8 = 5;

/// The rule for the two probes that stroke curves. Snapped fills now differ from the CPU raster on 0.02 to 0.04 % of pixels,
/// but the stroker's outlines are not snapped on the GPU, so a stroked ring or corner differs on its edge pixels
/// (strokes are #318's). Ruled on #2002: 0.03 for such probes, with the reason here.
const STROKED: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.03 };

const PROBES: [(&str, Probe, Tolerance); 5] = [
    ("overlapping-fills", overlapping_fills, RULE),
    ("clipped-radial-gradient", clipped_radial_gradient, RULE),
    ("translucent-stack", translucent_stack, STROKED),
    ("tiny-ellipse", tiny_ellipse, RULE),
    ("nested-transforms", nested_transforms, STROKED),
];

/// The layer every probe is rasterised under.
fn layer() -> FaceLayerSpec {
    spec()
}

/// The probe on the GPU, as premultiplied RGBA8.
fn gpu_probe(gpu: &Gpu, rasterizer: &mut VelloRasterizer, probe: Probe) -> Vec<u8> {
    let mut list = DrawList::new();
    probe(&mut list);
    gpu_pixels(gpu, rasterizer, &layer(), &list)
}

/// The probe on the CPU, as premultiplied RGBA8.
fn cpu_probe(probe: Probe) -> Vec<u8> {
    let mut list = DrawList::new();
    probe(&mut list);
    cpu_pixels(&layer(), &list)
}

fn probe_image(pixels: &[u8]) -> RgbaImage {
    image((WIDTH, HEIGHT), pixels)
}

/// The `p`-th percentile (0..=100) and the maximum of the channel differences over the pixels whose whole 5 x 5
/// neighbourhood the CPU raster inked. Edge pixels are left out: they are the fills' rule, and the gradient bound is
/// about the ramp (decision 7.5).
fn interior_difference(gpu: &[u8], cpu: &[u8], percentile: usize) -> (u8, u8) {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let inked = |x: usize, y: usize| cpu[(y * w + x) * 4 + 3] > 0;
    let mut diffs = Vec::new();
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            if (y - 2..=y + 2).all(|yy| (x - 2..=x + 2).all(|xx| inked(xx, yy))) {
                let i = (y * w + x) * 4;
                diffs.extend((0..4).map(|k| gpu[i + k].abs_diff(cpu[i + k])));
            }
        }
    }
    diffs.sort_unstable();
    let at = (diffs.len() * percentile / 100).min(diffs.len().saturating_sub(1));
    (diffs.get(at).copied().unwrap_or(0), diffs.last().copied().unwrap_or(0))
}

#[test]
fn the_fill_probes_match_the_cpu_raster() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let mut table = String::new();
    let mut failures = String::new();
    for (name, probe, rule) in PROBES {
        let (g, c) = (gpu_probe(&gpu, &mut rasterizer, probe), cpu_probe(probe));
        let ink = g.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
        assert!(ink > 8, "{name}: the GPU drew {ink} inked pixels");
        let report = compare_region(&probe_image(&g), &probe_image(&c), Rect::full(WIDTH, HEIGHT), rule).unwrap();
        let _ = writeln!(
            table,
            "{name}: bad fraction {:.3} %, max difference {}, PSNR {:?} dB",
            report.stats.bad_fraction * 100.0,
            report.stats.max_delta,
            report.stats.psnr_db
        );
        if !report.passes(MIN_PSNR_DB) {
            let _ = writeln!(failures, "{name}: {}", report.stats);
        }
    }
    eprintln!("{table}");
    assert!(failures.is_empty(), "probes outside their rule / {MIN_PSNR_DB} dB:\n{failures}");
}

#[test]
fn the_radial_gradient_is_within_decision_7_5() {
    let Some(gpu) = device() else { return };
    let (g, c) =
        (gpu_probe(&gpu, &mut VelloRasterizer::new(), clipped_radial_gradient), cpu_probe(clipped_radial_gradient));
    let (p99, max) = interior_difference(&g, &c, 99);
    eprintln!("clipped-radial-gradient: max {max}, p99 {p99} levels per channel");
    assert!(
        max <= GRADIENT_MAX && p99 <= GRADIENT_P99,
        "max {max} (limit {GRADIENT_MAX}), p99 {p99} (limit {GRADIENT_P99})"
    );
}

#[test]
fn a_repaint_with_the_same_rasteriser_gives_the_same_pixels() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let first = gpu_probe(&gpu, &mut rasterizer, overlapping_fills);
    gpu_probe(&gpu, &mut rasterizer, tiny_ellipse);
    assert_eq!(first, gpu_probe(&gpu, &mut rasterizer, overlapping_fills), "a reused scene and texture left traces");
}

#[test]
fn a_native_handle_that_is_not_the_tuple_is_declined_and_nothing_is_drawn() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let mut list = DrawList::new();
    overlapping_fills(&mut list);
    let (drawn, error) = paint(&gpu, &mut rasterizer, &layer(), &list, &mut 7_u32);
    assert!(!drawn, "a u32 is not a native handle");
    assert!(error.is_none(), "{error:?}");
    // A tuple in the wrong order is declined too, and the texture it holds stays empty.
    let texture = face_texture(&gpu.0, (WIDTH, HEIGHT));
    let mut wrong_order = (texture.clone(), gpu.0.clone(), gpu.1.clone());
    let (drawn, _) = paint(&gpu, &mut rasterizer, &layer(), &list, &mut wrong_order);
    assert!(!drawn);
    // The convention before #2074, without the renderer's encoder, is declined: a renderer that drifted back to it
    // gets the CPU raster, not a paint that is never submitted.
    let mut without_encoder = (gpu.0.clone(), gpu.1.clone(), texture.clone());
    let (drawn, _) = paint(&gpu, &mut rasterizer, &layer(), &list, &mut without_encoder);
    assert!(!drawn, "a (Device, Queue, Texture) tuple is not the native handle any more");
    assert!(read_back(&gpu, &texture).iter().all(|&b| b == 0), "a declined paint wrote pixels");
}
