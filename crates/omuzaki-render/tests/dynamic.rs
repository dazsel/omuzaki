//! Dynamic meshes (R2): `create_mesh_with { usage: Dynamic }` and `update_vertices`.
//!
//! The contract's own checks (`DYNAMIC_CHECKS`) run in `conformance.rs`; these cover what is specific to this renderer:
//! that an updated mesh draws exactly like a fresh one in every pass that reads its geometry (colour, outline hull,
//! shadow bounds), that bad updates fail without touching the mesh, and that updates keep their order with frames.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/grid.rs"]
mod grid;
mod support;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Capabilities, Colour, Image, Instance, Light, Material, MeshData, MeshId, MeshOptions,
    MeshUsage, Outline, Quality, RenderError, Renderer, RendererExt, Scene,
};

const SIZE: UVec2 = UVec2::new(96, 64);
const DYNAMIC: MeshOptions = MeshOptions { usage: MeshUsage::Dynamic, morphs: None };

fn scene(mesh: MeshId, material: omuzaki_render_api::MaterialId) -> Scene {
    let camera = Camera::look_at(Vec3::new(0.6, 0.5, 2.6), Vec3::ZERO, Vec3::Y, Camera::default().projection);
    let mut scene = Scene::new(camera);
    scene.lights.push(Light::Directional {
        to_light: Vec3::new(0.3, 0.6, 1.0).normalize(),
        colour: Colour::WHITE,
        intensity: 1.0,
        casts_shadow: true,
    });
    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    scene
}

fn outlined() -> Material {
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.double_sided = true;
    material.outline = Some(Outline { colour: Colour::BLACK, width: 0.02 });
    material
}

fn render(r: &mut WgpuRenderer, mesh: MeshId, material: omuzaki_render_api::MaterialId) -> Image {
    r.render_image(SIZE, AlphaEncoding::Straight, &scene(mesh, material), &Quality::high()).unwrap()
}

fn mesh_vertices(mesh: &MeshData) -> (Vec<Vec3>, Vec<Vec3>) {
    (mesh.positions.clone(), mesh.normals.clone())
}

#[test]
fn the_renderer_reports_dynamic_meshes() {
    let caps: Capabilities = support::renderer().capabilities().clone();
    assert!(caps.dynamic_meshes);
}

#[test]
fn an_updated_mesh_draws_like_a_fresh_one_with_its_outline_and_shadow() {
    let mut r = support::renderer();
    let material = r.create_material(&outlined()).unwrap();
    let dynamic = r.create_mesh_with(&grid::grid(40, 20), &DYNAMIC).unwrap();
    // Two updates, so the hull is refreshed from a mesh that already moved once.
    for t in [0.7, 2.1] {
        let (positions, normals) = mesh_vertices(&grid::waved(40, 20, t));
        r.update_vertices(dynamic, &positions, &normals).unwrap();
        let updated = render(&mut r, dynamic, material);
        let fresh_mesh = r.create_mesh(&grid::waved(40, 20, t)).unwrap();
        let fresh = render(&mut r, fresh_mesh, material);
        r.release(fresh_mesh.into()).unwrap();
        assert_eq!(updated.rgba8, fresh.rgba8, "t = {t}: the updated mesh differs from a fresh one");
    }
    let flat_mesh = r.create_mesh(&grid::grid(40, 20)).unwrap();
    let flat = render(&mut r, flat_mesh, material);
    let waved = render(&mut r, dynamic, material);
    assert_ne!(flat.rgba8, waved.rgba8, "the wave must be visible, or the comparison proves nothing");
}

