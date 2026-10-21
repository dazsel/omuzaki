//! Benchmark: `ClothRuntime` step plus mesh rewrite (#197, #706, #198). A skirt (48 x 32 render grid on a 24 x 16
//! sheet, 5 leg and hip colliders) walking, stepped as 30 fps frames and as single 1/120 s physics steps; the
//! skirt-plus-robe class (two runtimes, about 800 particles with the robe on the skirt's shell); each level of detail
//! forced; and Offline quality.
//!
//! Before the criterion cases the bench prints p50/p95 over a 2,000-frame replay after a warm-up (run it pinned:
//! `taskset -c 4 cargo bench -p omuzaki-char --bench cloth_runtime`).

#![allow(clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::f32::consts::TAU;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::cloth::collide::{ColliderKind, ColliderSet};
use omuzaki_char::cloth::runtime::{ClothGrid, ClothRuntime, ClothSpec, SkinWeights};
use omuzaki_char_api::{JointRef, PartRuntime, PoseSource, Quality, StepCtx};
use omuzaki_core::glam::{Affine3A, Mat4, Quat, Vec3};
use omuzaki_core::time::Seconds;

const JOINTS: [HumanoidBone; 4] =
    [HumanoidBone::Hips, HumanoidBone::LeftUpperLeg, HumanoidBone::RightUpperLeg, HumanoidBone::Spine];
const HIPS: Vec3 = Vec3::new(0.0, 1.0, 0.0);

/// A walk: hip flexion ±35° in opposition, a little hips yaw.
#[derive(Copy, Clone)]
struct Walk(f32);

impl Walk {
    fn matrix(self, i: usize) -> Affine3A {
        let s = (TAU * self.0).sin();
        let about =
            |p: Vec3, q: Quat| Affine3A::from_translation(p) * Affine3A::from_quat(q) * Affine3A::from_translation(-p);
        let hips = about(HIPS, Quat::from_rotation_y(0.08 * s));
        match i {
            1 => hips * about(Vec3::new(0.09, 0.95, 0.0), Quat::from_rotation_x(-0.6 * s)),
            2 => hips * about(Vec3::new(-0.09, 0.95, 0.0), Quat::from_rotation_x(0.6 * s)),
            _ => hips,
        }
    }
}

