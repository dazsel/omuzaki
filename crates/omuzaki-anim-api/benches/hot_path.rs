//! Benchmarks for the per-frame operations the contract itself implements (the budgets are spec §10: the whole CPU
//! side is under 3% of an 8-core at 1080p30). Run with `cargo bench -p omuzaki-anim-api`.

#![allow(clippy::cast_precision_loss)] // loop counters used as test data

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{
    BlendMode, BoneMask, Bus, ChannelBuf, ChannelMask, ChannelTrack, Ease, HumanoidBone, HumanoidPose, Key, Layer,
    LayerStack, PlayoutDelay, Pose, PoseLayer, PoseLayers, PoseMode, Score, Skeleton, Transform,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_core::time::Seconds;

fn full_layer(mode: BlendMode, weight: f32) -> Layer {
    let mut layer = Layer::absent(mode);
    for (i, info) in omuzaki_anim_api::CHANNELS.iter().enumerate() {
        layer.values[info.channel] = (i % 10) as f32 / 10.0;
    }
    layer.mask = ChannelMask::ALL;
    layer.weight = weight;
    layer
}

fn blend(c: &mut Criterion) {
    let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
    stack.score = full_layer(BlendMode::Override, 1.0);
    stack.tracker = full_layer(BlendMode::Override, 0.8);
    stack.audio = full_layer(BlendMode::Max, 1.0);
    stack.trigger = full_layer(BlendMode::Add, 0.3);
    let mut bus = Bus::default();
    c.bench_function("resolve: 4 layers, all channels", |b| b.iter(|| black_box(&stack).resolve(black_box(&mut bus))));
    c.bench_function("sanitize", |b| b.iter(|| black_box(&mut bus.values).sanitize()));
    c.bench_function("mirror", |b| b.iter(|| black_box(&mut bus.values).mirror()));
}

/// The body pose side: layering onto a seeded base. "copy base" is the whole cost of a frame with no layers, the
/// "before" a layered frame is compared with.
fn pose_layers(c: &mut Criterion) {
    let (mut bind, mut other) = (HumanoidPose::REST, HumanoidPose::REST);
    for (i, bone) in HumanoidBone::ALL.into_iter().enumerate() {
        bind.set(bone, Quat::from_rotation_z(0.01 * i as f32));
        other.set(bone, Quat::from_rotation_x(0.02 * i as f32));
    }
    let arms = BoneMask::of(&[HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm]);
    let layer = |mask, weight, mode| PoseLayer { pose: other, mask, weight, mode };
    let life = [layer(BoneMask::ALL, 1.0, PoseMode::Add)];
    let stack = [
        layer(BoneMask::ALL, 1.0, PoseMode::Add),
        layer(BoneMask::ALL, 0.5, PoseMode::Override),
        layer(arms, 1.0, PoseMode::Override),
    ];
    // Every iteration starts from the base pose, as a frame does (`Animator::animate` gets `out` seeded with the bind).
    // Layering onto the previous iteration's result instead drives repeated slerps toward one target, whose differences
    // decay into denormals and slow each slerp ~10x: a benchmark artefact, not a cost a frame ever pays.
    let mut out = bind;
    c.bench_function("pose: seed only (copy base)", |b| b.iter(|| out = black_box(bind)));
    c.bench_function("pose: seed + resolve, no layers", |b| {
        b.iter(|| {
            out = black_box(bind);
            PoseLayers(&[]).resolve(&mut out);
        });
    });
    c.bench_function("pose: seed + life (add, all bones)", |b| {
        b.iter(|| {
            out = black_box(bind);
            PoseLayers(black_box(&life)).resolve(&mut out);
        });
    });
    c.bench_function("pose: seed + add + override 0.5 + arm clip", |b| {
        b.iter(|| {
            out = black_box(bind);
            PoseLayers(black_box(&stack)).resolve(&mut out);
        });
    });
}

fn score(c: &mut Criterion) {
    let tracks = omuzaki_anim_api::CHANNELS
        .iter()
        .take(40)
        .map(|info| ChannelTrack {
            channel: info.channel,
            initial: 0.0,
            keys: (0..200)
                .map(|i| Key {
                    time: Seconds(f64::from(i) * 0.1),
                    value: (i % 5) as f32 / 5.0,
                    over: 0.08,
                    ease: Ease::OutCubic,
                })
                .collect(),
        })
        .collect();
    let Ok(compiled) = (Score { duration: Seconds(20.0), tracks, cues: vec![] }).compile() else {
        panic!("the bench score is valid");
    };
    let mut layer = Layer::absent(BlendMode::Override);
    let mut t = 0.0;
    c.bench_function("score sample: 40 tracks x 200 keys", |b| {
        b.iter(|| {
            t = (t + 0.0333) % 20.0;
            compiled.sample(Seconds(black_box(t)), &mut layer);
        });
    });
}

fn rig(c: &mut Criterion) {
    let mut pose = HumanoidPose::REST;
    for bone in HumanoidBone::ALL {
        pose.set(bone, Quat::from_rotation_y(0.1));
    }
    let other = HumanoidPose::REST;
    c.bench_function("humanoid pose lerp (55 slerps)", |b| b.iter(|| black_box(&pose).lerp(black_box(&other), 0.3)));

    let Ok(skeleton) = Skeleton::humanoid(&HumanoidBone::ALL, |_| Transform::from_translation(Vec3::Y * 0.1)) else {
        panic!("the bench skeleton is valid");
    };
    let local = skeleton.rest_pose();
    let mut world = Pose::IDENTITY;
    c.bench_function("forward kinematics (55 bones)", |b| {
        b.iter(|| skeleton.world_pose(black_box(&local), &mut world));
    });
    let inverse_bind = skeleton.inverse_bind();
    let mut skin = vec![Mat4::IDENTITY; skeleton.len()];
    c.bench_function("skinning matrices (55 bones)", |b| {
        b.iter(|| skeleton.skin_matrices(black_box(&local), &inverse_bind, &mut world, &mut skin));
    });
}

/// The A/V delay: a full 64-slot ring read at a moving `now - 200 ms`, and a push into a full ring (which also
/// drops the oldest), each against the 50 ns budget.
fn playout(c: &mut Criterion) {
    const STEP: f64 = 1.0 / 120.0;
    let layer = full_layer(BlendMode::Override, 1.0);
    let mut ring = PlayoutDelay::<64>::new();
    let mut t = 0.0;
    for _ in 0..64 {
        t += STEP;
        let _ = ring.push(Seconds(t), layer);
    }
    c.bench_function("playout: push (full ring)", |b| {
        b.iter(|| {
            t += STEP;
            let _ = ring.push(Seconds(black_box(t)), black_box(layer));
        });
    });
    // Between two stamps, so the interpolation runs (the most common case: a frame rarely lands on a stamp).
    let mut now = t;
    c.bench_function("playout: sample (interpolating, 200 ms back)", |b| {
        b.iter(|| {
            now = t - 0.0123;
            black_box(ring.sample(Seconds(black_box(now)), Seconds(0.2)))
        });
    });
    c.bench_function("playout: sample (exact stamp)", |b| {
        b.iter(|| black_box(ring.sample(Seconds(black_box(t)), Seconds(0.0))));
    });
}

criterion_group!(benches, blend, pose_layers, score, rig, playout);
criterion_main!(benches);
