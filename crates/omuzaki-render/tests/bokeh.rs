//! Bokeh (`DepthOfField::bokeh`) on the software adapter: a small light far behind the focal plane opens into a disc of its
//! blur radius, flat across and brighter than the gather's haze, while what is in focus keeps every byte. With
//! `OMUZAKI_RENDER_OUT=<dir>` the tests keep the pictures they render, for evidence.

// Integration tests may panic: a panic is a clear test failure. Pixel maths uses small counts in float arithmetic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod support;

use std::path::PathBuf;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, DepthOfField, Image, Instance, Material, Projection, Quality, Renderer,
    RendererExt, Scene,
};
use omuzaki_render_mock::shapes;

const SIZE: u32 = 512;
/// Sharp at 1.5 m (the stand-in for the character), blurring to the most the renderer draws.
const FOCUS: f32 = 1.5;
const RANGE: f32 = 0.3;
const MAX_BLUR: f32 = 0.07;
/// The light, 5.9 m from the camera, and the wall behind it at 6 m.
const LIGHT_DISTANCE: f32 = 5.9;

fn quad(r: &mut dyn Renderer, scene: &mut Scene, half: Vec2, at: Vec3, colour: u32) {
    let mesh = r.create_mesh(&shapes::quad(half)).unwrap();
    let material = r.create_material(&Material::unlit(Colour::hex(colour))).unwrap();
    scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(at)));
}

/// The camera at z = 3 looking down -z: a grey stand-in for the character in focus at the left, a small white light far
/// behind at the right, and (unless `wall` is false) a dark wall behind that.
fn scene(r: &mut dyn Renderer, background: Background, wall: bool) -> Scene {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 3.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.1, far: 50.0 },
    ));
    scene.background = background;
    if wall {
        quad(r, &mut scene, Vec2::new(5.0, 3.1), Vec3::new(0.0, 0.0, -3.0), 0x0018_1c2a);
    }
    quad(r, &mut scene, Vec2::splat(0.015), Vec3::new(0.6, 0.4, 3.0 - LIGHT_DISTANCE), 0x00ff_f4e0);
    quad(r, &mut scene, Vec2::new(0.1, 0.15), Vec3::new(-0.25, 0.0, 3.0 - FOCUS), 0x0080_8890);
    scene
}

fn dof(bokeh: f32) -> DepthOfField {
    DepthOfField { bokeh, ..DepthOfField::new(FOCUS, RANGE, MAX_BLUR) }
}

fn render(r: &mut dyn Renderer, scene: &Scene) -> Image {
    let quality = Quality { samples: 4, ..Quality::offline() };
    r.render_image(UVec2::splat(SIZE), AlphaEncoding::Straight, scene, &quality).unwrap()
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

/// The centre of the light in a sharp render: the mean place of its near-white pixels.
fn light_centre(sharp: &Image) -> Vec2 {
    let (mut sum, mut count) = (Vec2::ZERO, 0.0);
    for y in 0..SIZE {
        for x in 0..SIZE {
            if luma(sharp.pixel(x, y).unwrap()) > 200.0 {
                sum += Vec2::new(x as f32, y as f32);
                count += 1.0;
            }
        }
    }
    assert!(count > 4.0, "the light is in the frame");
    sum / count
}

/// The blur radius of the light in target pixels, as the pass computes it: the thin lens's blur at its distance times
/// the largest blur, which is `MAX_BLUR` of the half-size images' height.
fn disc_radius() -> f32 {
    let blur = ((LIGHT_DISTANCE - FOCUS).abs() - RANGE * 0.5) / LIGHT_DISTANCE;
    blur * MAX_BLUR * SIZE as f32
}

/// The mean luma round a ring of `radius` pixels about `centre`.
fn ring(image: &Image, centre: Vec2, radius: f32) -> f32 {
    let steps = 48;
    let mut total = 0.0;
    for i in 0..steps {
        let angle = std::f32::consts::TAU * i as f32 / steps as f32;
        let at = centre + Vec2::new(angle.cos(), angle.sin()) * radius;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // inside the frame
        let (x, y) = (at.x.round() as u32, at.y.round() as u32);
        total += luma(image.pixel(x, y).unwrap());
    }
    total / steps as f32
}

#[test]
fn a_light_behind_the_focus_opens_into_a_flat_disc_of_its_blur() {
    let mut r = support::renderer();
    let mut shot = scene(&mut r, Background::Solid { colour: Colour::hex(0x0010_1018) }, true);
    let sharp = render(&mut r, &shot);
    let centre = light_centre(&sharp);
    shot.post.depth_of_field = Some(dof(0.0));
    let haze = render(&mut r, &shot);
    shot.post.depth_of_field = Some(dof(4.0));
    let disc = render(&mut r, &shot);
    save("bokeh_sharp.png", &sharp);
    save("bokeh_off.png", &haze);
    save("bokeh_on.png", &disc);

    let radius = disc_radius();
    let gains: Vec<(f32, f32)> = [0.3, 0.6, 0.85, 1.3]
        .iter()
        .map(|&f| (f, ring(&disc, centre, f * radius) - ring(&haze, centre, f * radius)))
        .collect();
    println!("disc radius {radius:.1} px at {centre}; gain over the haze by fraction of it: {gains:?}");
    for &(f, gain) in &gains[..3] {
        assert!(gain > 8.0, "inside the disc ({f} of its radius) it is brighter than the haze: {gain}");
    }
    let (inner, edge) = (gains[0].1, gains[2].1);
    assert!(
        edge > 0.6 * inner,
        "the disc is flat to its edge, not a falling haze: {inner} inside, {edge} near the edge"
    );
    assert!(gains[3].1.abs() < 3.0, "outside the disc nothing changes: {}", gains[3].1);

    // In focus: the stand-in keeps every byte.
    for y in 200..312 {
        for x in 50..145 {
            assert_eq!(disc.pixel(x, y), haze.pixel(x, y), "the sharp stand-in at {x}, {y}");
        }
    }
}

#[test]
fn bokeh_off_keeps_the_gather_and_a_disc_shows_over_a_transparent_background() {
    let mut r = support::renderer();
    let mut shot = scene(&mut r, Background::Solid { colour: Colour::hex(0x0010_1018) }, true);
    shot.post.depth_of_field = Some(DepthOfField { bokeh_threshold: 0.1, ..dof(0.0) });
    let any_threshold = render(&mut r, &shot);
    shot.post.depth_of_field = Some(dof(0.0));
    let off = render(&mut r, &shot);
    assert_eq!(any_threshold.rgba8, off.rgba8, "with bokeh 0 the threshold changes nothing");

    let mut alone = scene(&mut r, Background::Transparent, false);
    let centre = light_centre(&render(&mut r, &alone));
    alone.post.depth_of_field = Some(dof(4.0));
    let image = render(&mut r, &alone);
    save("bokeh_transparent.png", &image);
    assert_eq!(image.pixel(500, 500).unwrap(), [0, 0, 0, 0], "far from everything: nothing");
    let radius = disc_radius();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // inside the frame
    let at = |f: f32| {
        let p = centre + Vec2::new(f * radius, 0.0);
        image.pixel(p.x.round() as u32, p.y.round() as u32).unwrap()
    };
    let inside = at(0.6);
    assert!(inside[3] > 20, "the disc has alpha over the transparent background: {inside:?}");
    assert!(luma(inside) > 100.0, "and it is the light's colour, not darkened by the empty background: {inside:?}");
    assert!(at(1.4)[3] < inside[3] / 4, "past the disc it fades out: {:?}", at(1.4));
}
