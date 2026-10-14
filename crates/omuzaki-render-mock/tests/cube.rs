//! The example from the contract: render a cube through the mock to a PNG, and check what is in it.
//!
//! `tests/golden/cube.png` is the committed result (and the picture in `docs/design/render-api.md`). A deliberate
//! change to the mock's look is blessed with `OMUZAKI_BLESS=1 cargo test -p omuzaki-render-mock`, which rewrites it.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Light, Material, Outline, Quality, Renderer,
    RendererExt, Rim, Scene, Shading, Toon,
};
use omuzaki_render_mock::{MockRenderer, shapes};

const SIZE: UVec2 = UVec2::new(192, 192);
/// Cross-platform slack for the float functions the standard library doesn't pin down (`powf` in the sRGB curve).
const TOLERANCE: u8 = 2;

/// A lavender toon cube with a warm key from the upper left, a cool fill from the right and a dark outline,
/// lit the way the prototype's face-cam rig is.
fn render_cube(quality: &Quality, rim_strength: f32) -> Image {
    let mut renderer = MockRenderer::new();
    let mesh = renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.5))).unwrap();
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.shading = Shading::Toon(Toon {
        shade: Colour::hex(0x0088_78b8),
        rim: Rim { strength: rim_strength, ..Rim::default() },
        ..Toon::default()
    });
    material.outline = Some(Outline { colour: Colour::hex(0x0043_3a6e), width: 0.012 });
    let material = renderer.create_material(&material).unwrap();

    let mut scene =
        Scene::new(Camera::look_at(Vec3::new(2.4, 1.8, 3.2), Vec3::ZERO, Vec3::Y, Camera::default().projection));
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

    let image = renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, quality).unwrap();
    renderer.release(mesh.into()).unwrap();
    renderer.release(material.into()).unwrap();
    let left = renderer.stats();
    assert_eq!([left.meshes, left.textures, left.materials, left.targets], [0; 4], "the example leaks nothing");
    image
}

fn save_png(image: &Image, path: &Path) {
    image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cube.png")
}

#[test]
fn the_cube_matches_its_golden_png() {
    let image = render_cube(&Quality::offline(), Rim::default().strength);
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        save_png(&image, &golden_path());
    }
    let golden = image::open(golden_path()).unwrap().to_rgba8();
    assert_eq!((golden.width(), golden.height()), (SIZE.x, SIZE.y));
    let worst = golden.as_raw().iter().zip(&image.rgba8).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    if worst > TOLERANCE {
        let actual = std::env::temp_dir().join("omuzaki-render-mock-cube-actual.png");
        save_png(&image, &actual);
        panic!(
            "the cube differs from its golden by up to {worst} levels; see {actual:?}, or bless with OMUZAKI_BLESS=1"
        );
    }
}

#[test]
fn a_png_round_trip_keeps_every_byte() {
    let image = render_cube(&Quality::live(), Rim::default().strength);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cube.png");
    save_png(&image, &path);
    assert_eq!(image::open(&path).unwrap().to_rgba8().as_raw(), &image.rgba8);
}

#[test]
fn rendering_twice_gives_identical_bytes() {
    let quality = Quality::offline();
    assert_eq!(render_cube(&quality, 0.18), render_cube(&quality, 0.18));
}

#[test]
fn flat_toon_bands_and_an_outline_are_the_only_colours() {
    // One sample per pixel, no rim: the picture is the backdrop, the outline and one flat colour per visible face.
    let image = render_cube(&Quality { samples: 1, ..Quality::offline() }, 0.0);
    let mut distinct: Vec<[u8; 4]> = image.rgba8.as_chunks::<4>().0.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 5, "backdrop + outline + three lit faces, got {distinct:?}");
}

#[test]
fn the_cube_stands_out_from_the_backdrop() {
    let image = render_cube(&Quality::live(), Rim::default().strength);
    let corner = image.colour(2, 2).unwrap();
    let centre = image.colour(SIZE.x / 2, SIZE.y / 2).unwrap();
    assert!(corner.r < 60 && corner.g < 60 && corner.b < 80, "the backdrop is dark: {corner}");
    assert!(centre.r > 150, "the cube is bright at the centre: {centre}");
    assert!(centre.b > centre.r, "lavender stays lavender, not grey: {centre}");
}
