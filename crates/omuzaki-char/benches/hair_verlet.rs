//! Benchmark: the hair Verlet core on Mira-class hair (44 head locks of 7 particles and 10 body locks of 13 down the
//! back, 438 particles, 5 sphere colliders, default physics: 90 Hz, 2 iterations), one frame being `targets` plus
//! `step`, with the head swinging so the hair never settles. Budget: <= 0.25 ms per frame (spec §10 gives all physics
//! 0.5 ms). Frames of 1/90 s (one fixed step), 1/60 s and 1/30 s (three steps). Run with
//! `cargo bench -p omuzaki-char --bench hair_verlet`.

#![allow(clippy::expect_used, clippy::cast_precision_loss)] // a set that fails to build should stop the run; small counts

use std::f32::consts::TAU;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_char::hair::physics::verlet::ChainSet;
use omuzaki_char_api::{
    Bulge, Falloff, Follow, GroupId, HairClump, HairPhysics, JointRef, PoseSource, SphereCollider, Taper,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3};

const HEAD: Vec3 = Vec3::new(0.0, 0.778, 0.0);

/// The head turned by `yaw` about its centre; every other joint at rest.
struct Swing(Mat4);

impl PoseSource for Swing {
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4> {
        Some(if *joint == JointRef::HEAD { self.0 } else { Mat4::IDENTITY })
    }
    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

fn lock(a: f32, points: usize, follow: Follow) -> HairClump {
    let out = Vec3::new(a.sin(), 0.0, a.cos());
    HairClump {
        group: GroupId::new("bench").expect("a valid id"),
        rest: (0..points).map(|i| HEAD + out * 0.1 + Vec3::new(0.0, 0.06 - 0.02 * i as f32, 0.0)).collect(),
        width: Taper::default(),
        thickness: Bulge::default(),
        stiffness: Falloff::default(),
        shade: 1.0,
        simulate: true,
        follow,
        tint: None,
    }
}

fn sphere(bone: JointRef, centre: [f64; 3], radius: f64) -> SphereCollider {
    SphereCollider { bone, centre, radius }
}

fn frames(c: &mut Criterion) {
    let mut clumps: Vec<HairClump> = (0..44).map(|n| lock(TAU * n as f32 / 44.0, 13, Follow::Head)).collect();
    clumps.extend((0..10).map(|n| lock(2.4 + 0.15 * n as f32, 25, Follow::Body)));
    let colliders = vec![
        sphere(JointRef::HEAD, [0.0, 0.778, 0.0], 0.09),
        sphere(JointRef::NECK, [0.0, 0.66, -0.01], 0.045),
        sphere(JointRef::CHEST, [0.0, 0.52, -0.02], 0.11),
        sphere(JointRef::CHEST, [0.12, 0.6, -0.02], 0.06),
        sphere(JointRef::CHEST, [-0.12, 0.6, -0.02], 0.06),
    ];
    let physics = HairPhysics { colliders, ..HairPhysics::default() };
    for (name, dt) in [
        ("hair_verlet/frame_90hz", 1.0 / 90.0),
        ("hair_verlet/frame_60fps", 1.0 / 60.0),
        ("hair_verlet/frame_30fps", 1.0 / 30.0),
    ] {
        let mut set = ChainSet::from_clumps(&clumps, &physics, &[]).expect("valid hair");
        set.reset(&Swing(Mat4::IDENTITY)).expect("a full pose");
        set.settle(480);
        let mut frame = 0_u32;
        c.bench_function(name, |b| {
            b.iter(|| {
                frame = frame.wrapping_add(1);
                // Swing the head +-30 degrees at 1 Hz.
                let yaw = 0.52 * (frame as f32 * dt * TAU).sin();
                let head = Mat4::from_translation(HEAD)
                    * Mat4::from_quat(Quat::from_rotation_y(yaw))
                    * Mat4::from_translation(-HEAD);
                set.targets(&Swing(head)).expect("a full pose");
                set.step(black_box(dt)).expect("a valid frame time")
            });
        });
    }
}

criterion_group!(benches, frames);
criterion_main!(benches);
