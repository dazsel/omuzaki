//! Spot lights and fog on the GPU renderer (LK-08): goldens of the mock character under one spot (inside,
//! in the penumbra of and outside the cone) and in fog at two densities, the GPU against the mock on the same scenes,
//! and the invariants that no rendering of them may break (full fog covers outlines too, the background is not fogged).
//!
//! The conformance suite (`conformance.rs`) already holds both features to the contract's numbers on probes; these are
//! whole pictures. The mock lights per triangle and the GPU per fragment, so the two are compared by their mean
//! difference, not pixel by pixel.
//!
//! `OMUZAKI_BLESS=1` rewrites the goldens. `OMUZAKI_RENDER_OUT=<dir>` also writes the GPU and the mock pictures of every case
//! there at 384 px, the inputs of the evidence sheet.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, Fog, Image, Instance, JointRange, Light, Material,
    Projection, Quality, Renderer, RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::{MockRenderer, shapes};

const SIZE: UVec2 = UVec2::new(256, 256);
const EVIDENCE_SIZE: UVec2 = UVec2::new(384, 384);
const SKY: u32 = 0x0086_8b99;

/// One GPU test at a time in this binary: software Vulkan compiles pipelines on the calling thread and racing devices
/// crashed the driver once (see `toon.rs`).
fn serial() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(PoisonError::into_inner)
}

fn quality() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

/// The mock character in its rest pose, with a row of pillars receding behind it (so fog has depth to work on), on the
/// renderer `r`. Lit by a dim hemisphere only: each test adds the light it is about.
fn stage(r: &mut dyn Renderer, camera: Camera) -> Scene {
    let def = mock_character().unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::hex(SKY) };
    scene.post.tonemap = Tonemap::None;
    scene.lights =
        vec![Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.35 }];
    let mut materials = BTreeMap::new();
    for part in &built.meshes {
        let name = part.mesh.material.clone();
        if !materials.contains_key(&name) {
            let mut spec = built.materials[&name].clone();
            spec.overlays.clear();
            spec.base_texture = None;
            let material = spec.to_material(&|_| None, &|_| None).unwrap();
            materials.insert(name.clone(), r.create_material(&material).unwrap());
        }
        let mesh = r.create_mesh(&part.mesh.geometry).unwrap();
        let mut instance = Instance::new(mesh, materials[&name], Mat4::IDENTITY);
        if !part.mesh.joint_bones.is_empty() {
            let start = u32::try_from(scene.joints.len()).unwrap();
            let len = u32::try_from(part.mesh.joint_bones.len()).unwrap();
            scene.joints.extend(std::iter::repeat_n(Mat4::IDENTITY, part.mesh.joint_bones.len()));
            instance.skin = Some(JointRange { start, len });
        }
        scene.instances.push(instance);
    }
    let pillar_mesh = r.create_mesh(&shapes::cuboid(Vec3::new(0.15, 0.9, 0.15))).unwrap();
    let pillar = r.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    for (x, z) in [(-0.9, -1.0), (0.9, -1.0), (-1.2, -3.0), (1.2, -3.0), (-1.6, -6.0), (1.6, -6.0)] {
        scene.instances.push(Instance::new(pillar_mesh, pillar, Mat4::from_translation(Vec3::new(x, 0.9, z))));
    }
    scene
}

/// Close on the head and torso, as the hair tests frame them: the lamp's cone falls across the picture.
fn close_camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 1.45, 1.35),
        Vec3::new(0.0, 0.7, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 30.0 },
    )
}

/// Back far enough to see the pillars recede.
fn wide_camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 1.0, 3.2),
        Vec3::new(0.0, 0.8, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far: 40.0 },
    )
}

const LAMP: Vec3 = Vec3::new(0.0, 1.9, 1.6);

/// One warm spot at the lamp, aimed at `aim`.
fn spot(aim: Vec3, cone_outer_deg: f32, penumbra: f32) -> Light {
    Light::Spot {
        position: LAMP,
        direction: aim - LAMP,
        colour: Colour::hex(0x00ff_e8c8),
        intensity: 9.0,
        range: 8.0,
        cone_outer_deg,
        penumbra,
        casts_shadow: false,
    }
}

/// The cases: name, camera and what to add to the dim stage. The cone is 30 degrees with a soft edge when the character
/// is inside; 12 degrees with the full penumbra, aimed past the character's right, when its left is in the fade; and aimed
/// at the floor in front of the character when it is outside.
type Setup = fn(&mut Scene);

