//! The Transparent pass on the software adapter: blended instances sort back to front, test depth without
//! writing it, and light like opaque ones. The scenes are overlapping blended quads (both submission orders, an authored
//! `Instance::order`, an opaque wall in front of and behind them) and the mock character with its hair and garments
//! turned translucent. `OMUZAKI_RENDER_OUT=<dir>` saves every image the tests render, for a contact sheet.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, MaterialName, RegistryBuilder};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, ExtraAttribute, Image, Instance, JointRange, Light, Material,
    MaterialId, MeshData, MeshId, Pass, ProgramParams, Projection, Quality, Renderer, RendererExt, Scene, Shading,
    Tonemap, Toon,
};
use omuzaki_render_mock::{MockRenderer, shapes};

const SIZE: UVec2 = UVec2::new(64, 64);
const CENTRE: (u32, u32) = (32, 32);

/// One GPU test at a time in this binary: software Vulkan compiles pipelines on the calling thread and racing devices
/// crashed the driver once (see `toon.rs`).
fn serial() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(PoisonError::into_inner)
}

fn save(image: &Image, name: &str) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        let (w, h) = (image.size.x, image.size.y);
        image::save_buffer(PathBuf::from(dir).join(name), &image.rgba8, w, h, image::ColorType::Rgba8).unwrap();
    }
}

/// Exact colours (no tone curve) on black, the camera 2 m in front of the origin.
fn quad_scene() -> Scene {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 2.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    ));
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    scene
}

/// One sample, no effects: pixels are exactly covered or not.
fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() }
}

fn pixel(image: &Image, (x, y): (u32, u32)) -> [u8; 4] {
    let at = ((y * image.size.x + x) * 4) as usize;
    [image.rgba8[at], image.rgba8[at + 1], image.rgba8[at + 2], image.rgba8[at + 3]]
}

fn max_diff(a: &Image, b: &Image) -> u8 {
    a.rgba8.iter().zip(&b.rgba8).map(|(x, y)| x.abs_diff(*y)).max().unwrap()
}

/// A half-transparent unlit material of `colour`.
fn blended(r: &mut dyn Renderer, colour: Colour) -> MaterialId {
    let mut material = Material::unlit(colour);
    material.alpha = AlphaMode::Blend;
    r.create_material(&material).unwrap()
}

struct Quads {
    mesh: MeshId,
    /// Green, half transparent.
    far: MaterialId,
    /// Red, half transparent.
    near: MaterialId,
    wall: MaterialId,
}

fn quads(r: &mut dyn Renderer) -> Quads {
    Quads {
        mesh: r.create_mesh(&shapes::quad(Vec2::splat(0.4))).unwrap(),
        far: blended(r, Colour::rgba(0, 255, 0, 128)),
        near: blended(r, Colour::rgba(255, 0, 0, 128)),
        wall: r.create_material(&Material::unlit(Colour::rgb(0, 0, 255))).unwrap(),
    }
}

fn at(mesh: MeshId, material: MaterialId, z: f32) -> Instance {
    Instance::new(mesh, material, Mat4::from_translation(Vec3::new(0.0, 0.0, z)))
}

fn render(r: &mut dyn Renderer, instances: impl IntoIterator<Item = Instance>) -> Image {
    let mut scene = quad_scene();
    scene.instances.extend(instances);
    r.render_image(SIZE, AlphaEncoding::Straight, &scene, &plain()).unwrap()
}

#[test]
fn overlapping_quads_look_the_same_in_either_submission_order_and_the_nearer_one_dominates() {
    let _serial = serial();
    let mut r = support::renderer();
    let q = quads(&mut r);
    let (far, near) = (at(q.mesh, q.far, -0.5), at(q.mesh, q.near, 0.5));
    let one = render(&mut r, [far, near]);
    let two = render(&mut r, [near, far]);
    save(&one, "quads-far-then-near-submitted.png");
    save(&two, "quads-near-then-far-submitted.png");
    assert_eq!(one.rgba8, two.rgba8, "blended quads must not depend on submission order");
    let [red, green, blue, _] = pixel(&one, CENTRE);
    assert!(red > green && blue == 0, "red (near) over green (far) over black: {:?}", pixel(&one, CENTRE));
    // Green over black is 128 of 255 in linear light, red over that is 128 more: red ~ 0.5 and green ~ 0.25 linear.
    let swapped = render(&mut r, [at(q.mesh, q.far, 0.5), at(q.mesh, q.near, -0.5)]);
    save(&swapped, "quads-green-near.png");
    let [red, green, ..] = pixel(&swapped, CENTRE);
    assert!(green > red, "moving the green quad nearer must put it on top: {:?}", pixel(&swapped, CENTRE));
}

