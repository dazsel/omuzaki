//! Benchmark: the emission log's per-frame work (particles.md D8 budgets the stage fill at 0.02 ms on `R`): `fill` of
//! a full ring of 1,024 batches into the scene, and a frame of advance, prune and fill with a weather stream and bursts.

// Bench helpers may panic: a panic is a clear failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::glam::{Mat4, Vec2};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::limits::MAX_SPAWN_BATCHES;
use omuzaki_render_api::{Colour, EmitterShape, Orientation, ParticleSystemDesc, Renderer, Span, Sprite, SpriteShape};
use omuzaki_render_mock::MockRenderer;
use omuzaki_render_mock::particle_sheet::still;
use omuzaki_stage::effects::log::{EffectLimits, EmissionLog, Spawn, StreamKey};

fn bench(c: &mut Criterion) {
    let mut renderer = MockRenderer::new();
    let desc = ParticleSystemDesc {
        sprite: Sprite::Shape(SpriteShape::Disc),
        params: still(0.02, Orientation::billboard(Span::fixed(0.0)), Colour::WHITE),
    };
    let system = renderer.create_particle_system(&desc).unwrap();
    let spawn = Spawn::new(system, Mat4::IDENTITY);

    let mut full = EmissionLog::new(EffectLimits::offline());
    for i in 0..MAX_SPAWN_BATCHES {
        full.burst(Seconds(0.0), &Spawn { seed: u32::try_from(i).unwrap(), ..spawn }, 10, 60.0);
    }
    assert_eq!(full.batches().len(), MAX_SPAWN_BATCHES);
    let mut out = Vec::with_capacity(MAX_SPAWN_BATCHES);
    c.bench_function("effects_log/fill_1024", |b| {
        b.iter(|| {
            out.clear();
            black_box(&full).fill(&mut out);
            black_box(out.len())
        });
    });

    let mut log = EmissionLog::new(EffectLimits::live());
    let snow = Spawn { shape: EmitterShape::rect(Vec2::new(2.0, 1.0)), ..spawn };
    let mut frame = 0u32;
    c.bench_function("effects_log/frame_weather_and_bursts", |b| {
        b.iter(|| {
            frame += 1;
            let t = Seconds(f64::from(frame) / 60.0);
            log.stream(StreamKey(0), t, &snow, 400.0, 8.0, true);
            if frame.is_multiple_of(30) {
                log.burst(t, &Spawn { seed: frame, ..spawn }, 150, 4.0);
            }
            log.prune(t);
            out.clear();
            log.fill(&mut out);
            black_box(out.len())
        });
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
