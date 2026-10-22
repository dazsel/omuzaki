//! Supersampling past the hardware's samples (`QualityMode::Offline` asks for 16): the geometry is drawn in rounds, each
//! moved by a fixed sub-pixel offset, and the rounds are averaged before the post passes. Checked on what it is for: a
//! thin line gets the sixteen coverage levels that hardware 4x cannot give it, the frame stays exact, the half-float average
//! keeps a flat region within a level, the post passes see the average, and the report counts every round. With
//! `OMUZAKI_RENDER_OUT=<dir>` the tests keep the line at both sample counts, enlarged, for evidence.

// Integration tests may panic: a panic is a clear test failure. Pixel maths uses small counts in float arithmetic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod support;

use std::collections::BTreeSet;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Bloom, Camera, Colour, Image, Instance, Material, Projection, Quality, Renderer,
    RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::shapes;

/// The picture's side, in pixels; the orthographic camera sees 2 m of it, so a metre is `SIZE / 2` pixels.
const SIZE: u32 = 64;

fn camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    )
}

/// Pixels to metres at this camera.
fn metres(pixels: f32) -> f32 {
    pixels * 2.0 / SIZE as f32
}

/// A white line one pixel wide across the picture, turned off the axes so it crosses pixels at every offset, over black
/// with no tone mapping: each pixel's level is how much of it the line covers.
fn line_scene(r: &mut dyn Renderer) -> Scene {
    let mut scene = Scene::new(camera());
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    let mesh = r.create_mesh(&shapes::quad(Vec2::new(metres(40.0), metres(0.5)))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let mut line = Instance::new(mesh, material, Mat4::from_rotation_z(0.37));
    line.casts_shadow = false;
    scene.instances.push(line);
    scene
}

fn render(r: &mut dyn Renderer, scene: &Scene, quality: &Quality) -> Image {
    r.render_image(UVec2::splat(SIZE), AlphaEncoding::Straight, scene, quality).unwrap()
}

/// The distinct red levels in the picture: the line's coverage levels, with 0 for the pixels it misses.
fn levels(image: &Image) -> BTreeSet<u8> {
    image.rgba8.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
}

/// `image` enlarged `by` times with square pixels, so the coverage levels can be seen.
fn enlarged(image: &Image, by: u32) -> Vec<u8> {
    let side = image.size.x * by;
    let mut out = Vec::with_capacity((side * image.size.y * by * 4) as usize);
    for y in 0..image.size.y * by {
        for x in 0..side {
            out.extend_from_slice(&image.pixel(x / by, y / by).unwrap());
        }
    }
    out
}

fn hardware(r: &dyn Renderer) -> u8 {
    r.capabilities().max_samples
}

#[test]
fn sixteen_samples_give_a_thin_line_sixteen_coverage_levels() {
    let mut r = support::renderer();
    let scene = line_scene(&mut r);
    let hardware = hardware(&r);
    let alone = render(&mut r, &scene, &Quality { samples: hardware, ..Quality::offline() });
    let full = render(&mut r, &scene, &Quality::offline());
    support::keep(&format!("supersample_line_{hardware}x.png"), (SIZE * 8, SIZE * 8), &enlarged(&alone, 8));
    support::keep("supersample_line_16x.png", (SIZE * 8, SIZE * 8), &enlarged(&full, 8));
    let (few, many) = (levels(&alone), levels(&full));
    println!("{hardware}x hardware: {} levels {few:?}; 16 samples: {} levels {many:?}", few.len(), many.len());
    assert!(
        few.len() <= usize::from(hardware) + 1,
        "the hardware alone gives at most {} levels: {few:?}",
        hardware + 1
    );
    assert!(many.len() >= 16, "16 samples give 16 coverage levels or more (17 with none): {many:?}");
}

#[test]
fn a_supersampled_frame_is_exact_and_does_not_depend_on_the_frame_index() {
    let mut r = support::renderer();
    let mut scene = line_scene(&mut r);
    let first = render(&mut r, &scene, &Quality::offline());
    let again = render(&mut r, &scene, &Quality::offline());
    assert_eq!(first.rgba8, again.rgba8, "two runs are byte-identical");
    scene.frame.index = 12_345;
    let later = render(&mut r, &scene, &Quality::offline());
    assert_eq!(first.rgba8, later.rgba8, "a frame later in a clip draws its samples in the same places");
}

#[test]
fn the_average_stays_within_a_level_on_a_flat_region_whatever_the_round_count() {
    let mut r = support::renderer();
    let mut scene = Scene::new(camera());
    scene.background = Background::Solid { colour: Colour::hex(0x0020_2840) };
    // A colour between levels after the average, where half-float rounding would show first.
    let material = r.create_material(&Material::unlit(Colour::hex(0x0081_5a3f))).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(metres(20.0)))).unwrap();
    let mut panel = Instance::new(mesh, material, Mat4::from_rotation_z(0.2));
    panel.casts_shadow = false;
    scene.instances.push(panel);
    let hardware = hardware(&r);
    let reference = render(&mut r, &scene, &Quality { samples: hardware, ..Quality::offline() });
    for samples in [hardware * 2, 16].into_iter().filter(|&s| s > hardware && s <= 16) {
        let image = render(&mut r, &scene, &Quality { samples, ..Quality::offline() });
        // The middle of the panel and a corner of the background: far from any edge. Half-float rounding of the sum is
        // a few parts in ten thousand of the value, well under half an output level, but a value that sits right on a
        // level's boundary can still tip to the next byte: at most one level, never more.
        for (x, y) in [(32, 32), (28, 36), (36, 28), (2, 2), (61, 61)] {
            let (a, b) = (image.pixel(x, y).unwrap(), reference.pixel(x, y).unwrap());
            let most = a.iter().zip(&b).map(|(p, q)| p.abs_diff(*q)).max().unwrap_or(0);
            assert!(most <= 1, "{samples} samples at ({x}, {y}): {a:?} against {b:?}");
        }
    }
}

