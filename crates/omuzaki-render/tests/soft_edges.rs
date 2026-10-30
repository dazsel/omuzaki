//! Soft edges (#2529) on the software adapter: a masked surface that is also order-independent keeps its opaque part
//! where the alpha reaches the cutoff and, instead of cutting the rest, blends it with the alpha scaled up to 1 at the
//! cutoff, as hair tips fade. A plain masked surface still cuts. Each check compares renders of one scene.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
// Pixel coordinates are small; the casts are exact enough.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Camera, Colour, Image, Instance, Material, Projection, Quality, Renderer,
    RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::shapes;

/// Software Vulkan JIT-compiles every pipeline on the calling thread; one GPU test at a time keeps it steady.
fn serial() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(PoisonError::into_inner)
}

const SIZE: UVec2 = UVec2::new(128, 32);
const CUTOFF: f32 = 0.5;

/// The red channel at `x`, a fraction of the way across the quad (which fills the frame), on the middle row.
fn red(image: &Image, x: f32) -> u8 {
    image.pixel((x * SIZE.x as f32) as u32, SIZE.y / 2).unwrap()[0]
}

/// A white quad filling the frame over black, its alpha rising from 0 at the left edge to 1 at the right.
fn render(order_independent: bool) -> Image {
    let mut r = support::renderer();
    let mut quad = shapes::quad(Vec2::new(2.0, 0.5));
    quad.colours = quad.positions.iter().map(|p| [1.0, 1.0, 1.0, (p.x + 2.0) / 4.0]).collect();
    let mesh = r.create_mesh(&quad).unwrap();
    let mut material = Material::unlit(Colour::WHITE);
    material.vertex_colours = true;
    material.alpha = AlphaMode::Mask { cutoff: CUTOFF };
    material.order_independent = order_independent;
    let material = r.create_material(&material).unwrap();
    let mut s = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 1.0, near: 0.1, far: 10.0 },
    ));
    s.background = Background::Solid { colour: Colour::BLACK };
    s.post.tonemap = Tonemap::None;
    s.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false.into(), ..Quality::offline() };
    r.render_image(SIZE, AlphaEncoding::Straight, &s, &quality).unwrap()
}

#[test]
fn a_soft_edge_blends_what_the_mask_would_cut_and_keeps_the_rest_opaque() {
    let _gpu = serial();
    let cut = render(false);
    let soft = render(true);
    for x in [0.6, 0.8, 0.95] {
        assert_eq!((red(&cut, x), red(&soft, x)), (255, 255), "above the cutoff at {x} both are opaque");
    }
    for x in [0.1, 0.25, 0.4] {
        assert_eq!(red(&cut, x), 0, "the mask cuts below the cutoff at {x}");
    }
    let ramp: Vec<u8> = [0.1, 0.25, 0.4].iter().map(|x| red(&soft, *x)).collect();
    println!("soft edge at 0.1, 0.25, 0.4: {ramp:?}");
    assert!(
        ramp[0] > 10 && ramp[0] < ramp[1] && ramp[1] < ramp[2] && ramp[2] < 255,
        "the soft edge fades in: {ramp:?}"
    );
}
