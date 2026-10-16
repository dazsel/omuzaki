//! The face painter draws straight into the renderer (#151, #2074): through `PaintTarget::native` the face's
//! `VelloRasterizer` records into the wgpu renderer's encoder, so a face layer is drawn on the GPU with no CPU raster
//! and nothing read back, and the frame matches the CPU path (`write_rgba`) within the face goldens' rule (`T_FACE`,
//! 34 dB). The stage is where the face and the renderer may meet. The tests fail loudly if the convention drifts: a
//! renderer whose `native()` the face no longer recognises sends the face to the CPU raster, which they catch.
//!
//! They run on any adapter (lavapipe in CI, at probe density); without one they print why and pass.
//! `OMUZAKI_FACE_INTEROP_OUT=<dir>` saves the stills and the side-by-side comparison for a PR.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "a test writing its own evidence pictures; not an op")]

#[path = "face_gpu/support.rs"]
mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_render::NativePaintTarget;
use omuzaki_render_api::{PaintTarget, Renderer, TextureDesc};
use omuzaki_stage::frame::Stage;
use omuzaki_test::regioncmp::{Rect, RegionReport, T_FACE, T_FACE_MIN_PSNR_DB, compare_region, write_side_by_side};
use support::{Counted, PROBE_PX_PER_M, cpu_rig, expression, face_stage, gpu_rig, renderer, rgba};

/// The frame's size in pixels.
const SIZE: u32 = 256;
/// Frames run before the compared one, so every section has changed at least once on both stages.
const FRAMES: u32 = 40;

/// Runs `frames` frames of [`expression`] and returns the last one's picture.
fn run(stage: &mut Stage, renderer: &mut dyn Renderer, frames: u32) -> image::RgbaImage {
    for n in 0..frames {
        stage.tick(renderer, &expression(n)).expect("a frame");
    }
    rgba(&renderer.read_target(stage.target()).expect("the frame reads back"))
}

/// Saves `gpu`, `cpu` and their comparison under `OMUZAKI_FACE_INTEROP_OUT`, if it is set.
fn save(name: &str, gpu: &image::RgbaImage, cpu: &image::RgbaImage, report: &RegionReport) {
    let Some(dir) = std::env::var_os("OMUZAKI_FACE_INTEROP_OUT").map(PathBuf::from) else { return };
    std::fs::create_dir_all(&dir).unwrap();
    gpu.save(dir.join(format!("{name}-gpu.png"))).unwrap();
    cpu.save(dir.join(format!("{name}-cpu.png"))).unwrap();
    write_side_by_side(&dir.join(format!("{name}-gpu-vs-cpu.png")), gpu, cpu, report, 2).unwrap();
}

#[test]
fn the_renderers_native_handle_is_the_one_the_face_rasteriser_draws_into() {
    let Some(mut renderer) = renderer() else { return };
    let texture =
        renderer.create_texture(&TextureDesc::colour(omuzaki_core::glam::UVec2::splat(8)).dynamic(), None).unwrap();
    let mut recognised = false;
    renderer
        .paint_texture(texture, &mut |target: &mut dyn PaintTarget| {
            recognised = target.native().downcast_mut::<NativePaintTarget>().is_some();
            Ok(())
        })
        .unwrap();
    assert!(
        recognised,
        "omuzaki-render's native() is no longer its NativePaintTarget; the face rasteriser names the same tuple, \
         (Device, Queue, Texture, CommandEncoder), and both must change together"
    );
}

#[test]
fn the_live_stage_paints_its_face_on_the_gpu_and_the_frame_matches_the_cpu_path() {
    let Some(mut renderer) = renderer() else { return };
    let drawn = Arc::new(AtomicUsize::new(0));
    let mut gpu_stage = face_stage(&mut renderer, gpu_rig(Box::new(Counted::new(&drawn))), SIZE, PROBE_PX_PER_M);
    let mut cpu_stage = face_stage(&mut renderer, cpu_rig(), SIZE, PROBE_PX_PER_M);
    let layers = gpu_stage.character().face_layers().count();
    assert!(layers > 0, "the mock character composites face layers");

    let gpu = run(&mut gpu_stage, &mut renderer, FRAMES);
    let cpu = run(&mut cpu_stage, &mut renderer, FRAMES);
    let drew = drawn.load(Ordering::Relaxed);
    assert!(
        drew >= FRAMES as usize,
        "the GPU rasteriser drew {drew} of the face's paints over {FRAMES} frames: the face fell back to the CPU \
         raster, so the renderer's native() and the face's convention have drifted apart"
    );
    let report = compare_region(&gpu, &cpu, Rect::full(SIZE, SIZE), T_FACE).unwrap();
    eprintln!("the face drawn on the GPU against the CPU path, frame {FRAMES}: {}", report.stats);
    save("stage-face", &gpu, &cpu, &report);
    assert!(report.passes(T_FACE_MIN_PSNR_DB), "the GPU face differs from the CPU path: {}", report.stats);
    for stage in [gpu_stage, cpu_stage] {
        stage.release(&mut renderer).unwrap();
    }
}

#[test]
fn after_a_device_loss_the_face_is_drawn_whole_again() {
    let Some(mut renderer) = renderer() else { return };
    let drawn = Arc::new(AtomicUsize::new(0));
    let mut stage = face_stage(&mut renderer, gpu_rig(Box::new(Counted::new(&drawn))), SIZE, PROBE_PX_PER_M);
    let mut reference = face_stage(&mut renderer, cpu_rig(), SIZE, PROBE_PX_PER_M);
    // Both stages see the same five frames, the last three with the same face.
    let frames = [0, 1, 2, 2, 2];
    for &n in &frames[..3] {
        stage.tick(&mut renderer, &expression(n)).unwrap();
    }
    let before = drawn.load(Ordering::Relaxed);
    stage.tick(&mut renderer, &expression(frames[3])).unwrap();
    assert_eq!(drawn.load(Ordering::Relaxed), before, "an unchanged face is not painted");
    // As after the app rebuilt the renderer: every layer is painted again, by a rasteriser that starts over, although
    // the face did not change.
    stage.face_device_lost();
    stage.tick(&mut renderer, &expression(frames[4])).unwrap();
    let layers = stage.character().face_layers().count();
    assert_eq!(drawn.load(Ordering::Relaxed), before + layers, "every face layer was painted again");
    let picture = rgba(&renderer.read_target(stage.target()).unwrap());
    for &n in &frames {
        reference.tick(&mut renderer, &expression(n)).unwrap();
    }
    let expected = rgba(&renderer.read_target(reference.target()).unwrap());
    let report = compare_region(&picture, &expected, Rect::full(SIZE, SIZE), T_FACE).unwrap();
    assert!(report.passes(T_FACE_MIN_PSNR_DB), "the repainted face differs from the CPU path: {}", report.stats);
    for stage in [stage, reference] {
        stage.release(&mut renderer).unwrap();
    }
}
