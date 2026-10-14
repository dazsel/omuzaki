//! Image-based lighting on the GPU renderer (LK-08): the mock character under two synthetic HDR environments, lit
//! smoothly and through the toon ramp, rendered by the GPU renderer and by the mock.
//!
//! The environments are made here (a sky gradient with a small bright sun, and a soft overcast), so no HDRI is
//! downloaded or committed. The GPU pictures are goldens (`tests/golden/environment-*.png`, bless with `OMUZAKI_BLESS=1`);
//! the mock's pictures are the reference the GPU must stay close to. They cannot be identical: the mock shades one flat
//! colour per triangle, the GPU per pixel. `OMUZAKI_RENDER_OUT=<dir>` saves both sets for a contact sheet.
//!
//! The per-pixel maths (harmonics, rotation, intensity, the toon blend, the specular cube's faces) is proven in the
//! unit tests of `src/environment/`; the contract's behaviour in `omuzaki-render-mock`'s conformance checks, which this
//! renderer now runs (`tests/conformance.rs`).

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::{PI, TAU};
use std::path::{Path, PathBuf};

use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, MaterialName, RegistryBuilder};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, EnvironmentDesc, EnvironmentUse, Image, Instance, JointRange,
    Material, Projection, Quality, Renderer, RendererExt, Scene, Shading, Tonemap, Toon, sky_radiance,
};
use omuzaki_render_mock::MockRenderer;

const SIZE: UVec2 = UVec2::new(256, 256);
const BACKGROUND: Colour = Colour::hex(0x0020_2228);

/// One picture of the set: the environment and whether its diffuse light goes through the toon ramp.
#[derive(Copy, Clone)]
struct Shot {
    name: &'static str,
    sky: Sky,
    toon_ramp: bool,
}

#[derive(Copy, Clone, PartialEq)]
enum Sky {
    WarmSun,
    CoolOvercast,
}

const SHOTS: [Shot; 4] = [
    Shot { name: "warm-lit", sky: Sky::WarmSun, toon_ramp: false },
    Shot { name: "warm-toon", sky: Sky::WarmSun, toon_ramp: true },
    Shot { name: "cool-lit", sky: Sky::CoolOvercast, toon_ramp: false },
    Shot { name: "cool-toon", sky: Sky::CoolOvercast, toon_ramp: true },
];

/// The direction at equirectangular pixel `(x, y)` of a `size` map (the contract's layout).
fn pixel_direction(x: u32, y: u32, size: UVec2) -> Vec3 {
    let phi = ((x as f32 + 0.5) / size.x as f32 - 0.5) * TAU;
    let elevation = (0.5 - (y as f32 + 0.5) / size.y as f32) * PI;
    Vec3::new(elevation.cos() * phi.cos(), elevation.sin(), elevation.cos() * phi.sin())
}

/// A 256 x 128 HDRI from a radiance function of the direction.
fn synthesise(radiance: impl Fn(Vec3) -> Vec3) -> EnvironmentDesc {
    let size = UVec2::new(256, 128);
    let rgb = (0..size.y)
        .flat_map(|y| (0..size.x).map(move |x| (x, y)))
        .flat_map(|(x, y)| radiance(pixel_direction(x, y, size)).to_array())
        .collect();
    EnvironmentDesc::Hdri { size, rgb }
}

/// A warm afternoon: a hazy gradient and a small, very bright sun to the right and slightly in front, 40 degrees up.
fn warm_sun() -> EnvironmentDesc {
    let (azimuth, elevation) = (15.0_f32.to_radians(), 40.0_f32.to_radians());
    let sun = Vec3::new(elevation.cos() * azimuth.cos(), elevation.sin(), elevation.cos() * azimuth.sin());
    synthesise(|d| {
        let sky = sky_radiance(Colour::hex(0x0050_78d0), Colour::hex(0x00ff_d9a8), Colour::hex(0x0040_3020), d.y);
        if d.dot(sun) > 6.0_f32.to_radians().cos() { Vec3::new(120.0, 90.0, 50.0) } else { sky * 0.6 }
    })
}

