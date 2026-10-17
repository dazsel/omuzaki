//! A scene that outgrows Vello's buffers (#2074): the GPU rasteriser's bump-allocated buffers (lines, bins, tiles,
//! segment counts, segments, blend spill, command lists) have fixed sizes, and a repaint that needs more draws nothing
//! or draws wrong. The rasteriser copies Vello's counters out after every repaint, reads them back without waiting once
//! the GPU has run that repaint, compares every counter with the capacity the repaint had, and on an overflow grows the
//! buffers and has the layer repainted whole. `repaint_needed` is how the stage learns of it for a face that stopped
//! changing. Forced here with buffers far too small for the real face, at probe size. Without an adapter the tests print
//! why and pass.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Gpu, Native, cpu_pixels, device, image, native, read_native};
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::raster::gpu::{BumpCapacity, VelloRasterizer};
use omuzaki_face::{FaceSystem, GpuRasterizer, RealPainter};
use omuzaki_face_api::{DrawList, FaceLayerId, FaceLayerSpec, FaceView};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, compare_region};

const RULE: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.01 };
const MIN_PSNR_DB: f64 = 34.0;
/// The layer is about this many pixels wide: probe size.
const PROBE_WIDTH_PX: f32 = 256.0;
/// Entries a buffer starts with: the real face needs thousands of lines, segments and command words.
const TINY: u32 = 64;
const LAYER: FaceLayerId = FaceLayerId::Base;

/// The Base layer's spec and draw list for neutral channels with `changes` set, from the real painter at probe size.
fn base(changes: &[(Channel, f32)]) -> (FaceLayerSpec, DrawList) {
    let ppm = PROBE_WIDTH_PX / mock_projection().rect.width();
    let mut system = FaceSystem::real(RealPainter::new(mock_projection(), ppm), Box::new(SkiaRasterizer::new()));
    let mut channels = ChannelBuf::NEUTRAL;
    for &(channel, value) in changes {
        channels.set(channel, value);
    }
    system.update(&channels, &mock_profile(), Seconds(0.0), FaceView::default()).unwrap();
    let spec = system.layers().iter().find(|layer| layer.id == LAYER).copied().expect("a Base layer");
    (spec, system.drawing()[LAYER].clone())
}

/// What the stage does once a frame, twice around the GPU finishing everything submitted: asks whether the layer must
/// be repainted. The first ask maps the read-backs of the repaints submitted since the last one; they come back while
/// the device is polled (the renderer polls once a frame); the second reads them.
fn ask_after_the_gpu_ran(gpu: &Gpu, rasterizer: &mut VelloRasterizer) -> bool {
    let early = rasterizer.repaint_needed(LAYER);
    gpu.0.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    early | rasterizer.repaint_needed(LAYER)
}

/// Paints `list` and returns the texture, after submitting the recorded commands as the renderer does.
fn paint(rasterizer: &mut VelloRasterizer, spec: &FaceLayerSpec, list: &DrawList, native: &mut Native) -> Vec<u8> {
    assert!(rasterizer.rasterize_native(spec, list, native).unwrap(), "the rasteriser draws on this device");
    read_native(native)
}

/// Asserts `drawn` matches the CPU raster of `list`.
fn assert_matches_cpu(spec: &FaceLayerSpec, list: &DrawList, drawn: &[u8], what: &str) {
    let size = spec.size_px();
    let expected = cpu_pixels(spec, list);
    let report =
        compare_region(&image(size, drawn), &image(size, &expected), Rect::full(size.0, size.1), RULE).unwrap();
    eprintln!("{what}, against the CPU raster: {}", report.stats);
    assert!(report.passes(MIN_PSNR_DB), "{what} differs from the CPU raster: {}", report.stats);
}

