//! The drawn look (`Post::stylize`) on the software adapter: each part checked against what it is defined to do. Flat
//! unlit panels under a clamp tone curve give exact display colours, so a part's effect can be measured on them: a
//! halftone's ink covers the share of a cell its darkness asks for, hatching only falls in the dark tones, posterize
//! steps the luma, a vignette darkens corners and leaves the centre, a fringe spreads red outward, paper is the same on
//! every frame, and alpha is never touched. With `OMUZAKI_RENDER_OUT=<dir>` the tests keep their pictures.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod support;

use std::path::PathBuf;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, FrameInfo, Halftone, Hatching, Image, Instance, Material, Paper,
    Projection, Quality, Renderer, RendererExt, Scene, SplitTone, Stylize, Tonemap, Vignette,
};
use omuzaki_render_mock::shapes;

const SIZE: u32 = 256;

/// Unlit panels (`[left, top, right, bottom]` in pixels, colour) over `background`, clamp tone mapping, exposure 1.
fn panels(r: &mut dyn Renderer, rects: &[([u32; 4], u32)], background: Background) -> Scene {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    ));
    scene.background = background;
    scene.post.tonemap = Tonemap::None;
    let half = (SIZE / 2) as f32;
    for (i, &([left, top, right, bottom], colour)) in rects.iter().enumerate() {
        let px = |v: u32| v as f32;
        let centre = Vec2::new(0.5 * (px(left) + px(right)) - half, half - 0.5 * (px(top) + px(bottom))) / half;
        let extent = Vec2::new(px(right - left), px(bottom - top)) / (2.0 * half);
        let mesh = r.create_mesh(&shapes::quad(extent)).unwrap();
        let material = r.create_material(&Material::unlit(Colour::hex(colour))).unwrap();
        scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(centre.extend(0.01 * i as f32))));
    }
    scene
}

fn full(r: &mut dyn Renderer, colour: u32) -> Scene {
    panels(r, &[([0, 0, SIZE, SIZE], colour)], Background::Solid { colour: Colour::BLACK })
}

fn render(r: &mut dyn Renderer, scene: &Scene) -> Image {
    r.render_image(UVec2::splat(SIZE), AlphaEncoding::Straight, scene, &Quality::offline()).unwrap()
}

fn save(name: &str, image: &Image) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        std::fs::create_dir_all(&dir).unwrap();
        let path = PathBuf::from(dir).join(name);
        image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
    }
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
}

/// The share of pixels inside `[left, top, right, bottom]` darker than `below` (luma, 0..255).
fn share_below(image: &Image, [left, top, right, bottom]: [u32; 4], below: f32) -> f32 {
    let mut dark = 0;
    for y in top..bottom {
        for x in left..right {
            if luma(image.pixel(x, y).unwrap()) < below {
                dark += 1;
            }
        }
    }
    dark as f32 / ((right - left) * (bottom - top)) as f32
}

#[test]
fn the_empty_look_changes_no_byte() {
    let mut r = support::renderer();
    let mut scene = full(&mut r, 0x0080_6070);
    let plain = render(&mut r, &scene);
    scene.post.stylize = Stylize {
        halftone: Some(Halftone { strength: 0.0, ..Halftone::default() }),
        vignette: Some(Vignette { strength: 0.0, ..Vignette::default() }),
        ..Stylize::default()
    };
    assert_eq!(render(&mut r, &scene), plain, "parts with no strength draw nothing");
}

#[test]
fn halftone_ink_covers_the_share_of_a_cell_its_darkness_asks_for_and_spares_light_tones() {
    let mut r = support::renderer();
    // Left half mid grey (display luma 0.5), right half white.
    let mut scene = panels(
        &mut r,
        &[([0, 0, SIZE / 2, SIZE], 0x0080_8080), ([SIZE / 2, 0, SIZE, SIZE], 0x00ff_ffff)],
        Background::Solid { colour: Colour::BLACK },
    );
    let dots = Halftone { cell: 0.04, angle_deg: 45.0, ink: Colour::BLACK, strength: 1.0, threshold: 1.0 };
    scene.post.stylize.halftone = Some(dots);
    let image = render(&mut r, &scene);
    save("stylize_halftone.png", &image);
    // Darkness (1 - 0.5) / 1 = 0.5: half of the grey is inked black (the grey itself has luma 128).
    let inked = share_below(&image, [8, 8, SIZE / 2 - 8, SIZE - 8], 64.0);
    assert!((inked - 0.5).abs() < 0.06, "half the grey is dots: {inked}");
    let white = share_below(&image, [SIZE / 2 + 8, 8, SIZE - 8, SIZE - 8], 250.0);
    assert!(white < 0.01, "white prints no dots: {white}");
    // A threshold under the grey's luma prints nothing on it.
    scene.post.stylize.halftone = Some(Halftone { threshold: 0.45, ..dots });
    let none = share_below(&render(&mut r, &scene), [8, 8, SIZE / 2 - 8, SIZE - 8], 64.0);
    assert!(none < 0.01, "above the threshold: no dots, got {none}");
}

