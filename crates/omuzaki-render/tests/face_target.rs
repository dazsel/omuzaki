//! A dynamic texture is a face target a GPU painter (Vello) can draw into: it takes `STORAGE_BINDING`, and since such
//! painters write straight alpha, the renderer premultiplies it in linear light into the texture the material samples
//! (hardware filtering, so anisotropy, wrap and trilinear mips all apply), and soft edges keep their colour (the face material premultiplies Vello's straight alpha).
//!
//! The picture is a soft red disc on a transparent texture, magnified so every edge texel is filtered. The disc's red is
//! full (255) and its green and blue equal, so over a white page an edge pixel that was blended correctly is still pure
//! red-channel 255 with green equal to blue; a dark fringe shows up as a red channel below 255. `OMUZAKI_RENDER_OUT=<dir>`
//! saves the before (a static texture of the same bytes: the hardware filters straight colour) and after (the dynamic
//! face target) picture side by side.

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
use omuzaki_render::NativePaintTarget;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Mapping, Material, Overlay, PaintTarget, Projection,
    Quality, Renderer, RendererExt, Sampler, Scene, TextureDesc, Tonemap, Wrap,
};
use omuzaki_render_mock::shapes;

const SIDE: u32 = 16;
const DISC: [u8; 3] = [255, 40, 40];

/// The disc's straight RGBA at texel `(x, y)`: soft over about one texel, transparent black outside (what a painter that
/// clears its target leaves behind). Kept in step with `PAINT_WGSL`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn disc_pixel(x: u32, y: u32) -> [u8; 4] {
    let (dx, dy) = (x as f32 + 0.5 - 8.0, y as f32 + 0.5 - 8.0);
    let alpha = (5.5 - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
    if alpha == 0.0 { [0; 4] } else { [DISC[0], DISC[1], DISC[2], (alpha * 255.0).round() as u8] }
}

/// The same disc, drawn by a compute shader into the storage texture, as Vello would.
const PAINT_WGSL: &str = "
@group(0) @binding(0) var target_texture: texture_storage_2d<rgba8unorm, write>;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let d = vec2<f32>(id.xy) + vec2<f32>(0.5) - vec2<f32>(8.0);
    let alpha = clamp(5.5 - length(d) + 0.5, 0.0, 1.0);
    var texel = vec4<f32>(0.0);
    if (alpha > 0.0) {
        texel = vec4<f32>(255.0 / 255.0, 40.0 / 255.0, 40.0 / 255.0, alpha);
    }
    textureStore(target_texture, vec2<i32>(id.xy), texel);
}
";

/// Runs the compute shader on the target's texture through `native()`. Returns false if the painter did not get a
/// storage-capable texture (the bind would be a validation error).
fn paint_with_storage_texture(target: &mut dyn PaintTarget) -> Result<(), omuzaki_render_api::RenderError> {
    let Some((device, queue, texture)) = target.native().downcast_mut::<NativePaintTarget>() else {
        return Err(omuzaki_render_api::RenderError::Backend("no native target".into()));
    };
    assert!(texture.usage().contains(wgpu::TextureUsages::STORAGE_BINDING), "a dynamic texture must allow storage");
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("paint disc"),
        source: wgpu::ShaderSource::Wgsl(PAINT_WGSL.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("paint disc"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    // Level 0 only: the mip views of this texture would not be a valid storage binding.
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        mip_level_count: Some(1),
        ..wgpu::TextureViewDescriptor::default()
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("paint disc"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("paint disc") });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(SIDE / 8, SIDE / 8, 1);
    }
    queue.submit([encoder.finish()]);
    Ok(())
}

fn page() -> Scene {
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    );
    let mut s = Scene::new(camera);
    s.background = Background::Solid { colour: Colour::WHITE };
    s.post.tonemap = Tonemap::None;
    s
}

/// An unlit white quad with the texture as its overlay.
fn quad_with(r: &mut dyn Renderer, texture: omuzaki_render_api::TextureId, x: f32) -> Instance {
    let mut material = Material::unlit(Colour::WHITE);
    material.overlays.push(Overlay { texture, mapping: Mapping::Uv, strength: 1.0 });
    let material = r.create_material(&material).unwrap();
    let mesh = r.create_mesh(&shapes::quad(Vec2::splat(0.9))).unwrap();
    Instance::new(mesh, material, Mat4::from_translation(Vec3::new(x, 0.0, 0.0)))
}

