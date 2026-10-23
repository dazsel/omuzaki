//! The fragment cost of `Material::uv_transform` (#2093): a full-frame quad whose material samples a base texture and
//! two `Mapping::Uv` overlays, at the live preset, with the identity transform and with an atlas cell picked. The
//! shader applies the transform (one multiply-add per UV sample) either way, so the two medians bound what it costs on
//! top of the three samples; the untextured frame beside them is the floor.
//!
//! `cargo bench -p omuzaki-render --bench uv_transform` runs on the software adapter; with `OMUZAKI_BENCH_GPU=1` it
//! takes the best GPU, which is the number #2093 asks for.

// A benchmark may panic: a panic is a clear failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

use std::time::Instant;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, Instance, Mapping, Material, Overlay, Projection, Quality, Renderer,
    Scene, TargetDesc, TextureDesc, UvTransform,
};
use omuzaki_render_mock::shapes;

const SIZE: UVec2 = UVec2::new(1920, 1080);
const FRAMES: u32 = 60;

fn main() {
    let adapter = if std::env::var_os("OMUZAKI_BENCH_GPU").is_some() {
        AdapterPreference::Auto
    } else {
        AdapterPreference::Software
    };
    let mut r = WgpuRenderer::headless(&HeadlessOptions { adapter })
        .expect("an adapter (install mesa-vulkan-drivers for the software one)");
    let texture = |r: &mut WgpuRenderer, seed: u8| {
        let side = 512_u32;
        // Truncation is the point: a cheap pattern of every byte value.
        #[allow(clippy::cast_possible_truncation)]
        let pixels: Vec<u8> = (0..side * side * 4).map(|i| (i as u8).wrapping_mul(seed)).collect();
        r.create_texture(&TextureDesc::colour(UVec2::splat(side)), Some(&pixels)).unwrap()
    };
    let (base, first, second) = (texture(&mut r, 3), texture(&mut r, 5), texture(&mut r, 7));
    let textured = |uv_transform: UvTransform| {
        let mut material = Material::toon(Colour::WHITE);
        material.base_texture = Some(base);
        material.uv_transform = uv_transform;
        for texture in [first, second] {
            material.overlays.push(Overlay { texture, mapping: Mapping::Uv, strength: 0.5 });
        }
        material
    };
    let atlas = UvTransform { scale: Vec2::new(0.5, 1.0), offset: Vec2::new(0.5, 0.0) };
    let materials = [
        ("untextured", r.create_material(&Material::toon(Colour::WHITE)).unwrap()),
        ("3 UV samples, identity", r.create_material(&textured(UvTransform::IDENTITY)).unwrap()),
        ("3 UV samples, atlas cell", r.create_material(&textured(atlas)).unwrap()),
    ];
    // A quad that covers the frame, so the cost is the fragment path's.
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(2.0))).unwrap();
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 1.0, near: 0.01, far: 10.0 },
    );
    let quality = Quality::live();
    for (name, material) in materials {
        let mut scene = Scene::new(camera);
        scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
        let mut round = || {
            let start = Instant::now();
            for frame in 0..FRAMES {
                scene.frame.index = u64::from(frame);
                r.render(target, &scene, &quality).unwrap();
            }
            r.wait_idle().unwrap();
            start.elapsed().as_secs_f64() * 1e3 / f64::from(FRAMES)
        };
        round();
        let mut rounds: Vec<f64> = (0..5).map(|_| round()).collect();
        rounds.sort_by(f64::total_cmp);
        println!("{name}: {:.3} ms/frame at {}x{} (median of 5 rounds of {FRAMES})", rounds[2], SIZE.x, SIZE.y);
    }
}