#[test]
fn hatching_falls_only_in_the_dark_tones_and_crosses_in_the_darkest() {
    let mut r = support::renderer();
    // Three bands: light (0.8), dark (0.25), darkest (0.08) display luma.
    let mut scene = panels(
        &mut r,
        &[
            ([0, 0, 86, SIZE], 0x00cc_cccc),
            ([86, 0, 172, SIZE], 0x0040_4040),
            ([172, 0, SIZE, SIZE], 0x0014_1414),
        ],
        Background::Solid { colour: Colour::BLACK },
    );
    scene.post.stylize.hatching = Some(Hatching {
        spacing: 0.04,
        width: 0.6,
        angle_deg: 30.0,
        ink: Colour::BLACK,
        strength: 1.0,
        threshold: 0.4,
        cross_threshold: 0.15,
    });
    let image = render(&mut r, &scene);
    save("stylize_hatching.png", &image);
    let light = share_below(&image, [8, 8, 78, SIZE - 8], 150.0);
    let dark = share_below(&image, [94, 8, 164, SIZE - 8], 30.0);
    let darkest = share_below(&image, [180, 8, SIZE - 8, SIZE - 8], 10.0);
    assert!(light < 0.01, "no lines in the light tone: {light}");
    assert!(dark > 0.1, "lines in the dark tone: {dark}");
    assert!(darkest > dark, "the cross layer adds ink in the darkest: {darkest} vs {dark}");
}

#[test]
fn posterize_steps_the_luma_and_keeps_the_hue() {
    let mut r = support::renderer();
    let mut scene = panels(
        &mut r,
        &[([0, 0, SIZE / 2, SIZE], 0x0066_3322), ([SIZE / 2, 0, SIZE, SIZE], 0x0070_3826)],
        Background::Solid { colour: Colour::BLACK },
    );
    scene.post.stylize.posterize = 3;
    let image = render(&mut r, &scene);
    let (a, b) = (image.pixel(40, 40).unwrap(), image.pixel(200, 40).unwrap());
    assert!((luma(a) - luma(b)).abs() <= 1.0, "two near browns land on one step: {a:?} {b:?}");
    // The step is a third of the range (85 levels) and the colour is still a brown: red above green above blue.
    assert!((luma(a) - 85.0).abs() < 3.0, "{a:?}");
    assert!(a[0] > a[1] && a[1] > a[2], "hue kept: {a:?}");
}

#[test]
fn a_vignette_darkens_the_corners_and_leaves_the_centre() {
    let mut r = support::renderer();
    let mut scene = full(&mut r, 0x00ff_ffff);
    scene.post.stylize.vignette = Some(Vignette { strength: 0.5, radius: 0.5, softness: 0.8 });
    let image = render(&mut r, &scene);
    save("stylize_vignette.png", &image);
    assert_eq!(image.pixel(SIZE / 2, SIZE / 2).unwrap(), [255, 255, 255, 255], "the centre is untouched");
    let corner = luma(image.pixel(1, 1).unwrap());
    assert!(corner < 150.0, "the corner falls off: {corner}");
}

#[test]
fn a_fringe_spreads_red_outward_and_blue_inward_at_the_edges() {
    let mut r = support::renderer();
    let mut scene = panels(&mut r, &[([160, 96, 224, 160], 0x00ff_ffff)], Background::Solid { colour: Colour::BLACK });
    scene.post.stylize.fringe = 0.01;
    let image = render(&mut r, &scene);
    save("stylize_fringe.png", &image);
    // Just past the square's right edge (away from the centre) red shows and blue doesn't; just inside it, the reverse.
    let outside = image.pixel(225, 128).unwrap();
    let inside = image.pixel(223, 128).unwrap();
    assert!(outside[0] > 128 && outside[2] < 64, "red spills out: {outside:?}");
    assert!(inside[2] < 128 && inside[0] > 200, "blue pulls in: {inside:?}");
}

#[test]
fn paper_and_split_tone_are_the_same_on_every_frame_and_never_touch_alpha() {
    let mut r = support::renderer();
    // A panel over a transparent background.
    let mut scene = panels(&mut r, &[([64, 64, 192, 192], 0x00b0_a090)], Background::Transparent);
    scene.post.stylize.paper = Some(Paper { strength: 0.3, scale: 0.02, tint: Colour::hex(0x00f8_f0e0) });
    scene.post.stylize.split_tone = Some(SplitTone::default());
    let first = render(&mut r, &scene);
    save("stylize_paper.png", &first);
    scene.frame = FrameInfo { index: 99, ..scene.frame };
    assert_eq!(render(&mut r, &scene), first, "paper is a fixed surface, not grain");
    assert_eq!(first.pixel(8, 8).unwrap()[3], 0, "the background stays transparent");
    assert_eq!(first.pixel(128, 128).unwrap()[3], 255, "the panel stays opaque");
    let spread: Vec<f32> = (64..192).step_by(8).map(|x| luma(first.pixel(x, 128).unwrap())).collect();
    let (lo, hi) = spread.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    assert!(hi - lo > 4.0, "the paper has a grain: {spread:?}");
}