/// A cool overcast day: a smooth bluish dome, brightest overhead, no sun.
fn cool_overcast() -> EnvironmentDesc {
    synthesise(|d| sky_radiance(Colour::hex(0x00d0_e0ff), Colour::hex(0x00a0_b4d0), Colour::hex(0x0050_5868), d.y))
}

const MOCK_EYE: Vec3 = Vec3::new(0.0, 1.45, 1.35);
const MOCK_TARGET: Vec3 = Vec3::new(0.0, 0.7, 0.0);

/// Builds the mock character in the scene on `renderer` (rest pose, plain toon materials) and returns it with no
/// environment yet.
fn character_scene(renderer: &mut dyn Renderer) -> Scene {
    let def = mock_character().unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let mut scene = Scene::new(Camera::look_at(
        MOCK_EYE,
        MOCK_TARGET,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    ));
    scene.background = Background::Solid { colour: BACKGROUND };
    // Exact colours: the mock and the GPU tone-map differently (see `tests/toon.rs`), and this is about the light.
    scene.post.tonemap = Tonemap::None;
    let mut materials = BTreeMap::new();
    for part in &built.meshes {
        let name: MaterialName = part.mesh.material.clone();
        if !materials.contains_key(&name) {
            let mut spec = built.materials[&name].clone();
            spec.overlays.clear();
            spec.base_texture = None;
            spec.outline = None;
            if matches!(spec.shading, Shading::Program { .. }) {
                // The hair program is `tests/hair.rs`'s; the mock draws programs as toon too.
                spec.shading = Shading::Toon(Toon::default());
            }
            let material = spec.to_material(&|_| None, &|_| None).unwrap();
            materials.insert(name.clone(), renderer.create_material(&material).unwrap());
        }
        let mesh = renderer.create_mesh(&part.mesh.geometry).unwrap();
        let mut instance = Instance::new(mesh, materials[&name], Mat4::IDENTITY);
        if !part.mesh.joint_bones.is_empty() {
            let start = u32::try_from(scene.joints.len()).unwrap();
            let len = u32::try_from(part.mesh.joint_bones.len()).unwrap();
            scene.joints.extend(std::iter::repeat_n(Mat4::IDENTITY, part.mesh.joint_bones.len()));
            instance.skin = Some(JointRange { start, len });
        }
        scene.instances.push(instance);
    }
    scene
}

fn quality() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

/// The four pictures of [`SHOTS`] on `renderer`, with the sky turned by `rotation_deg`.
fn shoot(renderer: &mut dyn Renderer, rotation_deg: f32) -> Vec<Image> {
    let mut scene = character_scene(renderer);
    let (warm, cool) =
        (renderer.create_environment(&warm_sun()).unwrap(), renderer.create_environment(&cool_overcast()).unwrap());
    SHOTS
        .iter()
        .map(|shot| {
            let id = if shot.sky == Sky::WarmSun { warm } else { cool };
            scene.environment =
                Some(EnvironmentUse { rotation_deg, toon_ramp: shot.toon_ramp, ..EnvironmentUse::new(id) });
            renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, &quality()).unwrap()
        })
        .collect()
}

fn save(image: &Image, name: &str) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        image::save_buffer(
            PathBuf::from(dir).join(name),
            &image.rgba8,
            image.size.x,
            image.size.y,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}

fn background_pixel() -> [u8; 3] {
    let c = BACKGROUND.to_linear();
    let encode = |v: f32| {
        let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    };
    [encode(c.x), encode(c.y), encode(c.z)]
}

fn is_background(image: &Image, i: usize) -> bool {
    let at = i * 4;
    let bg = background_pixel();
    image.rgba8[at..at + 3].iter().zip(bg).all(|(a, b)| a.abs_diff(b) <= 1)
}

