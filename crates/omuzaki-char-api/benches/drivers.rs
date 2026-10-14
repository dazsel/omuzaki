//! Per-frame cost of evaluating bone drivers: 0, 10 and 100 drivers, channel-driven and bone-following.
//! Run with `cargo bench -p omuzaki-char-api --bench drivers`.

#![allow(clippy::cast_precision_loss, clippy::unwrap_used, clippy::expect_used)] // bench setup, loop counters as data

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{BoneDef, BoneId, Channel, ChannelBuf, Skeleton, Transform};
use omuzaki_char_api::{BoneDriver, DriverPlan};
use omuzaki_core::glam::{Quat, Vec3};

/// A root, then `n` extra bones: even ones driven by a custom channel, odd ones following the root.
fn fixture(n: usize) -> (Skeleton, Vec<BoneDriver>) {
    let bone = |name: String, parent| BoneDef {
        name,
        parent,
        rest: Transform::from_translation(Vec3::Y * 0.1),
        humanoid: None,
    };
    let mut bones = vec![bone("root".to_owned(), None)];
    bones.extend((0..n).map(|i| bone(format!("wing_{i}"), Some(BoneId(0)))));
    let skeleton = Skeleton::generic(bones).expect("valid skeleton");
    let spread = Transform { translation: Vec3::new(0.2, 0.0, 0.0), rotation: Quat::from_rotation_z(1.2) };
    let drivers = (0..n)
        .map(|i| {
            let id = BoneId(u16::try_from(i + 1).expect("small"));
            if i % 2 == 0 {
                let channel = Channel::custom(i % 16).expect("custom slot");
                BoneDriver::channel(id, channel, Transform::IDENTITY, spread)
            } else {
                BoneDriver::follow(id, BoneId(0), 0.5)
            }
        })
        .collect();
    (skeleton, drivers)
}

fn apply(c: &mut Criterion) {
    let mut channels = ChannelBuf::NEUTRAL;
    for n in 0..16 {
        channels.set(Channel::custom(n).expect("custom slot"), n as f32 / 15.0);
    }
    for count in [0, 10, 100] {
        let (skeleton, drivers) = fixture(count);
        let plan = DriverPlan::compile(&skeleton, &drivers).expect("valid drivers");
        let mut pose = skeleton.rest_pose();
        pose.local[0].rotation = Quat::from_rotation_x(0.7);
        c.bench_function(&format!("apply: {count} drivers"), |b| {
            b.iter(|| black_box(&plan).apply(black_box(&channels), black_box(&mut pose)));
        });
    }
}

criterion_group!(benches, apply);
criterion_main!(benches);
