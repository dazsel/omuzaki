//! The `Particles` pass at 1080p with 5,000 particles (#940's first number; the full measurement is #958): snow in a
//! steady stream over the frame and confetti and star bursts in front of a character, the frame's batches refilled
//! every frame as the stage does. Prints the pass's GPU time (median and p95) when the adapter has timestamps, its CPU
//! recording time, and the frame time with and without the particles.
//! `cargo bench -p omuzaki-render --bench particles`; on a hardware adapter set `WGPU_ADAPTER_NAME`.

// A benchmark may panic: a panic is a clear failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::time::Instant;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, EmitterShape, Instance, Material, Orientation, ParticleParams, ParticleShading,
    ParticleSystemDesc, Pass, Quality, Renderer, Scene, Span, SpawnBatch, Sprite, SpriteShape, TargetDesc,
};
use omuzaki_render_mock::particle_sheet::still;
use omuzaki_render_mock::shapes;

const FRAMES: u64 = 120;
const SIZE: UVec2 = UVec2::new(1920, 1080);

fn main() {
    // The software rasteriser, or the adapter `WGPU_ADAPTER_NAME` names.
    let mut r = support::renderer();
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::new(0.5, 1.6, 0.3))).unwrap();
    let material = r.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let falling = |size, orientation, shape, colour| {
        let params = ParticleParams {
            lifetime: Span::new(3.0, 5.0),
            speed: Span::new(0.2, 1.0),
            spread_deg: 60.0,
            gravity: [0.0, -0.8, 0.0],
            drag: 0.6,
            shading: ParticleShading::toon(Colour::hex(0x00b8_a8c8), 0.0, 0.1),
            ..still(size, orientation, Colour::hex(colour))
        };
        ParticleSystemDesc { sprite: Sprite::Shape(shape), params }
    };
    let systems = [
        falling(0.02, Orientation::billboard(Span::new(-60.0, 60.0)), SpriteShape::Disc, 0x00f4_f8ff),
        falling(0.015, Orientation::tumble(Span::new(200.0, 600.0)), SpriteShape::rect(0.6), 0x00ff_5a7a),
        falling(0.03, Orientation::billboard(Span::fixed(90.0)), SpriteShape::star(5), 0x00ff_c94d),
    ]
    .map(|d| r.create_particle_system(&d).unwrap());
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 1.0, 3.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::Y,
        Camera::default().projection,
    ));
    let quality = Quality::live();

    let mut run = |particles: bool| {
        let (mut gpu, mut cpu, mut slots) = (Vec::new(), Vec::new(), 0);
        let start = Instant::now();
        for index in 0..FRAMES {
            scene.clear();
            scene.frame.index = index;
            scene.frame.time = Seconds(10.0 + index as f64 / 60.0);
            scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::new(0.0, 0.8, 0.0))));
            if particles {
                // Snow: a stream of 3,500 over a 4 m x 2 m ceiling in 100 batches. Confetti and stars: 20 bursts of 75.
                for segment in 0..100u32 {
                    scene.particles.push(SpawnBatch {
                        shape: EmitterShape::rect(Vec2::new(2.0, 1.0)),
                        start: Seconds(f64::from(segment) * 0.05),
                        interval: 0.0015,
                        first: segment * 35,
                        count: 35,
                        seed: 7,
                        ..SpawnBatch::new(systems[0], Mat4::from_translation(Vec3::new(0.0, 2.6, 0.0)))
                    });
                }
                for burst in 0..20u32 {
                    scene.particles.push(SpawnBatch {
                        shape: EmitterShape::sphere(0.1),
                        start: Seconds(9.0 + f64::from(burst) * 0.1),
                        count: 75,
                        seed: burst,
                        ..SpawnBatch::new(
                            systems[1 + burst as usize % 2],
                            Mat4::from_translation(Vec3::new(0.0, 1.0, 0.5)),
                        )
                    });
                }
            }
            let report = r.render(target, &scene, &quality).unwrap();
            slots = report.particles;
            if let Some(t) = report.cpu.get(Pass::Particles) {
                cpu.push(t.0 * 1e3);
            }
            if let Some(t) = report.gpu.and_then(|g| g.passes.get(Pass::Particles)) {
                gpu.push(t.0 * 1e3);
            }
        }
        (start.elapsed().as_secs_f64() * 1e3 / FRAMES as f64, slots, gpu, cpu)
    };
    run(true);
    let (without, ..) = run(false);
    let (with, slots, mut gpu, mut cpu) = run(true);
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (v.get(v.len() / 2).copied(), v.get(v.len() * 95 / 100).copied())
    };
    let (gpu_median, gpu_p95) = stats(&mut gpu);
    let (cpu_median, _) = stats(&mut cpu);
    let ms = |t: Option<f64>| t.map_or_else(|| "n/a".to_owned(), |t| format!("{t:.3} ms"));
    println!("Particles at {}x{}, {slots} slots in 120 batches, {} frames:", SIZE.x, SIZE.y, FRAMES);
    println!("  GPU time of the pass: median {}, p95 {}", ms(gpu_median), ms(gpu_p95));
    println!("  CPU recording of the pass: median {}", ms(cpu_median));
    println!("  frame: {with:.2} ms with particles, {without:.2} ms without ({:+.2} ms)", with - without);
}
