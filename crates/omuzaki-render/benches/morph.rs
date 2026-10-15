//! The CPU morph blend: 52 sparse targets over a 20 000-vertex mesh, measured through the renderer as the cost of a
//! frame with new weights (blend and upload) over the cost of one with the same weights (both skipped).
//! `cargo bench -p omuzaki-render --bench morph`.

// A benchmark may panic: a panic is a clear failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

#[path = "../tests/support/grid.rs"]
mod grid;
#[path = "../tests/support/mod.rs"]
mod support;

use std::time::Instant;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, Instance, Material, MeshOptions, MorphRange, MorphSet, Quality, Renderer, Scene,
    TargetDesc,
};

const TARGETS: usize = 52;
const FRAMES: u32 = 200;

fn main() {
    let mut r = support::renderer();
    let mesh = grid::grid(100, 200);
    let n = mesh.positions.len();
    // Each target moves a different 1/16th of the vertices (a face's blend shape touches a part of the head).
    let sparse: Vec<_> = (0..TARGETS)
        .map(|t| {
            let deltas: Vec<Vec3> =
                (0..n).map(|i| if (i + t) % 16 == 0 { Vec3::new(0.001, 0.002, 0.003) } else { Vec3::ZERO }).collect();
            (Slug::new(format!("t{t}")).unwrap(), deltas)
        })
        .collect();
    let dense: Vec<_> = sparse.iter().map(|(name, d)| (name.clone(), &d[..], &[][..])).collect();
    let set = MorphSet::from_dense(n, &dense, 0.0).unwrap();
    let id = r.create_mesh_with(&mesh, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }).unwrap();
    let material = r.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let target = r.create_target(&TargetDesc { size: UVec2::splat(32), alpha: AlphaEncoding::Straight }).unwrap();
    let camera = Camera::look_at(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection);
    let mut scene = Scene::new(camera);
    let mut instance = Instance::new(id, material, Mat4::IDENTITY);
    instance.morph = Some(MorphRange { start: 0, len: TARGETS as u32 });
    scene.instances.push(instance);
    let quality = Quality::live();
    let mut run = |vary: bool| {
        let start = Instant::now();
        for f in 0..FRAMES {
            let phase = if vary { f as f32 * 0.013 } else { 0.0 };
            scene.morph_weights.clear();
            scene.morph_weights.extend((0..TARGETS).map(|t| 0.1 + 0.8 * ((t as f32 * 0.3 + phase).sin().abs())));
            r.render(target, &scene, &quality).unwrap();
        }
        start.elapsed().as_secs_f64() * 1e3 / f64::from(FRAMES)
    };
    run(true);
    let (same, changing) = (run(false), run(true));
    println!(
        "{TARGETS} targets x {n} vertices: {changing:.2} ms/frame with new weights, {same:.2} ms with unchanged; \
         blend + upload = {:.2} ms",
        changing - same
    );
}
