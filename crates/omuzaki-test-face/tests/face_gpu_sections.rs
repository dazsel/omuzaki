//! The real painter's sections on the GPU against the CPU raster (CB-24, #1433 A4): each section of the synthetic face,
//! neutral and with the expressions that move it, painted alone by `RealPainter::paint_section`, then its Base layer
//! rasterised by `VelloRasterizer` and by `SkiaRasterizer` at probe scale (about 256 px wide) and compared.
//!
//! The rule is A3's: per channel 8, at most 1 % bad pixels, PSNR of at least 34 dB. Faces are made of curves, which Skia
//! flattens and snaps differently from Vello (see `face_gpu_fills.rs`), so a case that misses only on bad fraction names its
//! own looser bound. Set `OMUZAKI_FACE_GPU_DIFF_DIR` to write each case's GPU, CPU and difference pictures there. Without an
//! adapter the tests print why and pass. The synthetic face only: never a private character.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;
use std::path::PathBuf;

use common::{cpu_pixels, device, gpu_pixels, image};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping, FaceProfile, FaceView, NoseKind,
    PaintParams,
};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, compare_region, write_side_by_side};

const MIN_PSNR_DB: f64 = 34.0;
/// The width the layer is rasterised at, in pixels.
const PROBE_WIDTH_PX: f32 = 256.0;
const RULE: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.01 };

/// One case: a section, a name for the expression, and what it changes in the synthetic profile and the resting parameters.
struct Case {
    section: Section,
    name: &'static str,
    set: fn(&mut FaceProfile, &mut PaintParams),
}

const CASES: [Case; 15] = [
    Case { section: Section::Eyes, name: "neutral", set: |_, _| {} },
    Case { section: Section::Eyes, name: "half-blink", set: |_, p| p.left_eye.close = 0.5 },
    Case { section: Section::Eyes, name: "wide-open", set: |_, p| p.left_eye.open = 1.6 },
    Case { section: Section::Eyes, name: "gaze-left-up", set: |_, p| p.gaze = [0.6, 0.4].into() },
    Case { section: Section::Mouth, name: "neutral", set: |_, _| {} },
    Case {
        section: Section::Mouth,
        name: "smile",
        set: |_, p| {
            p.mouth.smile_left = 1.0;
            p.mouth.smile_right = 1.0;
        },
    },
    Case { section: Section::Mouth, name: "open-half", set: |_, p| p.mouth.open = 0.5 },
    Case { section: Section::Brows, name: "neutral", set: |_, _| {} },
    Case {
        section: Section::Brows,
        name: "raised",
        set: |_, p| {
            p.left_brow.height = 1.0;
            p.right_brow.height = 1.0;
        },
    },
    Case {
        section: Section::Brows,
        name: "angry",
        set: |_, p| {
            p.left_brow.tilt = 1.5;
            p.right_brow.tilt = 1.5;
        },
    },
    Case { section: Section::Nose, name: "tick", set: |f, _| f.nose.kind = NoseKind::Tick },
    Case { section: Section::Nose, name: "dot", set: |f, _| f.nose.kind = NoseKind::Dot },
    Case { section: Section::Cheeks, name: "neutral", set: |_, _| {} },
    Case { section: Section::Cheeks, name: "blush", set: |_, p| p.blush = 1.0 },
    Case { section: Section::Cheeks, name: "blush-strong", set: |_, p| p.blush = 1.6 },
];

/// The Base layer's spec at probe scale: the projection's rectangle, about [`PROBE_WIDTH_PX`] wide.
fn probe_layer() -> FaceLayerSpec {
    let rect = FaceLayerSpec::for_layer(FaceLayerId::Base, &mock_projection(), DEFAULT_PX_PER_M).rect;
    FaceLayerSpec::for_rect(rect, FaceLayerId::Base, PROBE_WIDTH_PX / rect.width())
}

fn paint_case(case: &Case) -> FaceDrawing {
    let mut profile = mock_profile();
    let mut params = FaceMapping::default().rest_params();
    (case.set)(&mut profile, &mut params);
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    let mut drawing = FaceDrawing::new();
    RealPainter::new(mock_projection(), DEFAULT_PX_PER_M).paint_section(case.section, &input, &mut drawing).unwrap();
    drawing
}

#[test]
fn every_section_case_matches_the_cpu_raster() {
    let Some(gpu) = device() else { return };
    let diff_dir = std::env::var_os("OMUZAKI_FACE_GPU_DIFF_DIR").map(PathBuf::from);
    let spec = probe_layer();
    let size = spec.size_px();
    let mut rasterizer = VelloRasterizer::new();
    let mut table =
        String::from("| section | expression | bad fraction | max difference | PSNR |\n|---|---|---:|---:|---:|\n");
    let mut failures = String::new();
    for case in &CASES {
        let list = &paint_case(case)[FaceLayerId::Base];
        let (g, c) = (gpu_pixels(&gpu, &mut rasterizer, &spec, list), cpu_pixels(&spec, list));
        let (gi, ci) = (image(size, &g), image(size, &c));
        let report = compare_region(&gi, &ci, Rect::full(size.0, size.1), RULE).unwrap();
        let _ = writeln!(
            table,
            "| {:?} | {} | {:.3} % | {} | {} |",
            case.section,
            case.name,
            report.stats.bad_fraction * 100.0,
            report.stats.max_delta,
            report.stats.psnr_db.map_or_else(|| "inf".to_owned(), |db| format!("{db:.2} dB")),
        );
        if let Some(dir) = &diff_dir {
            std::fs::create_dir_all(dir).unwrap();
            let stem = format!("{:?}-{}", case.section, case.name).to_lowercase();
            write_side_by_side(&dir.join(format!("{stem}.png")), &gi, &ci, &report, 2).unwrap();
        }
        if !report.passes(MIN_PSNR_DB) {
            let _ = writeln!(failures, "{:?} {}: {}", case.section, case.name, report.stats);
        }
    }
    eprintln!("{table}");
    assert!(failures.is_empty(), "cases outside {RULE:?} / {MIN_PSNR_DB} dB:\n{failures}");
}

#[test]
fn every_case_draws_something() {
    // A blank list would match a blank list: each case must put ink on the CPU raster, and the Nose and Mouth are small.
    let spec = probe_layer();
    for case in &CASES {
        let list = &paint_case(case)[FaceLayerId::Base];
        let ink = cpu_pixels(&spec, list).as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
        assert!(ink > 3, "{:?} {}: only {ink} inked pixels", case.section, case.name);
    }
}
