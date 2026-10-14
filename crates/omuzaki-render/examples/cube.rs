//! Renders a toon cube headless and writes it as a PNG: `cargo run -p omuzaki-render --example cube -- out.png`.
//!
//! Uses the software rasteriser unless `WGPU_ADAPTER_NAME` names a GPU. The cube is lit by a hemisphere light and two
//! directional lights (the scene of the mock's golden cube, without its outline): it is the "does it draw at all" smoke
//! test, and the smallest complete program against the renderer.

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Instance, Light, Material, Quality, Renderer, RendererExt, Scene,
};
use omuzaki_render_mock::shapes;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args().nth(1).unwrap_or_else(|| "cube.png".to_owned());
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })?;
    println!("rendering on {}", renderer.capabilities().backend);

    let mesh = renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.5)))?;
    let material = renderer.create_material(&Material::toon(Colour::hex(0x00c9_b8f0)))?;
    let eye = Vec3::new(2.4, 1.8, 3.2);
    let mut scene = Scene::new(Camera::look_at(eye, Vec3::ZERO, Vec3::Y, Camera::default().projection));
    scene.background = Background::Solid { colour: Colour::hex(0x0018_1220) };
    scene.lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.5 },
        Light::Directional {
            to_light: Vec3::new(-0.8, 1.6, 2.2),
            colour: Colour::hex(0x00ff_f0e0),
            intensity: 2.0,
            casts_shadow: false,
        },
        Light::Directional {
            to_light: Vec3::new(1.8, 0.6, 1.5),
            colour: Colour::hex(0x00ff_d9c4),
            intensity: 0.55,
            casts_shadow: false,
        },
    ];
    scene.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(0.4)));

    let image = renderer.render_image(UVec2::new(192, 192), AlphaEncoding::Straight, &scene, &Quality::offline())?;
    image::save_buffer(&out, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8)?;
    println!("wrote {out}");
    Ok(())
}
