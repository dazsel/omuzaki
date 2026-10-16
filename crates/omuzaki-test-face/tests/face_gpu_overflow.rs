//! A scene that outgrows Vello's buffers (#2074): the GPU rasteriser's bump-allocated buffers (lines, bins, tiles,
//! segments, blend spill, command lists) have fixed sizes, and a repaint that needs more draws nothing. The rasteriser
//! copies Vello's counters out after every repaint, reads them back without waiting once the GPU has run that repaint,
//! and on a failure grows the buffers and repaints the layer whole. Forced here with buffers far too small for the real
//! face, at probe size. Without an adapter the test prints why and passes.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{cpu_pixels, device, image, native, read_native, submit};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{GpuRasterizer, RealPainter};
use omuzaki_face_api::{FaceDrawing, FaceInput, FaceLayerId, FaceMapping, FacePainter, FaceView};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, compare_region};

const RULE: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.01 };
const MIN_PSNR_DB: f64 = 34.0;
/// The layer is about this many pixels wide: probe size.
const PROBE_WIDTH_PX: f32 = 256.0;
/// Entries every bump-allocated buffer starts with: the real face needs thousands of lines and command words.
const TINY: u32 = 64;

#[test]
fn a_face_that_outgrows_vellos_buffers_is_drawn_right_on_the_paint_after_its_counters_come_back() {
    let Some(gpu) = device() else { return };
    let mut painter = RealPainter::new(mock_projection(), PROBE_WIDTH_PX / mock_projection().rect.width());
    let spec = *painter.layers().iter().find(|layer| layer.id == FaceLayerId::Base).expect("a Base layer");
    let (profile, params) = (mock_profile(), FaceMapping::default().rest_params());
    let mut drawing = FaceDrawing::new();
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint(&input, &mut drawing).unwrap();
    let list = &drawing[FaceLayerId::Base];
    let (size, expected) = (spec.size_px(), cpu_pixels(&spec, list));
    let mut rasterizer = VelloRasterizer::new().with_bump_capacity(TINY);
    let mut native = native(&gpu, size);

    // The first repaint outgrows the buffers: Vello's stages give up and nothing reaches the texture.
    assert!(rasterizer.rasterize_native(&spec, list, &mut native).unwrap(), "the rasteriser draws on this device");
    let first = read_native(&mut native);
    assert!(first.chunks(4).all(|p| p[3] == 0), "the overflowing repaint drew something, or this test aims at nothing");

    // The same face again: nothing changed, so nothing is drawn, but the first repaint's counters are asked for, and
    // they come back while the device is polled.
    assert!(rasterizer.rasterize_native(&spec, list, &mut native).unwrap());
    submit(&mut native);
    gpu.0.poll(wgpu::PollType::wait_indefinitely()).unwrap();

    // This paint reads them: the buffers grow, and the layer is drawn whole although the face did not change.
    assert!(rasterizer.rasterize_native(&spec, list, &mut native).unwrap());
    let drawn = read_native(&mut native);
    let report =
        compare_region(&image(size, &drawn), &image(size, &expected), Rect::full(size.0, size.1), RULE).unwrap();
    eprintln!("after the buffers grew, the face against the CPU raster: {}", report.stats);
    assert!(report.passes(MIN_PSNR_DB), "the repaint after the overflow differs from the CPU raster: {}", report.stats);
}