impl PoseSource for Walk {
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4> {
        let JointRef::Humanoid(b) = joint else { return None };
        JOINTS.iter().position(|x| x == b).map(|i| Mat4::from(self.matrix(i)))
    }

    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

/// A wrapped garment grid from a waist ring down to a hem ring, the lower front weighted to the thighs.
fn tube(y0: f32, r0: f32, y1: f32, r1: f32) -> ClothGrid {
    let (rows, cols) = (32, 48);
    let mut positions = Vec::new();
    let mut skin = Vec::new();
    for r in 0..rows {
        let t = r as f32 / (rows - 1) as f32;
        for c in 0..cols {
            let a = TAU * c as f32 / cols as f32;
            let radius = r0 + (r1 - r0) * t;
            positions.push(Vec3::new(radius * a.sin(), y0 + (y1 - y0) * t, radius * a.cos()));
            let leg = 0.8 * (t / 0.4).min(1.0) * (0.6 + 0.4 * a.cos());
            let left = (0.5 + a.sin() / 0.6).clamp(0.0, 1.0);
            skin.push(SkinWeights { joints: [0, 1, 2, 0], weights: [1.0 - leg, leg * left, leg * (1.0 - left), 0.0] });
        }
    }
    ClothGrid {
        rows,
        cols,
        wrap: true,
        positions,
        skin,
        joints: JOINTS.iter().map(|&b| JointRef::Humanoid(b)).collect(),
    }
}

fn legs() -> ColliderSet {
    let mut set = ColliderSet::new();
    let j = JointRef::Humanoid;
    set.add_capsule(
        Vec3::new(0.09, 0.95, 0.0),
        Vec3::new(0.09, 0.52, 0.0),
        0.065,
        &j(HumanoidBone::LeftUpperLeg),
        ColliderKind::Body,
    );
    set.add_capsule(
        Vec3::new(-0.09, 0.95, 0.0),
        Vec3::new(-0.09, 0.52, 0.0),
        0.065,
        &j(HumanoidBone::RightUpperLeg),
        ColliderKind::Body,
    );
    set.add_capsule(
        Vec3::new(0.04, 0.98, 0.0),
        Vec3::new(-0.04, 0.98, 0.0),
        0.1,
        &j(HumanoidBone::Hips),
        ColliderKind::Body,
    );
    set
}

fn skirt_grid() -> ClothGrid {
    tube(1.02, 0.15, 0.58, 0.24)
}

fn runtime(spec: &ClothSpec) -> ClothRuntime {
    ClothRuntime::new(&skirt_grid(), spec, legs()).expect("a valid skirt")
}

/// The robe over the skirt: the legs grown by the skirt's layer, and the skirt's shell.
fn robe() -> ClothRuntime {
    let mut set = ColliderSet::new();
    for s in legs().shapes() {
        let joint = JointRef::Humanoid(JOINTS[s.joint().map_or(0, |j| j.min(3))]);
        set.add_capsule(s.a, s.b, s.radius + 0.012, &joint, ColliderKind::Body);
    }
    set.add_shell(&skirt_grid(), 8, 16, 0.012, 0.03).expect("a valid shell");
    ClothRuntime::new(&tube(1.04, 0.18, 0.4, 0.29), &ClothSpec::default(), set).expect("a valid robe")
}

/// Steps the runtimes one frame of `dt` at walk time `t`.
fn frame(rts: &mut [ClothRuntime], t: f32, dt: f32) {
    let pose = Walk(t);
    let ctx = StepCtx::new(dt, Seconds(f64::from(t)), &pose, Vec3::new(1.0, 0.0, 0.5));
    for rt in rts {
        rt.step(&ctx);
    }
}

/// Prints p50 and p95 of a frame of `dt` over a replay.
fn report(name: &str, rts: &mut [ClothRuntime], dt: f32) {
    let mut t = 0.0;
    for _ in 0..300 {
        t += dt;
        frame(rts, t, dt);
    }
    let mut times: Vec<f64> = (0..2000)
        .map(|_| {
            t += dt;
            let start = Instant::now();
            frame(rts, t, dt);
            start.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    let particles: u32 = rts.iter().map(|r| r.cost().particles).sum();
    println!("cloth runtime[{name}, {particles} particles]: p50 {:.0} us, p95 {:.0} us", at(0.5), at(0.95));
}

fn case(c: &mut Criterion, name: &str, mut rts: Vec<ClothRuntime>, dt: f32) {
    report(name, &mut rts, dt);
    let mut t = 0.0;
    c.bench_function(name, |b| {
        b.iter(|| {
            t += dt;
            frame(&mut rts, t, dt);
        });
    });
}

fn bench(c: &mut Criterion) {
    let live = ClothSpec::default();
    case(c, "skirt, 1/120 s step", vec![runtime(&live)], 1.0 / 120.0);
    case(c, "skirt, 30 fps frame", vec![runtime(&live)], 1.0 / 30.0);
    case(c, "skirt + robe, 1/120 s step", vec![runtime(&live), robe()], 1.0 / 120.0);
    case(c, "skirt + robe, 30 fps frame", vec![runtime(&live), robe()], 1.0 / 30.0);
    for level in 0..3 {
        let mut rt = runtime(&live);
        rt.force_level(Some(level));
        let size = rt.binding(level).map(omuzaki_char::cloth::runtime::ClothBinding::size).expect("three levels");
        case(c, &format!("skirt level {level} ({}x{}), 30 fps frame", size.cols, size.rows), vec![rt], 1.0 / 30.0);
    }
    let offline = ClothSpec { quality: Quality::Offline, ..ClothSpec::default() };
    case(c, "skirt Offline, 30 fps frame", vec![runtime(&offline)], 1.0 / 30.0);
}

criterion_group!(benches, bench);
criterion_main!(benches);
