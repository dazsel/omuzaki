//! A morphed, outlined, hard-edged mesh draws exactly like a mesh created from the blended geometry: the outline hull
//! follows the morphed normals without splitting at the cube's edges.
//!
//! Set `OMUZAKI_RENDER_DUMP=<dir>` to also write the preblended and morphed pictures there (evidence for reviews).

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Light, Material, MeshData, MeshOptions, MorphRange,
    MorphSet, Outline, Quality, Renderer, RendererExt, Scene,
};
use omuzaki_render_mock::{MockRenderer, morph_vertices, shapes};

fn cube_with_morph() -> (MeshData, MorphSet) {
    let cube = shapes::cuboid(Vec3::splat(0.5));
    // The top face lifts and shears; every normal leans toward +X, so the hard edges' hulls must move together.
    let positions: Vec<Vec3> =
        cube.positions.iter().map(|p| if p.y > 0.0 { Vec3::new(0.35, 0.2, 0.0) } else { Vec3::ZERO }).collect();
    let normals = vec![Vec3::new(0.9, 0.0, 0.4); cube.positions.len()];
    let morphs =
        MorphSet::from_dense(cube.positions.len(), &[(Slug::new("lean").unwrap(), &positions, &normals)], 0.0).unwrap();
    (cube, morphs)
}

fn scene(mesh: omuzaki_render_api::MeshId, material: omuzaki_render_api::MaterialId, weights: &[f32]) -> Scene {
    let mut scene =
        Scene::new(Camera::look_at(Vec3::new(2.4, 1.8, 3.2), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    scene.background = Background::Solid { colour: Colour::hex(0x0018_1220) };
    scene.lights = vec![Light::Directional {
        to_light: Vec3::new(-0.8, 1.6, 2.2),
        colour: Colour::WHITE,
        intensity: 2.0,
        casts_shadow: false,
    }];
    scene.morph_weights = weights.to_vec();
    let mut inst = Instance::new(mesh, material, Mat4::IDENTITY);
    if !weights.is_empty() {
        inst.morph = Some(MorphRange { start: 0, len: 1 });
    }
    scene.instances.push(inst);
    scene
}

fn save(image: &Image, dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    image::save_buffer(dir.join(name), &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
}

#[test]
fn a_morphed_hard_edged_mesh_outlines_like_the_preblended_one() {
    let (cube, morphs) = cube_with_morph();
    let mut renderer = MockRenderer::new();
    let morphed = renderer
        .create_mesh_with(&cube, &MeshOptions { morphs: Some(morphs.clone()), ..MeshOptions::default() })
        .unwrap();
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.outline = Some(Outline { colour: Colour::hex(0x0043_3a6e), width: 0.03 });
    let material = renderer.create_material(&material).unwrap();
    let dump = std::env::var_os("OMUZAKI_RENDER_DUMP");

    for weight in [0.0f32, 0.5, 1.0] {
        let (positions, normals) = morph_vertices(&cube.positions, &cube.normals, &morphs, &[weight]).unwrap();
        let blended = MeshData { positions, normals, ..cube.clone() };
        let fresh = renderer.create_mesh(&blended).unwrap();
        let size = UVec2::new(256, 256);
        let quality = Quality::offline();
        let want =
            renderer.render_image(size, AlphaEncoding::Straight, &scene(fresh, material, &[]), &quality).unwrap();
        let got = renderer
            .render_image(size, AlphaEncoding::Straight, &scene(morphed, material, &[weight]), &quality)
            .unwrap();
        assert_eq!(got, want, "weight {weight}: the morphed mesh must draw like the preblended one, outline included");
        if let Some(dir) = &dump {
            save(&want, Path::new(dir), &format!("preblended-w{weight}.png"));
            save(&got, Path::new(dir), &format!("morphed-w{weight}.png"));
        }
        renderer.release(fresh.into()).unwrap();
    }
}
