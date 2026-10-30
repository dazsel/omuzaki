//! The hair shadow (#2528) on the software adapter: a card of hair two centimetres in front of a wall, lit from above
//! and in front, shades a band of the wall just under its edge when the wall receives the hair shadow, and leaves the
//! wall as it was when it does not. Shadow maps alone miss this band: the gap is inside their normal bias, and a face
//! does not receive them at all.
//!
//! Each check compares renders of one scene, so the expected colours need no oracle.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
// Pixel coordinates are small; the casts are exact enough.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, HairShadow, Image, Instance, Light, Material, MaterialId, MeshId,
    Projection, Quality, Renderer, RendererExt, Rim, Scene, ShadowQuality, Skin, Tonemap, Toon,
};
use omuzaki_render_mock::shapes;

/// Software Vulkan JIT-compiles every pipeline on the calling thread; one GPU test at a time keeps it steady.
fn serial() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(PoisonError::into_inner)
}

const SIZE: UVec2 = UVec2::new(128, 128);
/// The camera's distance from the wall: a 30 degree view 0.6 m away spans 0.32 m of wall, 400 pixels a metre.
const DISTANCE: f32 = 0.6;
/// How far the hair hangs in front of the wall, as a fringe does in front of a forehead.
const GAP: f32 = 0.02;

fn quality(shadows: ShadowQuality) -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), shadows, ..Quality::offline() }
}

/// The pixel row of the wall at height `y` (the camera looks at the wall's origin).
fn row(y: f32) -> u32 {
    let per_metre = SIZE.y as f32 / (2.0 * DISTANCE * 15f32.to_radians().tan());
    (SIZE.y as f32 / 2.0 - y * per_metre).floor() as u32
}

fn red(image: &Image, y: f32) -> u8 {
    image.pixel(SIZE.x / 2, row(y)).unwrap()[0]
}

/// A skin-like wall that does not receive shadow maps (as a face does not), and whether it takes the hair shadow.
fn wall(receives_hair_shadow: bool) -> Material {
    let mut m = Material::toon(Colour::hex(0x00ff_e4d6));
    m.shading = omuzaki_render_api::Shading::Toon(Toon {
        shade: Colour::hex(0x00e8_a8a0),
        rim: Rim { strength: 0.0, ..Rim::default() },
        skin: Some(Skin::default()),
        ..Toon::default()
    });
    m.receives_shadows = false;
    m.hair_shadow.receives = receives_hair_shadow;
    m
}

fn hair() -> Material {
    let mut m = Material::toon(Colour::hex(0x0040_3050));
    m.double_sided = true;
    m.hair_shadow = HairShadow::CASTS;
    m
}

struct World {
    r: WgpuRenderer,
    wall: MeshId,
    card: MeshId,
    hair: MaterialId,
    /// The wall that does not receive the hair shadow, and the one that does.
    walls: [MaterialId; 2],
}

impl World {
    fn new() -> Self {
        let mut r = support::renderer();
        let walls = [false, true].map(|receives| r.create_material(&wall(receives)).unwrap());
        let hair = r.create_material(&hair()).unwrap();
        let wall = r.create_mesh(&shapes::quad(Vec2::splat(0.3))).unwrap();
        let card = r.create_mesh(&shapes::quad(Vec2::new(0.1, 0.05))).unwrap();
        Self { r, wall, card, hair, walls }
    }

    /// The wall, and the hair card (its lower edge at the wall's origin height, `GAP` in front) when `card`.
    fn image(&mut self, receives: bool, card: bool, shadows: ShadowQuality) -> Image {
        let mut s = Scene::new(Camera::look_at(
            Vec3::new(0.0, 0.0, DISTANCE),
            Vec3::ZERO,
            Vec3::Y,
            Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
        ));
        s.background = Background::Solid { colour: Colour::BLACK };
        s.post.tonemap = Tonemap::None;
        s.lights = vec![
            Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.6 },
            Light::Directional {
                to_light: Vec3::new(0.0, 1.0, 1.0).normalize(),
                colour: Colour::WHITE,
                intensity: 2.0,
                casts_shadow: true,
            },
        ];
        s.instances.push(Instance::new(self.wall, self.walls[usize::from(receives)], Mat4::IDENTITY));
        if card {
            let at = Mat4::from_translation(Vec3::new(0.0, 0.05, GAP));
            s.instances.push(Instance::new(self.card, self.hair, at));
        }
        self.r.render_image(SIZE, AlphaEncoding::Straight, &s, &quality(shadows)).unwrap()
    }
}

/// Under the card's edge, a band as deep as the light's angle says (2 cm at 45 degrees) turns to the shade tone on a
/// receiving wall; further down the wall is lit as it is without the card. A wall that does not receive it, or a frame
/// without shadows, is untouched.
#[test]
fn hair_shades_the_band_under_it_on_a_receiving_surface_only() {
    let _gpu = serial();
    let mut w = World::new();
    let bare = w.image(true, false, ShadowQuality::Low);
    let shaded = w.image(true, true, ShadowQuality::Low);
    let ignored = w.image(false, true, ShadowQuality::Low);
    let off = w.image(true, true, ShadowQuality::Off);

    for y in [-0.006, -0.01, -0.014] {
        println!("y {y}: bare {}, shaded {}, ignored {}", red(&bare, y), red(&shaded, y), red(&ignored, y));
        assert!(red(&shaded, y) + 12 < red(&bare, y), "the band under the hair at {y} m is shaded");
        assert!(red(&ignored, y).abs_diff(red(&bare, y)) <= 1, "a wall that does not receive it is untouched at {y}");
        assert!(red(&off, y).abs_diff(red(&bare, y)) <= 1, "without shadows there is no hair shadow at {y}");
    }
    for y in [-0.04, -0.08] {
        assert!(red(&shaded, y).abs_diff(red(&bare, y)) <= 1, "the wall at {y} m is out of the hair's shadow");
    }
}
