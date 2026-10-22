//! Evidence pictures for the face target: a coloured, odd-sized face layer on a plane, seen head on, at 60 degrees
//! and across a zoom sweep through a mip transition. Each is drawn twice from the same bytes: from a static texture (what
//! the hardware does with straight alpha, the old behaviour) and from a dynamic painter target (the premultiplied path).
//! With `OMUZAKI_RENDER_OUT=<dir>` and `OMUZAKI_PANEL=<name>` it saves `<case>-<name>.png` for both. Without them it only
//! checks that the pictures differ where they should: at the soft edges.

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
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Mapping, Material, Overlay, PixelRect, Projection,
    Quality, Renderer, RendererExt, Sampler, Scene, TextureDesc, TextureId, Tonemap,
};
use omuzaki_render_mock::shapes;

/// An odd size, like the real face atlas (1548 x 1107).
const SIZE: UVec2 = UVec2::new(197, 121);

/// The signed distance to a disc, turned into one pixel of soft coverage.
fn coverage(x: u32, y: u32, centre: Vec2, radius: f32) -> f32 {
    (radius - Vec2::new(x as f32 + 0.5, y as f32 + 0.5).distance(centre) + 0.5).clamp(0.0, 1.0)
}

/// A cartoon face as straight-alpha RGBA on transparent black: skin, two blue eyes with dark rims, a red mouth, a green
/// hair clip. Every shape has a one-texel soft edge, which is where a dark fringe would show.
fn face_layer() -> Vec<u8> {
    let mut pixels = Vec::with_capacity(SIZE.x as usize * SIZE.y as usize * 4);
    let size = Vec2::new(SIZE.x as f32, SIZE.y as f32);
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            // Painter's order: each layer lies over the last, straight-alpha "over".
            let mut rgba = [0.0f32; 4];
            let mut over = |colour: [f32; 3], a: f32| {
                let out = a + rgba[3] * (1.0 - a);
                if out > 0.0 {
                    for c in 0..3 {
                        rgba[c] = (colour[c] * a + rgba[c] * rgba[3] * (1.0 - a)) / out;
                    }
                }
                rgba[3] = out;
            };
            let head = (1.0 - ((p - size * 0.5) / (size * 0.47)).length()) * size.y * 0.47;
            over([1.0, 0.78, 0.62], (head + 0.5).clamp(0.0, 1.0));
            for eye in [Vec2::new(0.35, 0.42), Vec2::new(0.65, 0.42)] {
                let c = size * eye;
                over([0.1, 0.1, 0.25], coverage(x, y, c, 17.0));
                over([0.15, 0.35, 0.95], coverage(x, y, c, 13.0));
                over([1.0, 1.0, 1.0], coverage(x, y, c + Vec2::new(-4.0, -4.0), 4.0));
            }
            over([0.9, 0.1, 0.15], coverage(x, y, size * Vec2::new(0.5, 0.74), 9.0) * f32::from(p.y > size.y * 0.7));
            over([0.1, 0.8, 0.3], coverage(x, y, size * Vec2::new(0.8, 0.12), 6.0));
            pixels.extend(rgba.map(|c| (c * 255.0).round() as u8));
        }
    }
    pixels
}

fn page() -> Scene {
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut s = Scene::new(camera);
    s.background = Background::Solid { colour: Colour::rgb(236, 236, 246) };
    s.post.tonemap = Tonemap::None;
    s
}

fn quality() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() }
}

/// The face layer as a texture: static (`dynamic = false`) or a painter target filled through `update_texture`.
/// Anisotropy 8, as the face asks.
fn texture(r: &mut dyn Renderer, dynamic: bool) -> TextureId {
    let desc = TextureDesc { sampler: Sampler { anisotropy: 8, ..Sampler::default() }, ..TextureDesc::colour(SIZE) };
    if dynamic {
        let id = r.create_texture(&desc.dynamic(), None).unwrap();
        r.update_texture(id, PixelRect::full(SIZE), &face_layer()).unwrap();
        id
    } else {
        r.create_texture(&desc, Some(&face_layer())).unwrap()
    }
}

/// `scales` quads in a row, each `scale` times a quad of half-width 0.4, turned `degrees` about the vertical.
fn row(r: &mut dyn Renderer, texture: TextureId, scales: &[f32], degrees: f32) -> Scene {
    let mut material = Material::unlit(Colour::rgb(236, 236, 246));
    material.overlays.push(Overlay { texture, mapping: Mapping::Uv, strength: 1.0 });
    let material = r.create_material(&material).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::new(0.4, 0.4 * SIZE.y as f32 / SIZE.x as f32))).unwrap();
    let mut scene = page();
    let step = 4.0 / scales.len() as f32;
    for (i, scale) in scales.iter().enumerate() {
        let x = -2.0 + step * (i as f32 + 0.5);
        let transform = Mat4::from_translation(Vec3::new(x, 0.0, 0.0))
            * Mat4::from_rotation_y(degrees.to_radians())
            * Mat4::from_scale(Vec3::splat(*scale));
        scene.instances.push(Instance::new(mesh, material, transform));
    }
    scene
}

/// A picture of `scales` and `degrees` from a static texture and from a painter target.
fn pair(scales: &[f32], degrees: f32, width: u32) -> (Image, Image) {
    let mut r = support::renderer();
    let size = UVec2::new(width, 256);
    let mut draw = |dynamic| {
        let t = texture(&mut r, dynamic);
        let scene = row(&mut r, t, scales, degrees);
        r.render_image(size, AlphaEncoding::Straight, &scene, &quality()).unwrap()
    };
    (draw(false), draw(true))
}

fn save(case: &str, kind: &str, image: &Image) {
    let (Some(dir), Some(panel)) = (std::env::var_os("OMUZAKI_RENDER_OUT"), std::env::var_os("OMUZAKI_PANEL")) else {
        return;
    };
    let name = format!("{case}-{kind}-{}.png", panel.to_string_lossy());
    image::save_buffer(
        PathBuf::from(dir).join(name),
        &image.rgba8,
        image.size.x,
        image.size.y,
        image::ColorType::Rgba8,
    )
    .unwrap();
}

/// How many pixels differ by more than `tolerance` in any channel.
fn differing(a: &Image, b: &Image, tolerance: u8) -> usize {
    a.rgba8
        .chunks(4)
        .zip(b.rgba8.chunks(4))
        .filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > tolerance))
        .count()
}

#[test]
fn the_face_layer_renders_head_on_oblique_and_across_a_zoom_sweep() {
    let cases: [(&str, &[f32], f32, u32); 3] = [
        ("head-on", &[3.6], 0.0, 512),
        ("oblique-60", &[3.6], 60.0, 512),
        ("zoom-sweep", &[1.0, 0.6, 0.35, 0.2, 0.12], 0.0, 1280),
    ];
    for (case, scales, degrees, width) in cases {
        let (fixed, painter) = pair(scales, degrees, width);
        save(case, "static", &fixed);
        save(case, "painter", &painter);
        assert!(differing(&fixed, &painter, 0) > 0, "{case}: the pictures should differ at the soft edges");
        // Away from edges they agree: the premultiplied path must not shift the flat colours. Allow the edge pixels.
        let edge_share = differing(&fixed, &painter, 8) as f32 / (fixed.size.x * fixed.size.y) as f32;
        assert!(edge_share < 0.08, "{case}: {edge_share} of the picture differs by more than 8 levels");
    }
}
