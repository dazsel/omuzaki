//! The material UV transform on the GPU renderer, against the mock. The atlas quad (red | blue) is drawn at UV offsets 0
//! and 0.5 by both renderers: the GPU picture must match the mock's pixel for pixel, and the mock's must show the cell
//! the transform picks, so the comparison cannot pass on two blank images.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Filter, Image, Instance, Material, Projection, Quality, Renderer,
    RendererExt, Sampler, Scene, TextureDesc, Tonemap, UvTransform,
};
use omuzaki_render_mock::{MockRenderer, shapes};

const SIZE: u32 = 64;
/// Offsets along u: the quad then shows the red cell, then the blue one.
const OFFSETS: [(f32, [u8; 3]); 2] = [(0.0, [255, 0, 0]), (0.5, [0, 0, 255])];

/// Renders a 2 x 1 atlas, red | blue, on a quad that covers it whole, with the base texture read through a UV scale of
/// 0.5 along u and `offset` added after it. Same camera as the conformance suite: 32 pixels to a world unit.
fn atlas_image(r: &mut dyn Renderer, offset: f32) -> Image {
    // Hard texels: the mock reads the nearest texel whatever the filter, so a linear atlas would blend where it does not.
    let desc = TextureDesc {
        sampler: Sampler { filter: Filter::Nearest, ..Sampler::default() },
        ..TextureDesc::colour(UVec2::new(2, 1))
    };
    let atlas = r.create_texture(&desc, Some(&[255, 0, 0, 255, 0, 0, 255, 255])).unwrap();
    let quad = r.create_mesh(&shapes::quad(Vec2::splat(0.5))).unwrap();
    let material = Material {
        base_texture: Some(atlas),
        uv_transform: UvTransform { scale: Vec2::new(0.5, 1.0), offset: Vec2::new(offset, 0.0) },
        ..Material::unlit(Colour::WHITE)
    };
    let material = r.create_material(&material).unwrap();
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    scene.instances.push(Instance::new(quad, material, Mat4::IDENTITY));
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() };
    let img = r.render_image(UVec2::splat(SIZE), AlphaEncoding::Straight, &scene, &quality).unwrap();
    for resource in [material.into(), quad.into(), atlas.into()] {
        r.release(resource).unwrap();
    }
    img
}

/// The colour at `(x, y)`, which must be inside the image.
fn colour_at(img: &Image, x: u32, y: u32) -> Colour {
    img.colour(x, y).expect("the pixel is inside the image")
}

/// The largest channel difference between two images of the same size, and how many channels differ at all.
fn differences(a: &Image, b: &Image) -> (u8, usize) {
    assert_eq!(a.rgba8.len(), b.rgba8.len(), "the images differ in size");
    a.rgba8.iter().zip(&b.rgba8).fold((0, 0), |(largest, count), (&x, &y)| {
        let d = x.abs_diff(y);
        (largest.max(d), count + usize::from(d > 0))
    })
}

#[test]
fn the_mock_shows_the_cell_the_transform_picks() {
    let mut mock = MockRenderer::new();
    for (offset, [r, g, b]) in OFFSETS {
        let img = atlas_image(&mut mock, offset);
        // Pixels 12 either side of the centre sit well inside the quad and away from the cell edge.
        for x in [SIZE / 2 - 12, SIZE / 2 + 12] {
            assert_eq!(colour_at(&img, x, SIZE / 2), Colour::rgb(r, g, b), "offset {offset}, pixel column {x}");
        }
    }
}

#[test]
fn the_gpu_matches_the_mock_pixel_for_pixel_at_offsets_0_and_0_5() {
    let mut mock = MockRenderer::new();
    let mut gpu = support::renderer();
    // The mock is exact; the GPU may differ by rounding when the renderer is not bit-exact (the conformance tolerance).
    let tolerance = if gpu.capabilities().bit_exact { 0 } else { 2 };
    for (offset, _) in OFFSETS {
        let reference = atlas_image(&mut mock, offset);
        let picture = atlas_image(&mut gpu, offset);
        let (largest, count) = differences(&reference, &picture);
        assert!(
            largest <= tolerance,
            "offset {offset}: largest channel difference {largest} > {tolerance} ({count} channels)"
        );
    }
}
