//! What a morphing frame allocates after warm-up: new weights every frame (a blend and an upload) and unchanged weights
//! (no blend). As in `hot_loop.rs`, wgpu allocates inside its own calls; what this crate controls, and what is asserted,
//! is that none of its own code does.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
use omuzaki_render_mock::counting;

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting::new(env!("CARGO_PKG_NAME"));
#[path = "support/grid.rs"]
mod grid;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, Instance, Material, MeshOptions, MorphRange, MorphSet, Outline, Quality, Renderer,
    Scene, TargetDesc,
};

/// Runs on the GPU path, or on the CPU path forced on the same adapter.
fn morphing_allocates_nothing_in_this_crate_after_warm_up(cpu: bool) {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let mut r = support::renderer();
    if cpu {
        r.force_cpu_morph().unwrap();
    }
    assert_eq!(r.morph_path(), if cpu { "cpu" } else { "gpu" });
    let mesh = grid::grid(100, 200);
    let n = mesh.positions.len();
    let ramp: Vec<Vec3> = (0..n).map(|i| Vec3::new(0.0, 0.0, (i % 7) as f32 * 0.01)).collect();
    let targets: Vec<_> = (0..4).map(|i| (Slug::new(format!("t{i}")).unwrap(), &ramp[..], &[][..])).collect();
    let set = MorphSet::from_dense(n, &targets, 0.0).unwrap();
    let id = r.create_mesh_with(&mesh, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }).unwrap();
    // Outlined, so each re-blend also re-welds the outline directions.
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.outline = Some(Outline { colour: Colour::BLACK, width: 0.01 });
    let material = r.create_material(&material).unwrap();
    let target = r.create_target(&TargetDesc { size: UVec2::splat(64), alpha: AlphaEncoding::Straight }).unwrap();
    let mut scene =
        Scene::new(Camera::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    let mut instance = Instance::new(id, material, Mat4::IDENTITY);
    instance.morph = Some(MorphRange { start: 0, len: 4 });
    scene.instances.push(instance);
    let quality = Quality::live();

    let mut frame = |i: u32| {
        let w = if i.is_multiple_of(2) { i as f32 * 0.01 } else { 0.3 };
        scene.morph_weights.clear();
        scene.morph_weights.extend([w, 0.5, 0.0, 1.0]);
        scene.frame.index = u64::from(i);
        r.render(target, &scene, &quality).unwrap();
    };
    for i in 0..30 {
        frame(i);
    }
    let totals = counting::measure(|| {
        for i in 30..50 {
            frame(i);
        }
    });
    println!("allocations over 20 frames by owner: {totals:?}");
    for owner in [counting::OWN, counting::WORKSPACE, counting::UNATTRIBUTED] {
        assert_eq!(totals.get(owner).copied().unwrap_or(0), 0, "{owner} allocated: {totals:?}");
    }
}

#[test]
fn on_the_gpu_path() {
    morphing_allocates_nothing_in_this_crate_after_warm_up(false);
}

#[test]
fn on_the_cpu_path() {
    morphing_allocates_nothing_in_this_crate_after_warm_up(true);
}
