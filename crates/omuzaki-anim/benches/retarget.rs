//! Benchmarks for the retarget's per-frame cost (spec §10: the whole CPU side is under 3% of an 8-core at 1080p30, so
//! the retarget has microseconds, not milliseconds). Run with `cargo bench -p omuzaki-anim`.

#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::retarget::{ArkitRetarget, default_calibration};
use omuzaki_anim_api::{BlendMode, DeviceProfile, Layer, Retarget, RetargetConfig};

/// Applies one frame per iteration, cycling through a track that has every channel, the head and a lost span.
fn bench_config(c: &mut Criterion, name: &str, config: RetargetConfig) {
    let frames = common::frames(240);
    let mut retarget = ArkitRetarget::with(default_calibration(DeviceProfile::Webcam), config);
    let mut out = Layer::absent(BlendMode::Override);
    let mut next = 0;
    c.bench_function(name, |b| {
        b.iter(|| {
            retarget.apply(black_box(&frames[next]), 1.0 / 30.0, &mut out);
            next = (next + 1) % frames.len();
            black_box(&out);
        });
    });
}

fn retarget(c: &mut Criterion) {
    bench_config(c, "apply: 52 channels + head, default settings", RetargetConfig::default());
    bench_config(
        c,
        "apply: mirrored, exaggerated 1.5",
        RetargetConfig { mirror: true, exaggeration: 1.5, ..RetargetConfig::default() },
    );
    bench_config(c, "apply: smoothing off", RetargetConfig { smoothing: 0.0, ..RetargetConfig::default() });
}

criterion_group!(benches, retarget);
criterion_main!(benches);