fn covered(image: &Image) -> usize {
    (0..image.rgba8.len() / 4).filter(|&i| !is_background(image, i)).count()
}

fn mean_luminance(image: &Image, include: impl Fn(u32, u32) -> bool) -> f32 {
    let (mut sum, mut n) = (0.0, 0.0);
    for y in 0..image.size.y {
        for x in 0..image.size.x {
            let i = (y * image.size.x + x) as usize;
            if include(x, y) && !is_background(image, i) {
                let p = &image.rgba8[i * 4..i * 4 + 3];
                sum += 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]);
                n += 1.0;
            }
        }
    }
    sum / n
}

fn distinct_colours(image: &Image) -> usize {
    (0..image.rgba8.len() / 4)
        .filter(|&i| !is_background(image, i))
        .map(|i| [image.rgba8[i * 4], image.rgba8[i * 4 + 1], image.rgba8[i * 4 + 2]])
        .collect::<BTreeSet<_>>()
        .len()
}

#[test]
fn the_mock_character_under_two_skies_matches_its_goldens() {
    let gpu = shoot(&mut support::renderer(), 0.0);
    for (shot, image) in SHOTS.iter().zip(&gpu) {
        save(image, &format!("gpu-{}.png", shot.name));
        assert!(covered(image) > 4000, "{}: the character should fill the frame", shot.name);
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/environment-{}.png", shot.name));
        if std::env::var_os("OMUZAKI_BLESS").is_some() {
            image::save_buffer(&path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
        }
        let golden = image::open(&path)
            .unwrap_or_else(|e| panic!("{} (bless with OMUZAKI_BLESS=1): {e}", path.display()))
            .to_rgba8();
        assert_eq!(golden.dimensions(), (image.size.x, image.size.y));
        let worst = golden.as_raw().iter().zip(&image.rgba8).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(
            worst <= 2,
            "{} differs from its golden by up to {worst} levels (bless with OMUZAKI_BLESS=1)",
            shot.name
        );
    }
}

#[test]
fn the_gpu_stays_close_to_the_mock_under_the_same_sky() {
    let gpu = shoot(&mut support::renderer(), 0.0);
    let mock = shoot(&mut MockRenderer::new(), 0.0);
    for ((shot, g), m) in SHOTS.iter().zip(&gpu).zip(&mock) {
        save(m, &format!("mock-{}.png", shot.name));
        // Silhouettes agree (the two rasterisers differ only on edge pixels).
        let both: Vec<usize> =
            (0..g.rgba8.len() / 4).filter(|&i| !is_background(g, i) && !is_background(m, i)).collect();
        let union = covered(g).max(covered(m));
        assert!(
            both.len() * 100 >= union * 97,
            "{}: the silhouettes differ ({} shared of {union})",
            shot.name,
            both.len()
        );
        // The mock lights one colour per triangle, the GPU per pixel: compare the picture's overall level and its mean error.
        let (mg, mm) = (mean_luminance(g, |_, _| true), mean_luminance(m, |_, _| true));
        let error: f32 = both
            .iter()
            .map(|&i| (0..3).map(|c| f32::from(g.rgba8[i * 4 + c].abs_diff(m.rgba8[i * 4 + c]))).sum::<f32>() / 3.0)
            .sum::<f32>()
            / both.len() as f32;
        println!(
            "{}: mean luminance GPU {mg:.1}, mock {mm:.1}; mean error {error:.2} levels over {} pixels",
            shot.name,
            both.len()
        );
        assert!((mg - mm).abs() <= 0.03 * mm.max(40.0), "{}: overall level GPU {mg}, mock {mm}", shot.name);
        assert!(error <= 4.0, "{}: mean error {error} levels", shot.name);
    }
}

#[test]
fn the_toon_ramp_keeps_the_cel_bands() {
    let gpu = shoot(&mut support::renderer(), 0.0);
    // The sun makes a gradient across the head and body; the toon ramp turns it into bands. (An overcast sky is nearly
    // flat either way, so only the warm pair says anything.)
    for pair in gpu.chunks(2).take(1) {
        let (smooth, banded) = (distinct_colours(&pair[0]), distinct_colours(&pair[1]));
        println!("distinct colours: smooth {smooth}, toon ramp {banded}");
        assert!(banded * 2 < smooth, "the ramp should quantise the sky's light: {banded} colours against {smooth}");
    }
}

#[test]
fn turning_the_sky_moves_the_sun_across_the_character() {
    let mut renderer = support::renderer();
    // The sun is to the right; turned by -150 degrees about +Y it is to the left.
    // The head is the top of the picture; its two halves differ most.
    let side = |image: &Image| {
        let (left, right) =
            (mean_luminance(image, |x, y| y < 120 && x < 128), mean_luminance(image, |x, y| y < 120 && x >= 128));
        right - left
    };
    let (front, turned) = (shoot(&mut renderer, 0.0), shoot(&mut renderer, -150.0));
    let (before, after) = (side(&front[0]), side(&turned[0]));
    println!("right minus left luminance: sun on the right {before:.1}, turned {after:.1}");
    assert!(before > 5.0 && after < -5.0, "the lit side should follow the sun: {before} then {after}");
}

// ---- the Transparent pass ----

/// A rotated toon cube (three faces facing different ways) in `alpha` mode, lit by `sky` when given, in front of black.
fn cube_picture(renderer: &mut dyn Renderer, alpha: AlphaMode, with_sky: bool) -> Image {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 2.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    ));
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    if with_sky {
        let id = renderer.create_environment(&cool_overcast()).unwrap();
        scene.environment = Some(EnvironmentUse::new(id));
    }
    let mesh = renderer.create_mesh(&omuzaki_render_mock::shapes::cuboid(Vec3::splat(0.4))).unwrap();
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.alpha = alpha;
    let material = renderer.create_material(&material).unwrap();
    let turn = Mat4::from_rotation_y(0.6) * Mat4::from_rotation_x(0.4);
    scene.instances.push(Instance::new(mesh, material, turn));
    renderer.render_image(UVec2::splat(128), AlphaEncoding::Straight, &scene, &quality()).unwrap()
}