#[test]
fn the_post_passes_read_the_average() {
    let mut r = support::renderer();
    let mut scene = Scene::new(camera());
    scene.background = Background::Solid { colour: Colour::hex(0x001b_2236) };
    scene.post.bloom = Some(Bloom { strength: 0.6, radius: 0.5, threshold: 0.8 });
    let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::new(metres(10.0), metres(1.5)))).unwrap();
    let mut bar = Instance::new(mesh, material, Mat4::from_rotation_z(0.3));
    bar.casts_shadow = false;
    scene.instances.push(bar);
    let hardware = hardware(&r);
    let alone = render(&mut r, &scene, &Quality { samples: hardware, ..Quality::offline() });
    let full = render(&mut r, &scene, &Quality::offline());
    // The glow around the bar is there, and the same: the post passes read the averaged picture, not one round of it.
    let glow = |image: &Image, x, y| f32::from(image.pixel(x, y).unwrap()[0]);
    let background = glow(&full, 2, 2);
    for (x, y) in [(32, 22), (32, 42), (12, 32), (52, 32)] {
        assert!(glow(&full, x, y) > background + 3.0, "({x}, {y}) glows: {} over {background}", glow(&full, x, y));
        let difference = (glow(&full, x, y) - glow(&alone, x, y)).abs();
        assert!(difference <= 2.0, "({x}, {y}): the glow differs by {difference} levels");
    }
    // And the picture is the supersampled one: its edges differ from the hardware's.
    assert_ne!(alone.rgba8, full.rgba8, "the edges are drawn with more samples");
}

#[test]
fn the_report_counts_every_round() {
    let mut r = support::renderer();
    let scene = line_scene(&mut r);
    let hardware = hardware(&r);
    let rounds = 16 / u32::from(hardware);
    let size = UVec2::splat(SIZE);
    let target = r.create_target(&omuzaki_render_api::TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
    let mut frame = |scene: &Scene, quality: &Quality| r.render(target, scene, quality).unwrap();
    let hardware_only = Quality { samples: hardware, ..Quality::offline() };
    let (once, all) = (frame(&scene, &hardware_only), frame(&scene, &Quality::offline()));
    // The same frames with nothing in them: what the frame draws whatever the scene (`Resolve`, and one accumulation
    // per round when it supersamples).
    let empty = Scene { instances: Vec::new(), ..scene.clone() };
    let (bare_once, bare_all) = (frame(&empty, &hardware_only), frame(&empty, &Quality::offline()));
    assert_eq!(bare_all.draw_calls, bare_once.draw_calls + rounds, "each round's accumulation is a draw");
    assert_eq!(all.render_size, size);
    assert_eq!(
        all.draw_calls - bare_all.draw_calls,
        rounds * (once.draw_calls - bare_once.draw_calls),
        "{rounds} rounds draw the line {rounds} times"
    );
    assert_eq!(all.triangles, once.triangles * u64::from(rounds));
    assert!(all.cpu.get(omuzaki_render_api::Pass::Opaque).is_some());
}
