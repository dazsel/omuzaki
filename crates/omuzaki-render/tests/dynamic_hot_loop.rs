//! What a dynamic-mesh frame allocates after warm-up: `update_vertices` plus a render of the mesh with its outline.
//!
//! As in `hot_loop.rs`, wgpu allocates inside its own calls; what this crate controls, and what is asserted, is that none
//! of its own code does.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
#[path = "support/counting.rs"]
mod counting;
#[path = "support/grid.rs"]
mod grid;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, Instance, Material, MeshOptions, MeshUsage, Outline, Quality, Renderer, Scene,
    TargetDesc,
};

#[test]
fn updating_and_drawing_a_dynamic_mesh_allocates_nothing_in_this_crate() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let mut r = support::renderer();
    let mesh = grid::grid(100, 200);
    let flat = mesh.positions.clone();
    let options = MeshOptions { usage: MeshUsage::Dynamic, morphs: None };
    let id = r.create_mesh_with(&mesh, &options).unwrap();
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.outline = Some(Outline { colour: Colour::BLACK, width: 0.01 });
    let material = r.create_material(&material).unwrap();
    let target = r.create_target(&TargetDesc { size: UVec2::splat(64), alpha: AlphaEncoding::Straight }).unwrap();
    let mut scene =
        Scene::new(Camera::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    scene.instances.push(Instance::new(id, material, Mat4::IDENTITY));
    let quality = Quality::live();
    let (mut positions, mut normals) = (mesh.positions.clone(), mesh.normals.clone());

    let mut frame = |i: u32| {
        grid::wave_into(&flat, i as f32 * 0.1, &mut positions, &mut normals);
        r.update_vertices(id, &positions, &normals).unwrap();
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