fn quality() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

/// Lowest red channel in `image` between pixel columns `x0..x1` (the quad's footprint), and the largest green-blue gap.
fn red_floor_and_gb_gap(image: &Image, x0: u32, x1: u32) -> (u8, u8) {
    let (mut floor, mut gap) = (255u8, 0u8);
    for y in 0..image.size.y {
        for x in x0..x1 {
            let p = image.pixel(x, y).unwrap();
            floor = floor.min(p[0]);
            gap = gap.max(p[1].abs_diff(p[2]));
        }
    }
    (floor, gap)
}

#[test]
fn a_dynamic_face_target_takes_a_storage_painter_and_its_straight_edges_keep_their_colour() {
    let mut r = support::renderer();
    let desc = TextureDesc::colour(UVec2::splat(SIDE));
    let face = r.create_texture(&desc.dynamic(), None).unwrap();
    r.paint_texture(face, &mut paint_with_storage_texture).unwrap();

    // Before: the same bytes in a static texture, filtered by the hardware as straight colour.
    let bytes: Vec<u8> = (0..SIDE).flat_map(|y| (0..SIDE).flat_map(move |x| disc_pixel(x, y))).collect();
    let hardware = r.create_texture(&desc, Some(&bytes)).unwrap();

    let mut scene = page();
    let before = quad_with(&mut r, hardware, -1.0);
    let after = quad_with(&mut r, face, 1.0);
    scene.instances.push(before);
    scene.instances.push(after);
    let image = r.render_image(UVec2::new(512, 256), AlphaEncoding::Straight, &scene, &quality()).unwrap();

    // Left half is the static texture, right half the face target; the quads span 0.1..0.9 of each half's width.
    let (before_red, _) = red_floor_and_gb_gap(&image, 64, 192);
    let (after_red, after_gap) = red_floor_and_gb_gap(&image, 320, 448);
    eprintln!("red floor: before {before_red}, after {after_red}; after green-blue gap {after_gap}");
    save(&image);
    assert!(
        before_red < 245,
        "the hardware path should show a dark fringe (red floor {before_red}), or this test proves nothing"
    );
    assert!(after_red >= 253, "premultiplied filtering keeps the edge's red at full (red floor {after_red})");
    assert!(after_gap <= 2, "green and blue stay equal across the edge (gap {after_gap})");
}

/// With `OMUZAKI_RENDER_OUT=<dir>`, saves the picture (left: before, right: after) for the PR.
fn save(image: &Image) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        let path = PathBuf::from(dir).join("face-target-before-after.png");
        image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
    }
}

/// A flat-lit quad showing `texture`, with its uvs scaled by `repeat` (so `Wrap::Repeat` has something to repeat).
fn tiled_quad(r: &mut dyn Renderer, texture: omuzaki_render_api::TextureId, repeat: f32) -> Instance {
    let mut material = Material::unlit(Colour::WHITE);
    material.overlays.push(Overlay { texture, mapping: Mapping::Uv, strength: 1.0 });
    let material = r.create_material(&material).unwrap();
    let mut mesh = shapes::quad(Vec2::splat(0.9));
    for uv in &mut mesh.uvs {
        *uv *= repeat;
    }
    let mesh = r.create_mesh(&mesh).unwrap();
    Instance::new(mesh, material, Mat4::IDENTITY)
}

fn render_one(r: &mut dyn Renderer, instance: Instance) -> Image {
    let mut scene = page();
    scene.instances.push(instance);
    r.render_image(UVec2::new(256, 256), AlphaEncoding::Straight, &scene, &quality()).unwrap()
}

