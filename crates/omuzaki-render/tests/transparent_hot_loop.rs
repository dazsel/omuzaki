//! What a frame with blended instances allocates after warm-up (spec §8, "no allocation in the hot loop"). The method
//! and its limits are those of `hot_loop.rs`: wgpu allocates inside its own calls, so the strict check is that none of
//! the allocations come from `omuzaki-render` itself. The Transparent pass sorts its draws every frame, so the scene is a
//! skinned character and a row of blended props, more than the sort's small-input path handles in place.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
use omuzaki_render_mock::counting;

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting::new(env!("CARGO_PKG_NAME"));

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Camera, Colour, Instance, JointRange, Material, MaterialId, MeshId, Pass, Quality,
    Renderer, Scene, TargetDesc, TargetId,
};
use omuzaki_render_mock::shapes;

struct Fixture {
    renderer: WgpuRenderer,
    scene: Scene,
    quality: Quality,
    target: TargetId,
    meshes: [MeshId; 2],
    material: MaterialId,
}

fn fixture() -> Fixture {
    let mut renderer = support::renderer();
    let mut skinned = shapes::cuboid(Vec3::splat(0.2));
    skinned.joints = vec![[0, 1, 0, 0]; skinned.positions.len()];
    skinned.weights = vec![[0.5, 0.5, 0.0, 0.0]; skinned.positions.len()];
    let meshes =
        [renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.1))).unwrap(), renderer.create_mesh(&skinned).unwrap()];
    let mut material = Material::toon(Colour::rgba(0, 201, 184, 140));
    material.alpha = AlphaMode::Blend;
    let material = renderer.create_material(&material).unwrap();
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(64, 64), alpha: AlphaEncoding::Straight }).unwrap();
    let scene =
        Scene::new(Camera::look_at(Vec3::new(2.0, 1.5, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    let quality = Quality { render_scale: 1.5, ..Quality::high() };
    Fixture { renderer, scene, quality, target, meshes, material }
}

impl Fixture {
    /// One frame the way the stage does it: clear the scene, refill it, render. True when the pass ran.
    fn frame(&mut self, index: u64) -> bool {
        let Self { renderer, scene, quality, target, meshes, material } = self;
        scene.clear();
        scene.frame.index = index;
        let twist = f32::from(u16::try_from(index % 1000).unwrap()) * 0.05;
        scene.joints.extend((0..16u8).map(|j| Mat4::from_rotation_y(twist * f32::from(j % 2))));
        let mut character = Instance::new(meshes[1], *material, Mat4::IDENTITY);
        character.skin = Some(JointRange { start: 0, len: 16 });
        scene.instances.push(character);
        scene.instances.extend((0..40u8).map(|i| {
            Instance::new(meshes[0], *material, Mat4::from_translation(Vec3::new(f32::from(i) * 0.05 - 1.0, -0.5, 0.0)))
        }));
        renderer.render(*target, scene, quality).unwrap().cpu.get(Pass::Transparent).is_some()
    }
}

/// Renders `frames` frames after a warm-up and returns each owner's allocations per frame.
fn allocations_per_frame(frames: u64) -> BTreeMap<&'static str, usize> {
    let mut f = fixture();
    // Warm-up: pipelines compile, buffers and the sort vector grow, and the GPU timing slots cycle through their states.
    for i in 0..30 {
        assert!(f.frame(i), "the Transparent pass must run in this scene");
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
fn a_frame_with_blended_instances_allocates_nothing_in_the_renderers_own_code() {
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
