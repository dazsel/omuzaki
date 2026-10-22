//! A magnified soft alpha edge on the face texture stays smooth. The face asks for 8x anisotropic filtering;
//! lavapipe's anisotropic path turned a face-on, magnified footprint into texel-sized steps along every soft edge, so
//! the renderer now caps anisotropy to 1 on a software adapter.
//!
//! The picture is an ellipse (soft-edged, and hard-edged with a one-texel step) with constant skin colour in every
//! texel, transparent ones included, so only the alpha varies and filtering cannot darken the edge. It is drawn from a static texture on a quad at about 2x and 4x
//! magnification. Over the page the blue channel gives the ellipse's coverage back, which is compared with a CPU
//! bilinear resample of the same texels: the hardware's linear filter must match it. The worst error against the reference bounds the edge: a stair-step is a texel-sized error.

// Integration tests may panic: a panic is a clear test failure.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

mod support;

use std::path::PathBuf;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Mapping, Material, Overlay, Projection, Quality,
    Renderer, RendererExt, Sampler, Scene, TextureDesc, Tonemap,
};
use omuzaki_render_mock::shapes;

const SIZE: UVec2 = UVec2::new(197, 121);
const VIEW: UVec2 = UVec2::new(1024, 512);
/// The view is 2 world units high, so one unit is 256 pixels.
const PX_PER_UNIT: f32 = 256.0;
const PAGE: [u8; 3] = [236, 236, 246];
/// The worst edge error allowed against the bilinear reference.
const LIMIT: f32 = 0.03;
const SKIN: [f32; 3] = [1.0, 0.78, 0.62];

/// How the ellipse's edge falls off.
#[derive(Clone, Copy)]
enum Edge {
    /// A ramp about one texel wide, the shape of an anti-aliased face outline.
    Soft,
    /// A step from clear to opaque between neighbouring texels.
    Hard,
}

/// The ellipse's straight-alpha coverage at texel `(x, y)`, quantised to 8 bits as the texture stores it.
fn coverage(edge: Edge, x: u32, y: u32) -> f32 {
    let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
    let size = Vec2::new(SIZE.x as f32, SIZE.y as f32);
    let head = (1.0 - ((p - size * 0.5) / (size * 0.47)).length()) * size.y * 0.47;
    let a = match edge {
        Edge::Soft => (head + 0.5).clamp(0.0, 1.0),
        Edge::Hard => f32::from(head >= 0.0),
    };
    (a * 255.0).round() / 255.0
}

fn texels(edge: Edge) -> Vec<u8> {
    let mut pixels = Vec::new();
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            let a = coverage(edge, x, y);
            let skin = SKIN.map(|c| (c * 255.0).round() as u8);
            pixels.extend([skin[0], skin[1], skin[2], (a * 255.0).round() as u8]);
        }
    }
    pixels
}

fn render(edge: Edge, scale: f32) -> Image {
    let mut r = support::renderer();
    // Anisotropy 8, as `face_texture_desc` asks.
    let desc = TextureDesc { sampler: Sampler { anisotropy: 8, ..Sampler::default() }, ..TextureDesc::colour(SIZE) };
    let texture = r.create_texture(&desc, Some(&texels(edge))).unwrap();
    let mut material = Material::unlit(Colour::rgb(PAGE[0], PAGE[1], PAGE[2]));
    material.overlays.push(Overlay { texture, mapping: Mapping::Uv, strength: 1.0 });
    let material = r.create_material(&material).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::new(0.4, 0.4 * SIZE.y as f32 / SIZE.x as f32))).unwrap();
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut scene = Scene::new(camera);
    scene.background = Background::Solid { colour: Colour::rgb(PAGE[0], PAGE[1], PAGE[2]) };
    scene.post.tonemap = Tonemap::None;
    scene.instances.push(Instance::new(mesh, material, Mat4::from_scale(Vec3::splat(scale))));
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() };
    r.render_image(VIEW, AlphaEncoding::Straight, &scene, &quality).unwrap()
}

fn linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The coverage the render shows at a pixel, from its blue channel (page 246, skin 158 in sRGB, mixed in linear light).
fn shown(image: &Image, x: u32, y: u32) -> f32 {
    let blue = linear(f32::from(image.pixel(x, y).unwrap()[2]) / 255.0);
    let (page, skin) = (linear(f32::from(PAGE[2]) / 255.0), linear(SKIN[2]));
    ((page - blue) / (page - skin)).clamp(0.0, 1.0)
}

/// Bilinear resample of the texels at texture coordinate `(u, v)` in texels, clamped at the border.
fn bilinear(edge: Edge, u: f32, v: f32) -> f32 {
    let (x, y) = ((u - 0.5).clamp(0.0, SIZE.x as f32 - 1.0), (v - 0.5).clamp(0.0, SIZE.y as f32 - 1.0));
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = ((x0 + 1).min(SIZE.x - 1), (y0 + 1).min(SIZE.y - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    coverage(edge, x0, y0) * (1.0 - fx) * (1.0 - fy)
        + coverage(edge, x1, y0) * fx * (1.0 - fy)
        + coverage(edge, x0, y1) * (1.0 - fx) * fy
        + coverage(edge, x1, y1) * fx * fy
}

/// The worst error against the bilinear reference over the ellipse's edge pixels.
fn edge_metrics(edge: Edge, image: &Image, scale: f32) -> f32 {
    let half = Vec2::new(0.4, 0.4 * SIZE.y as f32 / SIZE.x as f32) * scale * PX_PER_UNIT;
    let centre = Vec2::new(VIEW.x as f32, VIEW.y as f32) * 0.5;
    let mut worst = 0.0f32;
    // Rows through the middle third, over the left side of the ellipse, where the edge is steep: near the apex a row
    // legitimately rises and falls again.
    for y in 156..VIEW.y - 156 {
        for x in 0..VIEW.x / 2 - 100 {
            let uv = (Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - (centre - half)) / (half * 2.0)
                * Vec2::new(SIZE.x as f32, SIZE.y as f32);
            let (got, want) = (shown(image, x, y), bilinear(edge, uv.x, uv.y));
            if want > 0.02 && want < 0.98 {
                worst = worst.max((got - want).abs());
            }
        }
    }
    worst
}

fn save(name: &str, image: &Image) {
    let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") else { return };
    let path = PathBuf::from(dir).join(format!("{name}.png"));
    image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
}

#[test]
fn a_magnified_alpha_edge_matches_a_bilinear_resample_at_2x_and_4x() {
    for (kind, edge) in [("soft", Edge::Soft), ("hard", Edge::Hard)] {
        for (name, scale) in [("2x", 1.92), ("4x", 3.85)] {
            let image = render(edge, scale);
            save(&format!("edge-{kind}-{name}-{}", std::env::var("OMUZAKI_PANEL").unwrap_or_default()), &image);
            let worst = edge_metrics(edge, &image, scale);
            eprintln!("{kind} {name}: worst error {worst:.3}");
            // Bilinear filtering of 8-bit alpha agrees with the reference to a few 8-bit steps; the stair-step
            // artefact (anisotropic filtering on the edge) was a texel-sized error of about 0.45.
            assert!(worst < LIMIT, "{kind} {name}: the edge is off the bilinear reference by {worst}");
        }
    }
}
