//! Lens glare (`Post::glare`) on the software adapter: a star of streaks whose count and angle follow the settings,
//! ghosts across the frame's centre and a halo around the light, nothing from what is under the threshold, the same bytes
//! as no glare when it is off,
//! and a glow that reaches past a transparent background like bloom's. With `OMUZAKI_RENDER_OUT=<dir>` the tests keep
//! the pictures they render, for evidence.

// Integration tests may panic: a panic is a clear test failure. Pixel maths uses small counts in float arithmetic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod support;

use std::path::PathBuf;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Bloom, Camera, Colour, Glare, Image, Instance, Material, Projection, Quality, Renderer,
    RendererExt, Scene,
};
use omuzaki_render_mock::shapes;

const SIZE: u32 = 512;
const BACKDROP: u32 = 0x0010_1420;
/// A white light, a square 16 pixels a side, centred at `LIGHT`.
const LIGHT: (u32, u32) = (256, 256);
/// A dim panel, well under the threshold.
const DIM: u32 = 0x0060_5a70;
/// White counts as bright: the threshold sits just under it, as bloom's prototype does.
const THRESHOLD: f32 = 0.9;

/// A square `side` pixels across centred at `(x, y)` of a 512 x 512 image, by an orthographic camera 2 m tall.
fn square(r: &mut dyn Renderer, scene: &mut Scene, (x, y): (u32, u32), side: u32, colour: u32, z: f32) {
    let px = |v: u32| f32::from(u16::try_from(v).unwrap());
    let half = px(SIZE / 2);
    let centre = Vec2::new(px(x) - half, half - px(y)) / half;
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(px(side) / (2.0 * half)))).unwrap();
    let material = r.create_material(&Material::unlit(Colour::hex(colour))).unwrap();
    scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(centre.extend(z))));
}

fn empty_scene(background: Background) -> Scene {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    ));
    scene.background = background;
    scene
}

/// A dim panel across the middle of the frame with a white light at `at` on it.
fn light_scene(r: &mut dyn Renderer, at: (u32, u32)) -> Scene {
    let mut scene = empty_scene(Background::Solid { colour: Colour::hex(BACKDROP) });
    square(r, &mut scene, (256, 256), 200, DIM, 0.0);
    square(r, &mut scene, at, 16, 0x00ff_ffff, 0.01);
    scene
}

fn glare(streaks: u32, rotation_deg: f32) -> Glare {
    Glare { threshold: THRESHOLD, streaks, rotation_deg, length: 0.3, ..Glare::default() }
}

