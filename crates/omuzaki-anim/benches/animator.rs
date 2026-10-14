//! Benchmark for the bus animator's per-frame cost. Run with `cargo bench -p omuzaki-anim --bench animator`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::BusAnimator;
use omuzaki_anim_api::{Animator, Bus, Channel, HumanoidPose, Tick};

fn animator_frame(c: &mut Criterion) {
    let mut animator = BusAnimator::default();
    let mut bus = Bus::default();
    bus.values[Channel::HEAD_YAW] = 0.4;
    bus.values[Channel::HEAD_PITCH] = -0.2;
    bus.values[Channel::LEAN] = 0.1;
    bus.values[Channel::BREATH] = 0.7;
    bus.values[Channel::BODY_TURN] = 0.3;
    let base = HumanoidPose::REST;
    let mut index = 0_u64;
    c.bench_function("animator: one frame at 60 fps, head, lean, breath and turn", |b| {
        b.iter(|| {
            let mut out = base;
            animator.animate(Tick::at_rate(index, 60.0), black_box(&bus), &[], &mut out);
            index += 1;
            black_box(out);
        });
    });
}

criterion_group!(benches, animator_frame);
criterion_main!(benches);
