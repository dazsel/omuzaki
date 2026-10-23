//! What a frame with particles allocates after warm-up (spec §8, "no allocation in the hot loop"; particles.md D6). The
//! method and its limits are those of `hot_loop.rs`: wgpu allocates inside its own calls, so the strict check is that
//! none of the allocations come from `omuzaki-render` itself. The scene is the particle budget's shape: 5,000 slots of
//! snow, confetti and stars across three systems, a steady stream and bursts, more batches than one block holds, with a
//! character in front, the frame's spawn batches refilled every frame as the stage does.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
use omuzaki_render_mock::counting;

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting::new(env!("CARGO_PKG_NAME"));

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, EmitterShape, Instance, Material, MaterialId, MeshId, Orientation, ParticleParams,
    ParticleShading, ParticleSystemDesc, ParticleSystemId, Pass, Quality, Renderer, Scene, Span, SpawnBatch, Sprite,
    SpriteShape, TargetDesc, TargetId,
};
use omuzaki_render_mock::particle_sheet::still;
use omuzaki_render_mock::shapes;

struct Fixture {
    renderer: WgpuRenderer,
    scene: Scene,
    quality: Quality,
    target: TargetId,
    mesh: MeshId,
    material: MaterialId,
    systems: [ParticleSystemId; 3],
}

fn fixture() -> Fixture {
    let mut renderer = support::renderer();
    let mesh = renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.3))).unwrap();
    let material = renderer.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let falling = |size, orientation, shape| {
        let params = ParticleParams {
            lifetime: Span::new(3.0, 5.0),
            speed: Span::new(0.2, 1.0),
            spread_deg: 60.0,
            gravity: [0.0, -0.8, 0.0],
            drag: 0.6,
            shading: ParticleShading::toon(Colour::hex(0x00b8_a8c8), 0.0, 0.1),
            ..still(size, orientation, Colour::hex(0x00ff_5a7a))
        };
        ParticleSystemDesc { sprite: Sprite::Shape(shape), params }
    };
    let descs = [
        falling(0.02, Orientation::billboard(Span::new(-60.0, 60.0)), SpriteShape::Disc),
        falling(0.015, Orientation::tumble(Span::new(200.0, 600.0)), SpriteShape::rect(0.6)),
        falling(0.03, Orientation::billboard(Span::fixed(90.0)), SpriteShape::star(5)),
    ];
    let systems = descs.map(|d| renderer.create_particle_system(&d).unwrap());
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(96, 64), alpha: AlphaEncoding::Straight }).unwrap();
    let scene =
        Scene::new(Camera::look_at(Vec3::new(0.0, 0.5, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    Fixture { renderer, scene, quality: Quality::live(), target, mesh, material, systems }
}

impl Fixture {
    /// One frame the way the stage does it: clear the scene, refill it with the character and 5,000 slots in 120
    /// batches, render. True when the pass ran.
    fn frame(&mut self, index: u64) -> bool {
        let Self { renderer, scene, quality, target, mesh, material, systems } = self;
        scene.clear();
        scene.frame.index = index;
        scene.frame.time = Seconds(10.0 + f64::from(u32::try_from(index).unwrap()) / 60.0);
        scene.instances.push(Instance::new(*mesh, *material, Mat4::IDENTITY));
        // Snow: one long stream in 100 segments of 35. Confetti and stars: ten bursts of 75 each.
        for segment in 0..100u32 {
            scene.particles.push(SpawnBatch {
                shape: EmitterShape::rect(Vec2::new(2.0, 1.0)),
                start: Seconds(f64::from(segment) * 0.05),
                interval: 0.0015,
                first: segment * 35,
                count: 35,
                seed: 7,
                ..SpawnBatch::new(systems[0], Mat4::from_translation(Vec3::new(0.0, 2.0, 0.0)))
            });
        }
        for burst in 0..20u32 {
            scene.particles.push(SpawnBatch {
                shape: EmitterShape::sphere(0.1),
                start: Seconds(9.0 + f64::from(burst) * 0.1),
                count: 75,
                seed: burst,
                ..SpawnBatch::new(systems[1 + burst as usize % 2], Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0)))
            });
        }
        renderer.render(*target, scene, quality).unwrap().cpu.get(Pass::Particles).is_some()
    }
}

/// Renders `frames` frames after a warm-up and returns each owner's allocations per frame.
fn allocations_per_frame(frames: u64) -> BTreeMap<&'static str, usize> {
    let mut f = fixture();
    // Warm-up: pipelines compile, the pass's group is built, and the GPU timing slots cycle through their states.
    for i in 0..30 {
        assert!(f.frame(i), "the Particles pass must run in this scene");
        f.renderer.wait_idle().unwrap();
    }
    let totals = counting::measure(|| {
        for i in 30..30 + frames {
            f.frame(i);
        }
    });
    totals.into_iter().map(|(owner, n)| (owner, n / usize::try_from(frames).unwrap())).collect()
}

#[test]
fn a_frame_with_particles_allocates_nothing_in_the_renderers_own_code() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let per_frame = allocations_per_frame(20);
    println!("allocations per frame by owner: {per_frame:?}");
    for owner in [counting::OWN, counting::WORKSPACE, counting::TESTS, counting::UNATTRIBUTED] {
        assert_eq!(per_frame.get(owner).copied().unwrap_or(0), 0, "{owner} allocated per frame: {per_frame:?}");
    }
}
