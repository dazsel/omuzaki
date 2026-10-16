//! What the GPU face tests share: a device (or the reason there is none), a texture like the renderer's face texture, a
//! read-back, and both rasterisers behind one call each.

// Test code: a panic is a clear failure. Each test file uses some of these.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::any::Any;
use std::sync::mpsc;

use image::RgbaImage;
use omuzaki_face::GpuRasterizer;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face_api::{DrawList, FaceLayerSpec, FaceRasterizer, Raster};
use omuzaki_test_face::probes::straight_image;

/// The device and queue the GPU tests draw with.
pub type Gpu = (wgpu::Device, wgpu::Queue);

/// An adapter, or `None` after printing why there is none.
pub fn adapter() -> Option<wgpu::Adapter> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(adapter) => {
            // Which adapter ran is the first thing to know when a GPU test fails on a machine nobody can look at.
            let info = adapter.get_info();
            eprintln!("GPU tests run on {} ({:?}, {:?})", info.name, info.backend, info.device_type);
            // Windows' only software adapter is WARP, which dies with an access violation when Vello runs on it (#2236), so
            // these tests treat it as no adapter, like a machine with none. A real GPU on Windows still runs them.
            if cfg!(windows) && adapter.get_info().device_type == wgpu::DeviceType::Cpu {
                eprintln!("skipped: the only adapter is WARP (see #2236)");
                return None;
            }
            Some(adapter)
        }
        Err(e) => {
            eprintln!("skipped: no GPU adapter ({e}); install mesa-vulkan-drivers for lavapipe");
            None
        }
    }
}

/// A device and queue, or `None` after printing why there is none.
pub fn device() -> Option<Gpu> {
    let adapter = adapter()?;
    let descriptor = wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..wgpu::DeviceDescriptor::default() };
    match pollster::block_on(adapter.request_device(&descriptor)) {
        Ok(pair) => Some(pair),
        Err(e) => {
            eprintln!("skipped: the adapter gave no device ({e})");
            None
        }
    }
}

/// Two devices of one adapter, or `None` after printing why there are not. They must share an instance: wgpu numbers its
/// resources per instance, so devices of two instances can't tell each other's handles apart.
pub fn two_devices() -> Option<(Gpu, Gpu)> {
    let adapter = adapter()?;
    let descriptor = wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..wgpu::DeviceDescriptor::default() };
    let make = || pollster::block_on(adapter.request_device(&descriptor));
    match (make(), make()) {
        (Ok(first), Ok(second)) => Some((first, second)),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("skipped: the adapter gave no second device ({e})");
            None
        }
    }
}

/// A texture like the one `native()` hands out for a face layer.
pub fn face_texture(device: &wgpu::Device, (width, height): (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("face layer"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Level 0 of `texture` as tightly packed RGBA8.
pub fn read_back(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let (device, queue) = gpu;
    let (width, height) = (texture.width(), texture.height());
    let row = width as usize * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read-back"),
        size: (padded * height as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u32::try_from(padded).unwrap()),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let (sender, receiver) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| sender.send(r).unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receiver.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    mapped.chunks_exact(padded).flat_map(|line| line[..row].iter().copied()).collect()
}

/// `list` through `rasterizer` into `native`, inside a validation error scope: whether it drew, and any validation error.
pub fn paint(
    gpu: &Gpu,
    rasterizer: &mut VelloRasterizer,
    spec: &FaceLayerSpec,
    list: &DrawList,
    native: &mut dyn Any,
) -> (bool, Option<wgpu::Error>) {
    let scope = gpu.0.push_error_scope(wgpu::ErrorFilter::Validation);
    let drawn = rasterizer.rasterize_native(spec, list, native).unwrap();
    (drawn, pollster::block_on(scope.pop()))
}

/// `list` on the GPU, as premultiplied RGBA8. Asserts that it drew and that wgpu raised no validation error.
pub fn gpu_pixels(gpu: &Gpu, rasterizer: &mut VelloRasterizer, spec: &FaceLayerSpec, list: &DrawList) -> Vec<u8> {
    let mut native = (gpu.0.clone(), gpu.1.clone(), face_texture(&gpu.0, spec.size_px()));
    let (drawn, error) = paint(gpu, rasterizer, spec, list, &mut native);
    assert!(drawn, "the rasteriser declined a tuple of (Device, Queue, Texture)");
    assert!(error.is_none(), "wgpu raised a validation error: {error:?}");
    read_back(gpu, &native.2)
}

/// `list` on the CPU, as premultiplied RGBA8.
pub fn cpu_pixels(spec: &FaceLayerSpec, list: &DrawList) -> Vec<u8> {
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(spec, list, spec.px_per_m, &mut raster).unwrap();
    raster.pixels
}

/// Premultiplied pixels of a `size` raster as the straight-alpha image the comparison takes.
pub fn image(size: (u32, u32), pixels: &[u8]) -> RgbaImage {
    straight_image(size.0, size.1, pixels).unwrap()
}