#[test]
fn a_bad_update_is_an_error_and_leaves_the_mesh_as_it_was() {
    let mut r = support::renderer();
    let material = r.create_material(&outlined()).unwrap();
    let mesh = grid::waved(8, 4, 1.0);
    let dynamic = r.create_mesh_with(&mesh, &DYNAMIC).unwrap();
    let before = render(&mut r, dynamic, material);
    let (positions, normals) = mesh_vertices(&mesh);
    // After every refused update the original handle must still draw exactly as it did.
    let unchanged = |r: &mut WgpuRenderer, what: &str| {
        assert_eq!(render(r, dynamic, material).rgba8, before.rgba8, "{what}: the refused update changed the mesh");
    };

    let short = r.update_vertices(dynamic, &positions[1..], &normals);
    assert!(matches!(short, Err(RenderError::Invalid { .. })), "{short:?}");
    unchanged(&mut r, "short positions");
    let mut nan = positions.clone();
    nan[3].y = f32::NAN;
    let non_finite = r.update_vertices(dynamic, &nan, &normals);
    assert!(matches!(non_finite, Err(RenderError::Invalid { .. })), "{non_finite:?}");
    unchanged(&mut r, "non-finite position");
    let mut bad_normals = normals.clone();
    bad_normals[2].x = f32::INFINITY;
    let non_finite_normal = r.update_vertices(dynamic, &positions, &bad_normals);
    assert!(matches!(non_finite_normal, Err(RenderError::Invalid { .. })), "{non_finite_normal:?}");
    unchanged(&mut r, "non-finite normal");
    // The attempt on a static mesh carries a different shape (a flat sheet against the waved one), so a mesh that
    // accepted it would draw differently.
    let fixed = r.create_mesh(&mesh).unwrap();
    let fixed_before = render(&mut r, fixed, material);
    let (flat_positions, flat_normals) = mesh_vertices(&grid::grid(8, 4));
    assert_ne!(flat_positions, positions, "the attempted shape must differ from the mesh");
    let on_static = r.update_vertices(fixed, &flat_positions, &flat_normals);
    assert!(matches!(on_static, Err(RenderError::Invalid { .. })), "{on_static:?}");
    assert_eq!(render(&mut r, fixed, material).rgba8, fixed_before.rgba8, "the static mesh changed");
    let flat_mesh = r.create_mesh(&grid::grid(8, 4)).unwrap();
    let flat = render(&mut r, flat_mesh, material);
    assert_ne!(fixed_before.rgba8, flat.rgba8, "the two shapes must draw differently, or the check proves nothing");
    unchanged(&mut r, "update of a static mesh");
    r.release(dynamic.into()).unwrap();
    let released = r.update_vertices(dynamic, &positions, &normals);
    assert!(matches!(released, Err(RenderError::UnknownHandle { .. })), "{released:?}");
}

#[test]
fn morph_targets_are_refused_until_the_morph_task_lands() {
    // Morph targets are R3's: until it lands the renderer must say so rather than ignore the set.
    let mut r = support::renderer();
    let set = omuzaki_render_api::MorphSet::default();
    let options = MeshOptions { usage: MeshUsage::Dynamic, morphs: Some(set) };
    let refused = r.create_mesh_with(&grid::grid(4, 4), &options);
    assert!(matches!(refused, Err(RenderError::Unsupported(_))), "{refused:?}");
}

/// `mesh` with every vertex pushed out along its own normal: a cube's faces come apart, so the corner vertices that
/// shared a position no longer do.
fn exploded(mesh: &MeshData, distance: f32) -> MeshData {
    let mut out = mesh.clone();
    for (position, normal) in out.positions.iter_mut().zip(&mesh.normals) {
        *position += *normal * distance;
    }
    out
}

#[test]
fn an_update_that_splits_welded_vertices_and_joins_them_again_outlines_like_a_fresh_mesh() {
    let mut r = support::renderer();
    let material = r.create_material(&outlined()).unwrap();
    let cube = omuzaki_render_mock::shapes::cuboid(Vec3::splat(0.5));
    let apart = exploded(&cube, 0.3);

    // Welded at creation, split by an update (the corner's three normals no longer share one outline direction).
    let dynamic = r.create_mesh_with(&cube, &DYNAMIC).unwrap();
    let (positions, normals) = mesh_vertices(&apart);
    r.update_vertices(dynamic, &positions, &normals).unwrap();
    let split = render(&mut r, dynamic, material);
    let fresh_apart = r.create_mesh(&apart).unwrap();
    assert_eq!(split.rgba8, render(&mut r, fresh_apart, material).rgba8, "split: differs from a fresh upload");

    // ... and welded again by the next one.
    let (positions, normals) = mesh_vertices(&cube);
    r.update_vertices(dynamic, &positions, &normals).unwrap();
    let joined = render(&mut r, dynamic, material);
    let fresh_cube = r.create_mesh(&cube).unwrap();
    assert_eq!(joined.rgba8, render(&mut r, fresh_cube, material).rgba8, "joined: differs from a fresh upload");
    assert_ne!(split.rgba8, joined.rgba8, "the two shapes must differ, or the comparison proves nothing");

    // Created apart, then joined: the welds cannot have been fixed at creation.
    let from_apart = r.create_mesh_with(&apart, &DYNAMIC).unwrap();
    r.update_vertices(from_apart, &positions, &normals).unwrap();
    assert_eq!(render(&mut r, from_apart, material).rgba8, joined.rgba8, "created apart: differs once joined");
}