#[test]
fn black_and_white_blend_to_the_linear_midpoint_like_a_static_texture() {
    let mut r = support::renderer();
    let pixels = [0, 0, 0, 255, 255, 255, 255, 255];
    let desc = TextureDesc::colour(UVec2::new(2, 1));
    let sampler = Sampler { filter: omuzaki_render_api::Filter::Linear, wrap: Wrap::Clamp, anisotropy: 1 };
    let dynamic = r.create_texture(&TextureDesc { sampler, ..desc }.dynamic(), Some(&pixels)).unwrap();
    let fixed = r.create_texture(&TextureDesc { sampler, ..desc }, Some(&pixels)).unwrap();
    let a = quad_with(&mut r, dynamic, 0.0);
    let b = quad_with(&mut r, fixed, 0.0);
    let (a, b) = (render_one(&mut r, a), render_one(&mut r, b));
    // The middle of the quad is the blend of the two texels: linear 0.5, which is sRGB 188 (not the encoded 128).
    let (mid_dynamic, mid_static) = (a.pixel(128, 128).unwrap(), b.pixel(128, 128).unwrap());
    assert!(mid_dynamic[0].abs_diff(188) <= 2, "{mid_dynamic:?}");
    assert!(mid_dynamic[0].abs_diff(mid_static[0]) <= 1, "dynamic {mid_dynamic:?} vs static {mid_static:?}");
}

#[test]
fn a_painter_target_keeps_its_repeat_wrap() {
    let mut r = support::renderer();
    // Two texels, red then blue, tiled twice across the quad: red, blue, red, blue.
    let pixels = [255, 0, 0, 255, 0, 0, 255, 255];
    let sampler = Sampler { filter: omuzaki_render_api::Filter::Nearest, wrap: Wrap::Repeat, anisotropy: 1 };
    let desc = TextureDesc { sampler, mips: false, ..TextureDesc::colour(UVec2::new(2, 1)) };
    let face = r.create_texture(&desc.dynamic(), Some(&pixels)).unwrap();
    let image = {
        let q = tiled_quad(&mut r, face, 2.0);
        render_one(&mut r, q)
    };
    // The quad spans pixels 13..243 of 256; its quarters centre at about 0.125, 0.375, 0.625, 0.875 of that.
    let at = |f: f32| image.pixel((13.0 + 230.0 * f) as u32, 128).unwrap();
    for (f, want) in [(0.125, [255, 0, 0]), (0.375, [0, 0, 255]), (0.625, [255, 0, 0]), (0.875, [0, 0, 255])] {
        let got = at(f);
        assert!(got[..3].iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 2), "at {f}: {got:?}, wanted {want:?}");
    }
}

#[test]
fn two_coloured_shapes_keep_their_colours_at_every_soft_edge() {
    let mut r = support::renderer();
    // A red disc and a blue disc on transparent black, 24x12 texels, magnified. Over white, any blend of a disc colour
    // with the page keeps that disc's saturated channel at 255; a dark fringe would pull it below.
    let (w, h) = (24u32, 12u32);
    let mut pixels = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let (cx, colour) = if x < 12 { (6.0, [255, 30, 30]) } else { (18.0, [30, 30, 255]) };
            let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - 6.0);
            let alpha = (4.5 - d + 0.5).clamp(0.0, 1.0);
            let alpha = (alpha * 255.0).round() as u8;
            pixels.extend(if alpha == 0 { [0; 4] } else { [colour[0], colour[1], colour[2], alpha] });
        }
    }
    let face = r.create_texture(&TextureDesc::colour(UVec2::new(w, h)).dynamic(), Some(&pixels)).unwrap();
    let image = {
        let q = tiled_quad(&mut r, face, 1.0);
        render_one(&mut r, q)
    };
    let worst = (13..243).flat_map(|x| (60..200).map(move |y| (x, y))).map(|(x, y)| {
        let p = image.pixel(x, y).unwrap();
        p[0].max(p[2])
    });
    assert!(worst.min().unwrap() >= 253, "an edge went darker than either disc colour allows");
}

#[test]
fn damage_to_a_released_texture_does_not_reach_the_one_that_reuses_its_slot() {
    let mut r = support::renderer();
    let big = r.create_texture(&TextureDesc::colour(UVec2::splat(16)).dynamic(), None).unwrap();
    r.update_texture(big, omuzaki_render_api::PixelRect::full(UVec2::splat(16)), &[255; 16 * 16 * 4]).unwrap();
    r.release(omuzaki_render_api::ResourceRef::Texture(big)).unwrap();
    let small = r.create_texture(&TextureDesc::colour(UVec2::splat(4)).dynamic(), None).unwrap();
    assert_eq!(small.index(), big.index(), "the slot is reused, which is the case under test");
    // The 16x16 damage must not become a scissor rectangle on the 4x4 texture, which would fail the frame.
    let instance = quad_with(&mut r, small, 0.0);
    let image = render_one(&mut r, instance);
    assert_eq!(image.size, UVec2::splat(256));
}