fn render(r: &mut dyn Renderer, scene: &Scene, encoding: AlphaEncoding) -> Image {
    r.render_image(UVec2::splat(SIZE), encoding, scene, &Quality { samples: 4, ..Quality::offline() }).unwrap()
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

/// How much brighter `on` is than `off` at `(x, y)`, in 8-bit luma.
fn brighter(on: &Image, off: &Image, (x, y): (u32, u32)) -> f32 {
    luma(on.pixel(x, y).unwrap()) - luma(off.pixel(x, y).unwrap())
}

/// The mean brightening over the points `distance` pixels out from the light along `degrees` (anticlockwise from +x, y
/// up), both ways.
fn along(on: &Image, off: &Image, degrees: f32, distance: f32) -> f32 {
    let (s, c) = degrees.to_radians().sin_cos();
    let mut total = 0.0;
    for sign in [-1.0f32, 1.0] {
        let x = LIGHT.0 as f32 + sign * c * distance;
        let y = LIGHT.1 as f32 - sign * s * distance;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // inside the 512-pixel frame
        let at = (x.round() as u32, y.round() as u32);
        total += brighter(on, off, at);
    }
    total / 2.0
}

#[test]
fn the_streaks_follow_the_count_and_the_angle() {
    let mut r = support::renderer();
    let mut scene = light_scene(&mut r, LIGHT);
    let off = render(&mut r, &scene, AlphaEncoding::Straight);
    for (streaks, rotation, name) in
        [(2, 0.0, "2_at_0"), (2, 90.0, "2_at_90"), (4, 0.0, "4_at_0"), (6, 15.0, "6_at_15")]
    {
        scene.post.glare = Some(glare(streaks, rotation));
        let on = render(&mut r, &scene, AlphaEncoding::Straight);
        save(&format!("glare_{name}.png"), &on);
        // Each axis of a star of `streaks` sits `180 / axes` degrees from the last, starting at the rotation.
        let axes = streaks / 2;
        let step = 180.0 / axes as f32;
        for distance in [30.0, 60.0] {
            for k in 0..axes {
                let degrees = rotation + step * k as f32;
                let on_axis = along(&on, &off, degrees, distance);
                assert!(on_axis > 6.0, "{name}: {distance} px out along {degrees} degrees: {on_axis}");
                let between = along(&on, &off, degrees + step / 2.0, distance);
                assert!(
                    between < on_axis / 3.0,
                    "{name}: half-way between the axes at {distance} px is darker ({between} against {on_axis})"
                );
            }
        }
        // A streak fades along its length.
        let (near, far) = (along(&on, &off, rotation, 30.0), along(&on, &off, rotation, 120.0));
        assert!(far < near, "{name}: the streak fades ({near} at 30 px, {far} at 120 px)");
    }
}

#[test]
fn glare_off_or_under_the_threshold_changes_no_byte() {
    let mut r = support::renderer();
    let mut scene = light_scene(&mut r, LIGHT);
    let off = render(&mut r, &scene, AlphaEncoding::Straight);
    for (case, look) in
        [("zero strength", Glare { strength: 0.0, ..glare(4, 0.0) }), ("no streaks, halo or ghosts", glare(0, 0.0))]
    {
        scene.post.glare = Some(look);
        let image = render(&mut r, &scene, AlphaEncoding::Straight);
        assert_eq!(image.rgba8, off.rgba8, "{case}: the frame is byte-identical to no glare");
    }
    // A glare that runs adds nothing under its threshold; the frame differs only by the one-level dither every glow
    // turns on (so a glow's gradients do not band), as bloom's does.
    scene.post.glare = Some(Glare { threshold: 4.0, ..glare(8, 0.0) });
    let image = render(&mut r, &scene, AlphaEncoding::Straight);
    let worst = image.rgba8.iter().zip(&off.rgba8).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(worst <= 1, "everything under the threshold: at most the dither's one level, not {worst}");

    // A scene of only dim surfaces flares nowhere.
    let mut dim = empty_scene(Background::Solid { colour: Colour::hex(BACKDROP) });
    square(&mut r, &mut dim, (256, 256), 200, DIM, 0.0);
    let plain = render(&mut r, &dim, AlphaEncoding::Straight);
    dim.post.glare = Some(Glare { halo: 1.0, ghosts: 1.0, ..glare(8, 0.0) });
    let flared = render(&mut r, &dim, AlphaEncoding::Straight);
    let worst = flared.rgba8.iter().zip(&plain.rgba8).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(worst <= 1, "nothing over the threshold, nothing changes but the dither: {worst}");
}

#[test]
fn ghosts_fall_across_the_centre_and_the_halo_rings_it() {
    let mut r = support::renderer();
    // Off centre, so its ghosts land elsewhere: the first ghost scales it through the centre by -1.6.
    let at = (380, 140);
    let mut scene = light_scene(&mut r, at);
    scene.post.glare = Some(Glare { ghosts: 1.0, ..glare(2, 0.0) });
    let ghosts = render(&mut r, &scene, AlphaEncoding::Straight);
    scene.post.glare = Some(glare(2, 0.0));
    let streaks = render(&mut r, &scene, AlphaEncoding::Straight);
    save("glare_ghosts.png", &ghosts);
    let centre = SIZE as f32 / 2.0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // inside the frame
    let ghost = |factor: f32| {
        let x = centre + (at.0 as f32 - centre) / factor;
        let y = centre + (at.1 as f32 - centre) / factor;
        (x.round() as u32, y.round() as u32)
    };
    for factor in [-1.6, -0.7] {
        let gain = brighter(&ghosts, &streaks, ghost(factor));
        assert!(gain > 3.0, "the ghost at {factor} across the centre shows: {gain}");
    }
    assert!(brighter(&ghosts, &streaks, (60, 470)).abs() <= 1.0, "a corner with no ghost is untouched");

    scene.post.glare = Some(Glare { halo: 1.0, ..glare(2, 0.0) });
    let halo = render(&mut r, &scene, AlphaEncoding::Straight);
    save("glare_halo.png", &halo);
    // The ring sits 8 % of the height (41 pixels here) from the light, all round it; inside it, between, is darker.
    for (dx, dy) in [(41, 0), (-41, 0), (0, 41), (0, -41), (29, 29)] {
        let p = (at.0.saturating_add_signed(dx), at.1.saturating_add_signed(dy));
        let gain = brighter(&halo, &streaks, p);
        assert!(gain > 4.0, "the halo shows {dx}, {dy} px from the light: {gain}");
        let inside = (at.0.saturating_add_signed(dx / 2), at.1.saturating_add_signed(dy / 2));
        assert!(brighter(&halo, &streaks, inside) < gain / 2.0, "and inside the ring it is darker ({dx}, {dy})");
    }
}

#[test]
fn glare_adds_onto_bloom_and_alone_shows_through_a_transparent_background() {
    let mut r = support::renderer();
    let mut scene = light_scene(&mut r, LIGHT);
    let bloom = Bloom { strength: 0.35, radius: 0.5, threshold: 0.9 };
    scene.post.bloom = Some(bloom);
    let glow = render(&mut r, &scene, AlphaEncoding::Straight);
    scene.post.glare = Some(glare(4, 0.0));
    let both = render(&mut r, &scene, AlphaEncoding::Straight);
    save("glare_with_bloom.png", &both);
    assert!(along(&both, &glow, 0.0, 60.0) > 4.0, "the streak adds to the glow");
    assert!(along(&both, &glow, 45.0, 60.0) < 2.0, "and only along its axes");

    let mut alone = empty_scene(Background::Transparent);
    square(&mut r, &mut alone, LIGHT, 16, 0x00ff_ffff, 0.0);
    alone.post.glare = Some(glare(4, 0.0));
    let image = render(&mut r, &alone, AlphaEncoding::Straight);
    save("glare_transparent.png", &image);
    assert_eq!(image.pixel(10, 10).unwrap(), [0, 0, 0, 0], "far from the light: nothing");
    assert_eq!(image.pixel(256, 256).unwrap()[3], 255, "the light itself is opaque");
    let streak = image.pixel(256 + 40, 256).unwrap()[3];
    assert!(streak > 0 && streak < 255, "the streak has alpha past the light: {streak}");
    let diagonal = image.pixel(256 + 40, 256 - 40).unwrap()[3];
    assert!(diagonal < streak, "off its axes it is fainter ({diagonal} against {streak})");
}
