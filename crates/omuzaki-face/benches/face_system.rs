//! Benchmark: one `FaceSystem::update`, the per-frame face path (spec v3 §10, face-paint budget 0.5 ms).
//!
//! The stub sections draw nothing, so the painter here is the mock one: it draws a whole (flat) face, which exercises
//! the system's mapping, fingerprinting and change detection on realistic lists. Three cases: an unchanged face (the
//! steady state, which must cost next to nothing), a face that changes every frame with a rasteriser that does nothing
//! (the system's own overhead plus the painter), and the same with the mock CPU rasteriser (a slow reference, not the
//! production one).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::time::Seconds;
use omuzaki_face::{FaceSystem, RealPainter};
use omuzaki_face_api::{DEFAULT_PX_PER_M, DrawList, FaceError, FaceLayerSpec, FaceRasterizer, FaceView, Raster};
use omuzaki_face_mock::{MockPainter, MockRasterizer, mock_profile, mock_projection};

/// Draws nothing, so a benchmark measures the system and the painter alone.
struct NullRasterizer;

impl FaceRasterizer for NullRasterizer {
    fn rasterize(&mut self, _: &FaceLayerSpec, _: &DrawList, _: f32, _: &mut Raster) -> Result<(), FaceError> {
        Ok(())
    }
}

fn blinking(value: f32) -> ChannelBuf {
    let mut buf = ChannelBuf::NEUTRAL;
    buf.set(Channel::EYE_BLINK_LEFT, value);
    buf
}

fn brow_raised(value: f32) -> ChannelBuf {
    let mut buf = ChannelBuf::NEUTRAL;
    buf.set(Channel::BROW_INNER_UP, value);
    buf
}

/// The real painter with a brow that moves every frame: the brows section repaints both layers each time.
fn bench_brows(c: &mut Criterion) {
    let profile = mock_profile();
    let (down, up) = (brow_raised(0.0), brow_raised(1.0));
    let painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let mut system = FaceSystem::new(Box::new(painter), Box::new(NullRasterizer));
    for buf in [&down, &up] {
        system.update(buf, &profile, Seconds(0.0), FaceView::default()).ok();
    }
    let mut frame = 0_u32;
    c.bench_function("face_system_update/real-painter-moving-brows-null-raster", |b| {
        b.iter(|| {
            frame += 1;
            let buf = if frame.is_multiple_of(2) { &up } else { &down };
            black_box(system.update(buf, &profile, Seconds(0.0), FaceView::default()))
        });
    });
}

fn bench_update(c: &mut Criterion) {
    let profile = mock_profile();
    let (open, shut) = (blinking(0.0), blinking(1.0));
    let mut group = c.benchmark_group("face_system_update");

    let mut run = |name: &str, mut system: FaceSystem, alternate: bool| {
        // Warm up both poses so the draw lists are at working size before measuring.
        for buf in [&open, &shut] {
            system.update(buf, &profile, Seconds(0.0), FaceView::default()).ok();
        }
        let mut frame = 0_u32;
        group.bench_function(name, |b| {
            b.iter(|| {
                frame += 1;
                let buf = if alternate && frame.is_multiple_of(2) { &shut } else { &open };
                black_box(system.update(buf, &profile, Seconds(0.0), FaceView::default()))
            });
        });
    };

    let real = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    run("stub-real-painter-unchanged", FaceSystem::new(Box::new(real), Box::new(NullRasterizer)), false);
    let mock = || Box::new(MockPainter::default());
    run("mock-painter-unchanged", FaceSystem::new(mock(), Box::new(NullRasterizer)), false);
    run("mock-painter-changing-null-raster", FaceSystem::new(mock(), Box::new(NullRasterizer)), true);
    run("mock-painter-changing-mock-raster", FaceSystem::new(mock(), Box::new(MockRasterizer)), true);
    group.finish();
}

criterion_group!(benches, bench_update, bench_brows);
criterion_main!(benches);