fn cases() -> [(&'static str, Camera, Setup); 5] {
    [
        ("spot_inside", close_camera(), |s| s.lights.push(spot(Vec3::new(0.0, 0.75, 0.0), 30.0, 0.3))),
        ("spot_penumbra", close_camera(), |s| s.lights.push(spot(Vec3::new(0.3, 0.75, 0.0), 12.0, 0.8))),
        ("spot_outside", close_camera(), |s| s.lights.push(spot(Vec3::new(0.0, -1.0, 0.0), 12.0, 0.3))),
        ("fog_light", wide_camera(), |s| {
            add_key(s);
            s.fog = Some(Fog { colour: Colour::hex(SKY), near: 4.0, far: 14.0 });
        }),
        ("fog_dense", wide_camera(), |s| {
            add_key(s);
            s.fog = Some(Fog { colour: Colour::hex(SKY), near: 2.0, far: 6.0 });
        }),
    ]
}

fn add_key(s: &mut Scene) {
    s.lights.push(Light::Directional {
        to_light: Vec3::new(0.4, 0.8, 0.6).normalize(),
        colour: Colour::hex(0x00ff_f0e0),
        intensity: 2.0,
        casts_shadow: false,
    });
}

fn render(r: &mut dyn Renderer, camera: Camera, setup: Setup, size: UVec2) -> Image {
    let mut scene = stage(r, camera);
    setup(&mut scene);
    r.render_image(size, AlphaEncoding::Straight, &scene, &quality()).unwrap()
}

// ---- goldens ----

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Compares `image` with `tests/golden/<name>.png` (writes it instead when `OMUZAKI_BLESS=1`): at most `budget` pixels may
/// differ by more than 3 levels.
fn check_golden(name: &str, image: &Image, budget: usize) {
    let path = golden_dir().join(format!("{name}.png"));
    if std::env::var("OMUZAKI_BLESS").is_ok_and(|v| v == "1") {
        image::save_buffer(&path, &image.rgba8, image.size.x, image.size.y, image::ExtendedColorType::Rgba8).unwrap();
        return;
    }
    let golden = image::open(&path)
        .unwrap_or_else(|e| panic!("golden {}: {e} (bless with OMUZAKI_BLESS=1)", path.display()))
        .to_rgba8();
    assert_eq!(golden.dimensions(), (image.size.x, image.size.y), "{name}: golden size");
    let off = golden.as_raw().chunks(4).zip(image.rgba8.chunks(4)).filter(|(a, b)| differs(a, b, 3)).count();
    if off > budget {
        support::keep(&format!("{name}.png"), (image.size.x, image.size.y), &image.rgba8);
    }
    let diff = support::Diff::of(golden.as_raw(), &image.rgba8);
    assert!(
        off <= budget,
        "{name}: {off} pixels differ from the golden (budget {budget}); {diff}; bless with OMUZAKI_BLESS=1"
    );
}

fn differs(a: &[u8], b: &[u8], by: u8) -> bool {
    a.iter().zip(b).any(|(x, y)| x.abs_diff(*y) > by)
}

#[test]
fn the_spot_and_fog_pictures_match_their_goldens() {
    let _gpu = serial();
    let mut r = support::renderer();
    for (name, camera, setup) in cases() {
        let image = render(&mut r, camera, setup, SIZE);
        check_golden(&format!("spot_fog_{name}"), &image, 40);
    }
}

// ---- GPU against the mock ----

/// Mean absolute difference per channel over the picture, in 8-bit levels.
fn mean_difference(a: &Image, b: &Image) -> f64 {
    let total: u64 = a.rgba8.iter().zip(&b.rgba8).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    total as f64 / a.rgba8.len() as f64
}

fn mean_level(image: &Image) -> f64 {
    image.rgba8.iter().map(|&v| f64::from(v)).sum::<f64>() / image.rgba8.len() as f64
}

#[test]
fn the_gpu_pictures_agree_with_the_mocks() {
    let _gpu = serial();
    let mut gpu = support::renderer();
    let mut mock = MockRenderer::new();
    for (name, camera, setup) in cases() {
        let (g, m) = (render(&mut gpu, camera, setup, SIZE), render(&mut mock, camera, setup, SIZE));
        let diff = mean_difference(&g, &m);
        println!(
            "{name}: mean difference {diff:.2} levels (GPU mean {:.1}, mock mean {:.1})",
            mean_level(&g),
            mean_level(&m)
        );
        assert!(diff < 4.0, "{name}: the GPU differs from the mock by {diff:.2} levels on average");
    }
}

// ---- what the pictures must show ----

#[test]
fn the_cone_lights_the_character_inside_and_not_outside() {
    let _gpu = serial();
    let mut r = support::renderer();
    let [inside, penumbra, outside] = [0, 1, 2].map(|i| {
        let (_, camera, setup) = cases()[i];
        mean_level(&render(&mut r, camera, setup, SIZE))
    });
    let dim = {
        let mut scene = stage(&mut r, close_camera());
        scene.lights.truncate(1);
        mean_level(&r.render_image(SIZE, AlphaEncoding::Straight, &scene, &quality()).unwrap())
    };
    println!("mean level: dim {dim:.1}, outside {outside:.1}, penumbra {penumbra:.1}, inside {inside:.1}");
    assert!((outside - dim).abs() < 0.5, "a cone that misses the character adds nothing: {outside} vs {dim}");
    assert!(penumbra > dim + 1.0 && inside > penumbra + 1.0, "brightness grows from outside to inside the cone");
}

/// Fog far enough to cover everything: every surface pixel, outlines included, is the fog colour, and the background,
/// which fog leaves alone, is not.
#[test]
fn full_fog_covers_surfaces_and_outlines_but_not_the_background() {
    let _gpu = serial();
    let mut r = support::renderer();
    let mut scene = stage(&mut r, close_camera());
    add_key(&mut scene);
    let fog = Colour::hex(0x0030_c080);
    scene.fog = Some(Fog { colour: fog, near: 0.0, far: 0.25 });
    let image = r.render_image(SIZE, AlphaEncoding::Straight, &scene, &quality()).unwrap();
    let (sky_colour, want) = (Colour::hex(SKY), fog);
    let (want, sky) = ([want.r, want.g, want.b], [sky_colour.r, sky_colour.g, sky_colour.b]);
    let (mut fogged, mut background) = (0, 0);
    for px in image.rgba8.chunks(4) {
        let rgb = [px[0], px[1], px[2]];
        if rgb == sky {
            background += 1;
        } else {
            assert!(
                rgb.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1),
                "a surface pixel {rgb:?} is not the fog {want:?}"
            );
            fogged += 1;
        }
    }
    assert!(fogged > 2000 && background > 2000, "{fogged} fogged and {background} background pixels");
}

