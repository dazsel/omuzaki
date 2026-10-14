//! Benchmark for the life layer's per-frame cost (spec §10: the whole CPU side is under 3% of an 8-core at 1080p30, so
//! a frame has over a millisecond to spend in total). Run with `cargo bench -p omuzaki-anim --bench life`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::life::Life;
use omuzaki_anim_api::{Bus, Channel, ChannelMask, Cues, LifeConfig, LifeLayer, Source, Tick};

/// An incoming bus with a smile, an open mouth and a turned head, as a busy frame has.
fn busy_bus() -> Bus {
    let mut bus = Bus::default();
    bus.values[Channel::MOUTH_SMILE_LEFT] = 0.4;
    bus.values[Channel::MOUTH_SMILE_RIGHT] = 0.4;
    bus.values[Channel::JAW_OPEN] = 0.3;
    bus.values[Channel::HEAD_YAW] = 0.2;
    bus
}

fn life_frame(c: &mut Criterion) {
    let talking = Cues { loudness: 0.6, onset: false, laugh: 0.0 };
    let mut life = Life::new(LifeConfig::new(1));
    let mut index = 0_u64;
    c.bench_function("life: one frame at 60 fps, talking", |b| {
        b.iter(|| {
            let mut bus = black_box(busy_bus());
            life.step(Tick::at_rate(index, 60.0), black_box(&talking), &mut bus);
            index += 1;
            black_box(bus);
        });
    });

    // Every region driven by a person: the springs only snap, so this is the floor.
    let mut life = Life::new(LifeConfig::new(1));
    c.bench_function("life: one frame, a person drives everything", |b| {
        b.iter(|| {
            let mut bus = black_box(busy_bus());
            bus.owners.claim(Source::Tracker, ChannelMask::ALL);
            life.step(Tick::at_rate(index, 60.0), black_box(&talking), &mut bus);
            index += 1;
            black_box(bus);
        });
    });
}

criterion_group!(benches, life_frame);
criterion_main!(benches);