#[test]
fn an_authored_order_beats_distance_and_a_blended_surface_does_not_write_depth() {
    let _serial = serial();
    let mut r = support::renderer();
    let q = quads(&mut r);
    // Red is nearer. With `order` 1 on red and 0 on green, green draws first and red over it (the distance order);
    // with the orders swapped red draws first and green is drawn over it although it is farther. That second draw
    // only shows if the red quad left the depth buffer alone.
    let mut red = at(q.mesh, q.near, 0.5);
    let mut green = at(q.mesh, q.far, -0.5);
    (red.order, green.order) = (1, 0);
    let by_distance = render(&mut r, [red, green]);
    (red.order, green.order) = (0, 1);
    let by_order = render(&mut r, [red, green]);
    save(&by_distance, "quads-order-agrees-with-distance.png");
    save(&by_order, "quads-order-overrides-distance.png");
    let [dr, dg, ..] = pixel(&by_distance, CENTRE);
    let [or, og, ..] = pixel(&by_order, CENTRE);
    assert!(dr > dg, "red on top by distance: {:?}", pixel(&by_distance, CENTRE));
    assert!(og > or, "green on top by authored order, through the red quad: {:?}", pixel(&by_order, CENTRE));
}

#[test]
fn blended_surfaces_test_against_opaque_depth() {
    let _serial = serial();
    let mut r = support::renderer();
    let q = quads(&mut r);
    let behind = render(&mut r, [at(q.mesh, q.wall, 0.0), at(q.mesh, q.near, -0.5)]);
    let wall = render(&mut r, [at(q.mesh, q.wall, 0.0)]);
    let front = render(&mut r, [at(q.mesh, q.wall, 0.0), at(q.mesh, q.near, 0.5)]);
    save(&behind, "wall-hides-blended-quad.png");
    save(&front, "blended-quad-over-wall.png");
    assert_eq!(behind.rgba8, wall.rgba8, "a blended quad behind an opaque one must not show");
    let [red, _, blue, _] = pixel(&front, CENTRE);
    assert!(red > 0 && blue > 0, "a blended quad in front blends with the wall: {:?}", pixel(&front, CENTRE));
}

