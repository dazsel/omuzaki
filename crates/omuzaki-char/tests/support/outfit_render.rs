//! The evidence pictures of the outfit tests: a built stack rendered front, side and back through the mock toon renderer to
//! binary PPM files. Included by the tests that make pictures (`#[path = "support/outfit_render.rs"] mod render;`), next to
//! the conformance suite as `conformance`.

#![allow(dead_code)] // each test binary uses the part it needs
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers: a panic is a clear failure
#![allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]

use std::path::Path;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, Light, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::MockRenderer;

use super::conformance::Stack;

/// Renders every mesh of `stack` from the front, the side and the back to `<template>-<view>.ppm` in `directory`.
pub fn render(stack: &Stack, template: &str, directory: &Path) {
    let height = stack
        .published
        .get::<omuzaki_char_api::FitFact>()
        .fits
        .first()
        .and_then(|f| f.section(omuzaki_char_api::BodyRegion::Neck, 1.0))
        .map_or(0.6, |s| s.centre.y);
    let centre = Vec3::new(0.0, height * 0.72, 0.0);
    for (view, azimuth_deg) in [("front", 0.0_f32), ("side", 90.0), ("back", 180.0)] {
        let mut renderer = MockRenderer::new();
        let eye = centre
            + Mat4::from_rotation_y(azimuth_deg.to_radians())
                .transform_vector3(Vec3::new(0.0, 0.1, 1.5) * (height / 0.55).max(1.0));
        let mut scene = Scene::new(Camera::look_at(eye, centre, Vec3::Y, Camera::default().projection));
        scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
        scene.lights = vec![
            Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
            Light::Directional {
                to_light: Vec3::new(1.6, 3.0, 2.4),
                colour: Colour::hex(0x00ff_f6ec),
                intensity: 1.7,
                casts_shadow: false,
            },
        ];
        scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
        for (_, output) in &stack.outputs {
            for built in &output.meshes {
                let mesh = renderer.create_mesh(&built.geometry).unwrap();
                let spec = output.materials.iter().find(|(m, _)| *m == built.material).unwrap().1.clone();
                let textures: Vec<_> = output
                    .textures
                    .iter()
                    .map(|(name, data)| (name.clone(), renderer.create_texture(&data.desc, Some(&data.rgba8)).unwrap()))
                    .collect();
                let material = spec
                    .to_material(&|name| textures.iter().find(|(n, _)| n == name).map(|(_, id)| *id), &|_| None)
                    .unwrap();
                let material = renderer.create_material(&material).unwrap();
                scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
            }
        }
        let quality = Quality { samples: 4, ..Quality::offline() };
        let image = renderer.render_image(UVec2::splat(768), AlphaEncoding::Straight, &scene, &quality).unwrap();
        let mut ppm = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
        for y in 0..image.size.y {
            for x in 0..image.size.x {
                let [r, g, b, _] = image.pixel(x, y).unwrap();
                ppm.extend([r, g, b]);
            }
        }
        std::fs::write(directory.join(format!("{template}-{view}.ppm")), ppm).unwrap();
    }
}
