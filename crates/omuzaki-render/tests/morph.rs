//! Morph targets on the GPU renderer: the contract's morph conformance checks, a sparse mesh drawn at three weights,
//! and the pool's rules (a dynamic base, a released mesh).

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Instance, Material, MeshData, MeshId, MeshOptions, MeshUsage,
    MorphRange, MorphSet, Projection, Quality, Renderer, RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::{conformance, shapes};

#[test]
fn the_renderer_claims_morph_targets() {
    let caps = support::renderer().capabilities().clone();
    assert!(caps.morph_targets);
    assert_eq!(caps.max_morph_targets, 256);
}

#[test]
fn the_morph_conformance_checks_pass_and_leak_nothing() {
    let mut r = support::renderer();
    if let Err(failure) = conformance::run_capable(&mut r) {
        panic!("{failure}");
    }
}

/// The columns of the middle row that are lit, drawing the quad on black with orthographic pixels 1/32 m wide.
fn lit_columns(r: &mut impl Renderer, mesh: MeshId, weight: f32) -> Vec<u32> {
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    let mut instance = Instance::new(mesh, material, Mat4::IDENTITY);
    instance.morph = Some(MorphRange { start: 0, len: 1 });
    scene.instances.push(instance);
    scene.morph_weights.push(weight);
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() };
    let img = r.render_image(UVec2::splat(64), AlphaEncoding::Straight, &scene, &quality).unwrap();
    r.release(material.into()).unwrap();
    (0..64).filter(|x| img.rgba8[(32 * 64 + x) as usize * 4] > 128).collect()
}

fn slide_options(quad: &MeshData, usage: MeshUsage) -> MeshOptions {
    let deltas = vec![Vec3::new(0.5, 0.0, 0.0); quad.positions.len()];
    let set = MorphSet::from_dense(quad.positions.len(), &[(Slug::new("slide").unwrap(), &deltas, &[])], 0.0).unwrap();
    MeshOptions { usage, morphs: Some(set) }
}

/// A 0.25 m quad (pixels 24..40) whose one target slides it 0.5 m (16 pixels) to the right.
#[test]
fn one_mesh_renders_at_weights_0_half_and_1() {
    let mut r = support::renderer();
    let quad = shapes::quad(Vec2::splat(0.25));
    let mesh = r.create_mesh_with(&quad, &slide_options(&quad, MeshUsage::Static)).unwrap();
    let edges: Vec<(u32, u32)> = [0.0, 0.5, 1.0]
        .iter()
        .map(|&w| {
            let lit = lit_columns(&mut r, mesh, w);
            (*lit.first().unwrap(), *lit.last().unwrap())
        })
        .collect();
    // Edge pixels are exact at one sample: the quad is 16 pixels wide and moves 0, 8 and 16 pixels.
    assert_eq!(edges, vec![(24, 39), (32, 47), (40, 55)]);
    r.release(mesh.into()).unwrap();
}

#[test]
fn a_dynamic_meshes_morphs_follow_its_updated_vertices() {
    let mut r = support::renderer();
    let quad = shapes::quad(Vec2::splat(0.25));
    let mesh = r.create_mesh_with(&quad, &slide_options(&quad, MeshUsage::Dynamic)).unwrap();
    let before = lit_columns(&mut r, mesh, 1.0);
    // Same weights, but the base is twice as wide: the cached blend must not be reused.
    let wide: Vec<Vec3> = quad.positions.iter().map(|p| *p * Vec3::new(2.0, 1.0, 1.0)).collect();
    r.update_vertices(mesh, &wide, &quad.normals).unwrap();
    let after = lit_columns(&mut r, mesh, 1.0);
    assert_eq!((before.len(), after.len()), (16, 32), "the morphed copy follows the update");
    r.release(mesh.into()).unwrap();
}