#[test]
fn a_face_that_outgrows_vellos_buffers_is_repainted_whole_with_no_change_of_face() {
    let Some(gpu) = device() else { return };
    let (spec, list) = base(&[]);
    let mut rasterizer = VelloRasterizer::new().with_bump_capacity(BumpCapacity::uniform(TINY));
    let mut native = native(&gpu, spec.size_px());

    // The first repaint outgrows the buffers: Vello's stages give up and nothing reaches the texture.
    let first = paint(&mut rasterizer, &spec, &list, &mut native);
    assert!(first.chunks(4).all(|p| p[3] == 0), "the overflowing repaint drew something, or this test aims at nothing");

    // The face does not change, yet once the counters are back the rasteriser asks for the layer again ...
    assert!(ask_after_the_gpu_ran(&gpu, &mut rasterizer), "the overflow is found without another paint");
    // ... and the next paint, with grown buffers, draws it whole although its list is the same.
    let drawn = paint(&mut rasterizer, &spec, &list, &mut native);
    assert_matches_cpu(&spec, &list, &drawn, "after the buffers grew, the face");
    assert!(!ask_after_the_gpu_ran(&gpu, &mut rasterizer), "a repaint that fits asks for nothing more");
}

#[test]
fn segments_that_run_out_with_nothing_failed_are_found_and_grown() {
    // Vello's coarse stage reserves segments with no check against their buffer, so running out of segments sets no
    // failed flag: only the counter, compared with the capacity, shows it. Every other buffer has Vello's size.
    let Some(gpu) = device() else { return };
    let (spec, list) = base(&[]);

    let mut roomy = VelloRasterizer::new();
    let mut native_roomy = native(&gpu, spec.size_px());
    paint(&mut roomy, &spec, &list, &mut native_roomy);
    assert!(!ask_after_the_gpu_ran(&gpu, &mut roomy), "with Vello's sizes the face fits and nothing is asked");

    let segments = BumpCapacity { segments: TINY, ..BumpCapacity::vello() };
    let mut rasterizer = VelloRasterizer::new().with_bump_capacity(segments);
    let mut native = native(&gpu, spec.size_px());
    paint(&mut rasterizer, &spec, &list, &mut native);
    assert!(ask_after_the_gpu_ran(&gpu, &mut rasterizer), "the segments ran out and the rasteriser did not notice");
    let drawn = paint(&mut rasterizer, &spec, &list, &mut native);
    assert_matches_cpu(&spec, &list, &drawn, "after the segments grew, the face");
    assert!(!ask_after_the_gpu_ran(&gpu, &mut rasterizer), "the grown segments hold the face");
}

/// Paints `count` repaints that alternate between two faces without the GPU running any of them: their commands are
/// dropped unsubmitted and the device is not polled, so every read-back recorded stays waiting, as behind a GPU that
/// many repaints late.
fn paint_behind(rasterizer: &mut VelloRasterizer, gpu: &Gpu, native: &mut Native, count: usize) -> FaceLayerSpec {
    let faces = [base(&[]), base(&[(Channel::JAW_OPEN, 0.6)])];
    for at in 0..count {
        let (spec, list) = &faces[at % 2];
        assert!(rasterizer.rasterize_native(spec, list, native).unwrap());
        native.3 = gpu.0.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    }
    faces[0].0
}

#[test]
fn a_gpu_many_repaints_behind_gets_every_repaint_checked_and_no_extra_repaint() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let mut native = native(&gpu, base(&[]).0.size_px());
    paint_behind(&mut rasterizer, &gpu, &mut native, 12);
    assert!(!ask_after_the_gpu_ran(&gpu, &mut rasterizer), "twelve repaints in flight each had a read-back");
}

#[test]
fn a_repaint_past_the_last_read_back_is_repainted_once_one_is_free() {
    // More repaints in flight than a layer keeps read-backs for (128): the last ones go unchecked.
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let mut native = native(&gpu, base(&[]).0.size_px());
    let spec = paint_behind(&mut rasterizer, &gpu, &mut native, 130);
    assert!(ask_after_the_gpu_ran(&gpu, &mut rasterizer), "the unchecked repaint is asked for again");
    let (_, list) = base(&[(Channel::JAW_OPEN, 0.6)]);
    let drawn = paint(&mut rasterizer, &spec, &list, &mut native);
    assert_matches_cpu(&spec, &list, &drawn, "the repaint after the unchecked one");
    assert!(!ask_after_the_gpu_ran(&gpu, &mut rasterizer), "that repaint was checked");
}
