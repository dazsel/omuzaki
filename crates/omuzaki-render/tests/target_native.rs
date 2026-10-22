//! The rendered-frame contract (#629): `Renderer::target_native` lends the target's output texture with the device and
//! queue, so the app's viewport and the live outputs use a frame without reading it back first. The texture holds the
//! frame `read_target` returns, it is replaced on resize, and a dead target is refused.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::glam::UVec2;
use omuzaki_render::{AdapterPreference, HeadlessOptions, NativeTarget, WgpuRenderer};
use omuzaki_render_api::Quality;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, RenderError, Renderer, ResourceRef, Scene, TargetDesc,
};

fn renderer() -> WgpuRenderer {
    WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens")
}

/// Copies `native`'s texture to the CPU through its own device and queue, as an output would: rows top to bottom.
fn copy_out(native: &NativeTarget) -> Vec<u8> {
    let (device, queue, texture) = native;
    let size = texture.size();
    let row = size.width * 4;
    let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test read"),
        size: u64::from(padded * size.height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).unwrap();
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((row * size.height) as usize);
    for y in 0..size.height {
        let start = (y * padded) as usize;
        out.extend_from_slice(&data[start..start + row as usize]);
    }
    out
}

#[test]
fn the_native_texture_holds_the_frame_read_target_returns() {
    let mut r = renderer();
    let target = r.create_target(&TargetDesc { size: UVec2::new(40, 24), alpha: AlphaEncoding::Straight }).unwrap();
    let mut scene = Scene::new(Camera::default());
    scene.background = Background::Solid { colour: Colour::rgb(30, 140, 200) };
    r.render(target, &scene, &Quality::offline()).unwrap();
    let native = r.target_native(target).unwrap().expect("the wgpu renderer has native handles");
    let native: &NativeTarget = native.downcast_ref().expect("the documented convention");
    assert_eq!(native.2.size().width, 40);
    assert_eq!(native.2.size().height, 24);
    assert!(native.2.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC));
    let native = native.clone();
    let read = r.read_target(target).unwrap();
    assert_eq!(copy_out(&native), read.rgba8, "the native texture is the frame, byte for byte");
    r.release(ResourceRef::Target(target)).unwrap();
}

#[test]
fn a_resize_replaces_the_texture_and_a_dead_target_is_refused() {
    let mut r = renderer();
    let target =
        r.create_target(&TargetDesc { size: UVec2::new(16, 16), alpha: AlphaEncoding::Premultiplied }).unwrap();
    r.resize_target(target, UVec2::new(32, 8)).unwrap();
    let native: &NativeTarget = r.target_native(target).unwrap().unwrap().downcast_ref().unwrap();
    assert_eq!((native.2.size().width, native.2.size().height), (32, 8));
    r.release(ResourceRef::Target(target)).unwrap();
    assert!(matches!(r.target_native(target), Err(RenderError::UnknownHandle { .. })));
}
