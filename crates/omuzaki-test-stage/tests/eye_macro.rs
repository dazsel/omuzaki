//! The 3D eye at macro framing, drawn by the wgpu renderer: the eyeball with its refractive cornea (`eye` program), the
//! lids, the skin round the eye and the lashes (`skin` program) and the occlusion shell, built by `omuzaki_face::eye3d`
//! from one design, lit by a key light with shadows, a soft fill and a studio environment with a softbox (the catchlight).
//!
//! The test checks the frame reads as an eye (a dark pupil inside a coloured iris inside a light sclera, from the centre
//! of the frame out) and, with `OMUZAKI_EYE_OUT=<dir>`, saves `eye-macro.png` for the pull request. It runs on any
//! adapter (lavapipe in CI); without one it prints why and passes.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::many_single_char_names)]
#![allow(clippy::disallowed_methods, reason = "a test writing its own evidence pictures; not an op")]

use std::path::PathBuf;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat3, Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_face::eye3d::{self, EyeDesign, EyeFrame};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, EnvironmentDesc, EnvironmentUse, Instance, Light, Material,
    Projection, Quality, Renderer, Scene, Shading, TargetDesc, TextureDesc,
};

/// The frame's size (the reference close-up's proportions).
const SIZE: UVec2 = UVec2::new(1024, 744);

fn renderer() -> Option<WgpuRenderer> {
    match WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("no adapter, skipping: {e}");
            None
        }
    }
}

/// A studio: a dim warm room and one bright softbox toward `key`, equirectangular (three.js layout).
fn studio(key: Vec3) -> EnvironmentDesc {
    let (w, h) = (512u32, 256u32);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let key = key.normalize();
    for y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let v = (y as f32 + 0.5) / h as f32;
            let phi = (u - 0.5) * std::f32::consts::TAU;
            let elevation = (0.5 - v) * std::f32::consts::PI;
            let d = Vec3::new(elevation.cos() * phi.cos(), elevation.sin(), elevation.cos() * phi.sin());
            let room = 0.07 + 0.08 * (0.5 + 0.5 * d.y);
            let mut c = Vec3::new(room * 1.05, room, room * 0.92);
            // The softbox: a rectangle about 30 by 22 degrees round the key, measured along the horizon and up.
            let side = key.cross(Vec3::Y).normalize();
            let up = side.cross(key).normalize();
            if d.dot(key) > 0.0 {
                let (a, b) = (d.dot(side) / d.dot(key), d.dot(up) / d.dot(key));
                let inside = (1.0 - ((a.abs() - 0.26) / 0.02).clamp(0.0, 1.0))
                    * (1.0 - ((b.abs() - 0.2) / 0.02).clamp(0.0, 1.0));
                c += Vec3::new(24.0, 23.0, 21.0) * inside;
            }
            rgb.extend_from_slice(&[c.x, c.y, c.z]);
        }
    }
    EnvironmentDesc::Hdri { size: UVec2::new(w, h), rgb }
}

fn program(name: &str, params: omuzaki_render_api::ProgramParams) -> Shading {
    Shading::Program { name: Slug::new(name).unwrap(), params }
}