#[test]
fn the_pass_runs_only_when_something_is_blended() {
    let _serial = serial();
    let mut r = support::renderer();
    let q = quads(&mut r);
    let target =
        r.create_target(&omuzaki_render_api::TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    for (instances, expected) in [(vec![at(q.mesh, q.wall, 0.0)], false), (vec![at(q.mesh, q.near, 0.0)], true)] {
        let mut scene = quad_scene();
        scene.instances = instances;
        let report = r.render(target, &scene, &plain()).unwrap();
        assert_eq!(report.cpu.get(Pass::Transparent).is_some(), expected);
    }
    assert!(r.capabilities().supports(Pass::Transparent));
}

#[test]
fn the_gpu_blend_matches_the_mock() {
    let _serial = serial();
    let mut gpu = support::renderer();
    let mut mock = MockRenderer::new();
    let mut images = Vec::new();
    for r in [&mut gpu as &mut dyn Renderer, &mut mock] {
        let q = quads(r);
        images.push(render(r, [at(q.mesh, q.far, -0.5), at(q.mesh, q.near, 0.5), at(q.mesh, q.wall, -1.0)]));
    }
    save(&images[0], "three-layers-gpu.png");
    save(&images[1], "three-layers-mock.png");
    let worst = max_diff(&images[0], &images[1]);
    assert!(worst <= 2, "the GPU and the mock differ by up to {worst} levels");
}

#[test]
fn a_blended_program_material_runs_its_hooks_and_blends() {
    // The hair program on a quad with its two strand attributes: blended, it must differ from the same quad drawn as
    // blended toon (the hooks ran), and show the background through it (alpha 128 of a bright colour over black is dim).
    let _serial = serial();
    let mut r = support::renderer();
    let mut quad = shapes::quad(Vec2::splat(0.4));
    let n = quad.positions.len();
    quad.extra = vec![
        ExtraAttribute { name: Slug::new("hair-uv").unwrap(), components: 2, data: vec![0.3; n * 2] },
        ExtraAttribute { name: Slug::new("hair-info").unwrap(), components: 3, data: [0.4, 0.8, 0.5].repeat(n) },
    ];
    let mesh = r.create_mesh(&quad).unwrap();
    let mut toon = Material::toon(Colour::rgba(200, 160, 90, 128));
    toon.alpha = AlphaMode::Blend;
    toon.double_sided = true;
    let mut hair = toon.clone();
    hair.shading = Shading::Program { name: Slug::new("hair").unwrap(), params: ProgramParams::new() };
    let mut light = quad_scene();
    light.lights = vec![Light::Hemisphere { sky: Colour::WHITE, ground: Colour::WHITE, intensity: 3.0 }];
    let mut images = Vec::new();
    for material in [toon, hair] {
        let material = r.create_material(&material).unwrap();
        let mut scene = light.clone();
        scene.instances.push(at(mesh, material, 0.0));
        images.push(r.render_image(SIZE, AlphaEncoding::Straight, &scene, &plain()).unwrap());
    }
    save(&images[1], "blended-hair-program.png");
    assert_ne!(
        pixel(&images[0], CENTRE),
        pixel(&images[1], CENTRE),
        "the program's hooks must run in the blended pass"
    );
    assert!(
        pixel(&images[1], CENTRE)[0] < 200,
        "half transparent over black stays dim: {:?}",
        pixel(&images[1], CENTRE)
    );
}

// ---- the mock character ----

/// The mock head's centre and radius (`tests/hair.rs`).
const HEAD_CENTRE: Vec3 = Vec3::new(0.0, 0.78, -0.02);
const HEAD_RADIUS: f32 = 0.19;

/// The top of a sphere from the pole down to `rings` of `total` rings, smooth normals, counter-clockwise from outside:
/// a cap of hair over the head. (The mock builds no `cap` hair itself, `tests/hair.rs` has the same gap.)
fn cap(radius: f32, segments: u32, rings: u32, total: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for ring in 0..=rings {
        let polar = std::f32::consts::PI * ring as f32 / total as f32;
        for segment in 0..=segments {
            let azimuth = std::f32::consts::TAU * segment as f32 / segments as f32;
            let normal = Vec3::new(polar.sin() * azimuth.cos(), polar.cos(), polar.sin() * azimuth.sin());
            mesh.positions.push(normal * radius);
            mesh.normals.push(normal);
        }
    }
    let row = segments + 1;
    for ring in 0..rings {
        for segment in 0..segments {
            let (a, b) = (ring * row + segment, (ring + 1) * row + segment);
            mesh.indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    mesh
}

const EYE: Vec3 = Vec3::new(0.0, 1.45, 1.6);
const TARGET: Vec3 = Vec3::new(0.0, 0.8, 0.0);
const CHARACTER_SIZE: UVec2 = UVec2::new(256, 256);

/// Which of the mock character's materials are drawn blended.
#[derive(Copy, Clone)]
enum See {
    Opaque,
    /// Hair and garments translucent: hair over the head, cloth over the body.
    Through,
}

/// The mock character in its rest pose, framed from the front and above. The hair and the garments are blended with
/// `base` alpha 0.5 when `see` says so; the hair is a dark violet so it reads against the skin.
fn character(see: See) -> Image {
    let mut renderer = support::renderer();
    let def = mock_character().unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let mut scene = Scene::new(Camera::look_at(
        EYE,
        TARGET,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far: 20.0 },
    ));
    scene.background = Background::Solid { colour: Colour::hex(0x0086_8b99) };
    scene.lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.9 },
        Light::Directional {
            to_light: Vec3::new(0.4, 0.8, 0.6).normalize(),
            colour: Colour::hex(0x00ff_f0e0),
            intensity: 2.0,
            casts_shadow: false,
        },
    ];
    let mut materials = BTreeMap::new();
    let mut instances = Vec::new();
    for part in &built.meshes {
        let name = part.mesh.material.clone();
        if !materials.contains_key(&name) {
            let mut spec = built.materials[&name].clone();
            spec.overlays.clear();
            spec.base_texture = None;
            spec.outline = None;
            if name.as_str() == "mock-hair" {
                // The cap carries no strand attributes, so it is plain toon.
                spec.shading = Shading::Toon(Toon::default());
                spec.base = Colour::rgb(96, 48, 150);
            }
            let mut material = spec.to_material(&|_| None, &|_| None).unwrap();
            let translucent = matches!(see, See::Through)
                && (name == MaterialName::new("mock-hair").unwrap() || name.as_str().starts_with("mock-garment"));
            if translucent {
                material.alpha = AlphaMode::Blend;
                material.double_sided = true;
                material.base.a = 128;
            }
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
        instances.push(instance);
    }
    let hair_cap = renderer.create_mesh(&cap(HEAD_RADIUS * 1.12, 48, 14, 24)).unwrap();
    let hair = materials[&MaterialName::new("mock-hair").unwrap()];
    instances.push(Instance::new(hair_cap, hair, Mat4::from_translation(HEAD_CENTRE)));
    scene.instances = instances;
    scene.post.tonemap = Tonemap::None;
    renderer.render_image(CHARACTER_SIZE, AlphaEncoding::Straight, &scene, &plain()).unwrap()
}

#[test]
fn translucent_hair_and_cloth_over_the_mock_character_match_the_golden() {
    let _serial = serial();
    let (solid, through) = (character(See::Opaque), character(See::Through));
    save(&solid, "character-opaque.png");
    save(&through, "character-translucent.png");
    assert_ne!(solid.rgba8, through.rgba8, "translucent hair and cloth must change the picture");

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/transparent-mock-character.png");
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        let (w, h) = (through.size.x, through.size.y);
        image::save_buffer(&path, &through.rgba8, w, h, image::ColorType::Rgba8).unwrap();
    }
    let golden = image::open(&path)
        .expect("tests/golden/transparent-mock-character.png (bless with OMUZAKI_BLESS=1)")
        .to_rgba8();
    assert_eq!(golden.dimensions(), (through.size.x, through.size.y));
    let worst = golden.as_raw().iter().zip(&through.rgba8).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 2, "the picture differs from its golden by up to {worst} levels (bless with OMUZAKI_BLESS=1)");
}

#[test]
fn the_translucent_character_is_the_same_picture_every_time() {
    // The layers of a garment are concentric, so their sort keys tie and the scene order breaks the tie: the picture
    // is a pure function of the scene.
    let _serial = serial();
    assert_eq!(character(See::Through).rgba8, character(See::Through).rgba8);
}