// ---- evidence ----

#[test]
fn write_the_evidence_pictures_when_asked() {
    let Ok(dir) = std::env::var("OMUZAKI_RENDER_OUT") else { return };
    let _gpu = serial();
    let mut gpu = support::renderer();
    let mut mock = MockRenderer::new();
    for (name, camera, setup) in cases() {
        for (who, image) in [
            ("mock", render(&mut mock, camera, setup, EVIDENCE_SIZE)),
            ("gpu", render(&mut gpu, camera, setup, EVIDENCE_SIZE)),
        ] {
            let path = PathBuf::from(&dir).join(format!("{who}_{name}.png"));
            image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ExtendedColorType::Rgba8)
                .unwrap();
        }
    }
}

/// With `OMUZAKI_BASE_OUT=<dir>` and `OMUZAKI_BASE_TAG=<tag>`: the stage with no spot and no fog (hemisphere and key only),
/// from the mock and the GPU, as `<dir>/<tag>_{mock,gpu}.png`. Run on another revision, it shows what that revision
/// draws for the double-sided cloth with neither feature involved.
#[test]
fn write_the_base_pictures_when_asked() {
    let (Ok(dir), Ok(tag)) = (std::env::var("OMUZAKI_BASE_OUT"), std::env::var("OMUZAKI_BASE_TAG")) else { return };
    let _gpu = serial();
    let mut gpu = support::renderer();
    let mut mock = MockRenderer::new();
    for (who, r) in [("mock", &mut mock as &mut dyn Renderer), ("gpu", &mut gpu)] {
        let image = render(r, close_camera(), add_key, EVIDENCE_SIZE);
        let path = PathBuf::from(&dir).join(format!("{tag}_{who}.png"));
        image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ExtendedColorType::Rgba8).unwrap();
    }
}

/// The shipped WGSL `fog_amount` on a range one ulp wide. The surface is a few ulps beyond `far`, where the contract's
/// `Fog::amount` is 1: a shader that widened the range (a `max(far - near, 1e-6)` clamp) would give a fraction there and
/// leave some of the surface colour showing. (Exactly at `far` the shader's own depth rounding decides the side of the
/// edge, so the probe sits a few ulps past it, still inside 1e-6 of `near`, where the clamp makes the difference.)
#[test]
fn a_one_ulp_fog_range_is_full_fog_in_the_shader() {
    let _gpu = serial();
    let mut r = support::renderer();
    let near = 1.0_f32;
    let far = f32::from_bits(near.to_bits() + 1);
    assert_eq!(far, 1.0 + f32::EPSILON, "one ulp above 1.0 is 1.0000001192092896");
    let eye_depth = f32::from_bits(near.to_bits() + 4);
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, eye_depth),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    let mesh = r.create_mesh(&shapes::quad(omuzaki_core::glam::Vec2::splat(0.5))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::rgb(200, 50, 50))).unwrap();
    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    let fog = Colour::rgb(20, 40, 200);
    scene.fog = Some(Fog { colour: fog, near, far });
    let image = r.render_image(UVec2::splat(32), AlphaEncoding::Straight, &scene, &quality()).unwrap();
    let px = image.pixel(16, 16).unwrap();
    assert_eq!([px[0], px[1], px[2]], [fog.r, fog.g, fog.b], "a surface past `far` is fully fogged");
}