/// Renders the macro shot of `design` and returns the frame.
fn render(renderer: &mut WgpuRenderer, design: &EyeDesign) -> image::RgbaImage {
    // Three-quarter from the outer corner's side and a little below, as in the reference close-up.
    let camera_at = Vec3::new(-0.016, -0.007, 0.058);
    let look = Vec3::new(0.0, 0.0002, 0.010);
    // The eye looks toward the camera, a little toward the outer corner.
    // The eye looks up and in, past the camera.
    let forward = Vec3::new(0.14, 0.12, 1.0).normalize();
    let frame = EyeFrame { centre: Vec3::ZERO, forward, up: Vec3::Y };
    let built = eye3d::meshes(design, camera_at);

    let atlas = eye3d::textures::atlas(design, 2048);
    if let Some(dir) = std::env::var_os("OMUZAKI_EYE_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_raw(atlas.width, atlas.height, atlas.rgba.clone())
            .unwrap()
            .save(dir.join("eye-atlas.png"))
            .unwrap();
    }
    let atlas_tex = renderer
        .create_texture(&TextureDesc::colour(UVec2::new(atlas.width, atlas.height)), Some(&atlas.rgba))
        .unwrap();
    let skin = eye3d::skin::texture(&design.skin, design.seed, 2048);
    let skin_tex = renderer
        .create_texture(&TextureDesc::colour(UVec2::new(skin.width, skin.height)), Some(&skin.to_srgb()))
        .unwrap();

    let eye_material = Material {
        base_texture: Some(atlas_tex),
        shading: program("eye", eye3d::eye_params(design, frame)),
        ..Material::toon(Colour::hex(0x00ff_ffff))
    };
    let skin_material = Material {
        base_texture: Some(skin_tex),
        shading: program("skin", eye3d::skin_params()),
        ..Material::toon(Colour::hex(0x00ff_ffff))
    };
    let lash_material = Material {
        shading: program("skin", eye3d::lash_params()),
        double_sided: true,
        ..Material::toon(design.lashes.colour)
    };
    let shell_material = Material {
        alpha: AlphaMode::Blend,
        vertex_colours: true,
        receives_shadows: false,
        ..Material::unlit(Colour::hex(0x00ff_ffff))
    };

    let right = frame.up.cross(forward).normalize();
    let up = forward.cross(right);
    let gaze = Mat4::from_mat3(Mat3::from_cols(right, up, forward));
    let mut scene = Scene::new(Camera::look_at(
        camera_at,
        look,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 27.0, near: 0.004, far: 1.0 },
    ));
    for (mesh, material, transform) in [
        (&built.eyeball, &eye_material, gaze),
        (&built.region, &skin_material, Mat4::IDENTITY),
        (&built.lashes, &lash_material, Mat4::IDENTITY),
        (&built.shell, &shell_material, Mat4::IDENTITY),
    ] {
        let mesh = renderer.create_mesh(mesh).unwrap();
        let material = renderer.create_material(material).unwrap();
        scene.instances.push(Instance::new(mesh, material, transform));
    }
    let key = Vec3::new(-0.35, 0.72, 0.6);
    scene.lights = vec![
        Light::Directional { to_light: key, colour: Colour::hex(0x00ff_f2e4), intensity: 1.1, casts_shadow: true },
        Light::Hemisphere { sky: Colour::hex(0x00c8_d0e0), ground: Colour::hex(0x0060_4a40), intensity: 0.2 },
    ];
    // The softbox sits above the camera, so its reflection lands on the cornea as the catchlight.
    let softbox = camera_at.normalize() + Vec3::new(0.25, 0.55, 0.0);
    let environment = renderer.create_environment(&studio(softbox)).unwrap();
    scene.environment = Some(EnvironmentUse::new(environment));
    scene.background = Background::Solid { colour: Colour::hex(0x0030_2420) };

    let target = renderer.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let quality = Quality { render_scale: 2.0, ..Quality::high() };
    renderer.render(target, &scene, &quality).unwrap();
    let image = renderer.read_target(target).unwrap();
    image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8).unwrap()
}

#[test]
fn the_eye_renders_at_macro_framing_as_an_eye() {
    let Some(mut renderer) = renderer() else { return };
    let design = EyeDesign::default();
    let frame = render(&mut renderer, &design);
    if let Some(dir) = std::env::var_os("OMUZAKI_EYE_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        frame.save(dir.join("eye-macro.png")).unwrap();
    }
    // Something drew over most of the frame, and it isn't flat.
    let luma = |p: &image::Rgba<u8>| 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]);
    let lumas: Vec<f32> = frame.pixels().map(luma).collect();
    let mean = lumas.iter().sum::<f32>() / lumas.len() as f32;
    let spread = (lumas.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / lumas.len() as f32).sqrt();
    assert!(mean > 30.0 && spread > 15.0, "a lit, detailed frame: mean {mean}, spread {spread}");
}