/// A blended surface at full alpha under an environment is lit as the opaque one is: the shared shader reads the
/// environment at group 3, which the Transparent pipeline must declare and bind.
#[test]
fn a_blended_surface_under_an_environment_matches_the_opaque_one() {
    let mut gpu = support::renderer();
    let opaque = cube_picture(&mut gpu, AlphaMode::Opaque, true);
    let blended = cube_picture(&mut gpu, AlphaMode::Blend, true);
    save(&opaque, "gpu-cube-opaque.png");
    save(&blended, "gpu-cube-blended.png");
    let worst = opaque.rgba8.iter().zip(&blended.rgba8).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 2, "the blended cube differs from the opaque one by up to {worst} levels");
    let unlit = cube_picture(&mut gpu, AlphaMode::Blend, false);
    assert!(
        opaque.rgba8.iter().zip(&unlit.rgba8).any(|(a, b)| a.abs_diff(*b) > 8),
        "the environment must light the blended cube (it was identical without one)"
    );
}

/// A blended draw with no environment in the scene binds the fallback one and passes pipeline validation.
#[test]
fn a_blended_draw_with_no_environment_renders() {
    let mut gpu = support::renderer();
    let image = cube_picture(&mut gpu, AlphaMode::Blend, false);
    assert!(covered_by(&image) > 1000, "the blended cube should be drawn");
}

fn covered_by(image: &Image) -> usize {
    image.rgba8.chunks(4).filter(|p| p[..3] != [0, 0, 0]).count()
}