/// A camera 2 m in front of the origin looking down -z, for the blended-fog tests below.
fn quad_scene(fog: Fog) -> Scene {
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 2.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    );
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    scene.fog = Some(fog);
    scene
}

fn centre(image: &Image) -> [u8; 4] {
    let px = image.pixel(image.size.x / 2, image.size.y / 2).unwrap();
    [px[0], px[1], px[2], px[3]]
}

/// An unlit red quad of `alpha` at the origin (2 m away), in `mode`, over a blue wall 1 m behind it when `wall`.
fn red_quad(r: &mut dyn Renderer, fog: Fog, alpha: u8, mode: AlphaMode, wall: bool) -> Image {
    let mut scene = quad_scene(fog);
    let mesh = r.create_mesh(&shapes::quad(omuzaki_core::glam::Vec2::splat(0.4))).unwrap();
    let mut material = Material::unlit(Colour::rgba(255, 0, 0, alpha));
    material.alpha = mode;
    let material = r.create_material(&material).unwrap();
    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    if wall {
        let wall_mesh = r.create_mesh(&shapes::quad(omuzaki_core::glam::Vec2::splat(2.0))).unwrap();
        let blue = r.create_material(&Material::unlit(Colour::rgb(0, 0, 255))).unwrap();
        scene.instances.push(Instance::new(wall_mesh, blue, Mat4::from_translation(Vec3::new(0.0, 0.0, -1.0))));
    }
    r.render_image(UVec2::splat(64), AlphaEncoding::Straight, &scene, &quality()).unwrap()
}

/// A blended surface past `fog.far` is fogged exactly as an opaque one is (it was red: `fs_blend` skipped the fog).
#[test]
fn a_blended_surface_past_far_is_the_fog_colour_like_an_opaque_one() {
    let _gpu = serial();
    let mut r = support::renderer();
    let fog = Fog { colour: Colour::rgb(20, 40, 200), near: 0.5, far: 1.0 };
    let opaque = centre(&red_quad(&mut r, fog, 255, AlphaMode::Opaque, false));
    let blended = centre(&red_quad(&mut r, fog, 255, AlphaMode::Blend, false));
    assert_eq!(opaque[..3], [fog.colour.r, fog.colour.g, fog.colour.b], "the opaque quad is fully fogged");
    assert_eq!(blended, opaque, "the blended quad matches the opaque one");
}

/// Half-transparent fogged surfaces composite over what is behind them as the mock does, with the surface fogged
/// (full, here) and the wall behind it fogged too.
#[test]
fn a_partly_transparent_fogged_surface_composites_as_the_mock_does() {
    let _gpu = serial();
    let mut gpu = support::renderer();
    let mut mock = MockRenderer::new();
    for fog in [
        Fog { colour: Colour::rgb(20, 200, 40), near: 0.5, far: 1.0 },
        Fog { colour: Colour::rgb(20, 200, 40), near: 1.0, far: 4.0 },
    ] {
        let g = centre(&red_quad(&mut gpu, fog, 128, AlphaMode::Blend, true));
        let m = centre(&red_quad(&mut mock, fog, 128, AlphaMode::Blend, true));
        let worst = g.iter().zip(&m).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        println!("fog {fog:?}: GPU {g:?}, mock {m:?}");
        assert!(worst <= 2, "fog {fog:?}: the GPU {g:?} differs from the mock {m:?}");
    }
}

/// With `OMUZAKI_BLENDED_OUT=<dir>/<tag>`: the opaque, the opaque-over-wall blended and the half-transparent fogged quads
/// as `<tag>_{opaque,blend,half}.png` (the before/after of the blended-fog fix).
#[test]
fn write_the_blended_fog_pictures_when_asked() {
    let Ok(prefix) = std::env::var("OMUZAKI_BLENDED_OUT") else { return };
    let _gpu = serial();
    let mut r = support::renderer();
    let fog = Fog { colour: Colour::rgb(20, 200, 40), near: 0.5, far: 1.0 };
    let half = Fog { colour: Colour::rgb(20, 200, 40), near: 1.0, far: 4.0 };
    for (name, image) in [
        ("opaque", red_quad(&mut r, fog, 255, AlphaMode::Opaque, false)),
        ("blend", red_quad(&mut r, fog, 255, AlphaMode::Blend, false)),
        ("half", red_quad(&mut r, half, 128, AlphaMode::Blend, true)),
    ] {
        let path = format!("{prefix}_{name}.png");
        image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ExtendedColorType::Rgba8).unwrap();
    }
}
