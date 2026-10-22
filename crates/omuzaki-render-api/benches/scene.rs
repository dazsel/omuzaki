//! The contract's own per-frame cost: rebuilding a busy scene and validating it. Every renderer pays this each frame,
//! so it must stay a rounding error next to the 1.5 ms GPU budget (spec §10).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    Camera, Colour, EmitterShape, Instance, JointRange, Light, MaterialId, MeshId, ParticleSystemId, Scene, SpawnBatch,
};

/// A character with a 128-joint skeleton, 200 props and a full light rig.
fn fill(scene: &mut Scene) {
    let (mesh, material) = (MeshId::from_parts(0, 1), MaterialId::from_parts(0, 1));
    scene.lights.extend((0..8u8).map(|i| Light::Point {
        position: Vec3::splat(f32::from(i)),
        colour: Colour::WHITE,
        intensity: 1.0,
        range: 5.0,
    }));
    scene.joints.extend((0..128u8).map(|j| Mat4::from_translation(Vec3::new(f32::from(j), 0.0, 0.0))));
    for i in 0..200u8 {
        let mut instance = Instance::new(mesh, material, Mat4::from_translation(Vec3::splat(f32::from(i))));
        if i == 0 {
            instance.skin = Some(JointRange { start: 0, len: 128 });
        }
        scene.instances.push(instance);
    }
}

fn scene_benches(c: &mut Criterion) {
    let mut scene = Scene::new(Camera::default());
    fill(&mut scene);
    c.bench_function("scene_validate_200_instances_128_joints", |b| b.iter(|| black_box(&scene).validate()));
    c.bench_function("scene_clear_and_refill", |b| {
        b.iter(|| {
            scene.clear();
            fill(&mut scene);
            black_box(&scene);
        });
    });
}

/// The most spawn batches a scene may hold (particles.md D2), 19 slots each: 19,456 slots, under the 20,000 cap.
fn fill_particles(scene: &mut Scene) {
    let system = ParticleSystemId::from_parts(0, 1);
    scene.particles.extend((0..1024u16).map(|i| SpawnBatch {
        shape: EmitterShape::sphere(0.1),
        start: Seconds(f64::from(i) * 0.01),
        count: 19,
        seed: u32::from(i),
        ..SpawnBatch::new(system, Mat4::from_translation(Vec3::splat(f32::from(i))))
    }));
}

/// The #755 target: validating 1,024 batches takes at most 5 µs.
fn particle_benches(c: &mut Criterion) {
    let mut scene = Scene::new(Camera::default());
    fill_particles(&mut scene);
    c.bench_function("scene_validate_1024_spawn_batches", |b| b.iter(|| black_box(&scene).validate()));
}

criterion_group!(benches, scene_benches, particle_benches);
criterion_main!(benches);
