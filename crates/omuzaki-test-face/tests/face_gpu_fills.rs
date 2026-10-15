//! The GPU face rasteriser against the CPU one (CB-24, #1433 A3): the fill probes of `raster_probes.rs` drawn by
//! `VelloRasterizer` into a texture shaped like the renderer's face texture, and by `SkiaRasterizer`, then compared.
//!
//! The rule is the issue's: per channel 8, at most 1 % bad pixels, PSNR of at least 34 dB (looser than the Chrome probes':
//! Vello and Skia anti-alias differently); the radial gradient also within max 8 and p99 5 levels (decision 7.5). Without
//! an adapter (no lavapipe, no GPU) the tests print why and pass. The stroke probes are #318's.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::any::Any;
use std::fmt::Write as _;
use std::sync::mpsc;

use image::RgbaImage;
use omuzaki_face::GpuRasterizer;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face_api::{DrawList, FaceRasterizer, Raster};
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{Rect, compare_region};
use omuzaki_test_face::probes::{
    HEIGHT, PX_PER_M, Probe, WIDTH, clipped_radial_gradient, nested_transforms, overlapping_fills, spec,
    straight_image, tiny_ellipse, translucent_stack,
};

/// What the issue asks of every fill probe (with [`MIN_PSNR_DB`]).
const RULE: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.01 };
const MIN_PSNR_DB: f64 = 34.0;
/// The most the radial gradient may differ, and the most 99 % of its pixels may (decision 7.5, measured inside the shape).
const GRADIENT_MAX: u8 = 8;
/// 6, not the 5 of decision 7.5: the ramp is 8-bit in Vello, and 5 needs a finer ramp or dithering (#1670's area, if ever
/// needed). Ruled on #2002.
const GRADIENT_P99: u8 = 6;

/// The rule for probes made mostly of curves. Skia replaces a curve by segments from its own fixed-point forward
/// differencing and snaps each vertex to a quarter row; Vello flattens differently, so a few more edge pixels differ by
/// more than 8 levels even after the encoder snaps its end points. Ruled on #2002 (the fallback to 0.03, per probe).
const CURVED: Tolerance = Tolerance { per_channel: 8, max_bad_fraction: 0.03 };

const PROBES: [(&str, Probe, Tolerance); 5] = [
    ("overlapping-fills", overlapping_fills, RULE),
    ("clipped-radial-gradient", clipped_radial_gradient, RULE),
    ("translucent-stack", translucent_stack, CURVED),
    ("tiny-ellipse", tiny_ellipse, RULE),
    ("nested-transforms", nested_transforms, CURVED),
];

/// A device and queue, or `None` after printing why there is none.
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(adapter) => adapter,
        Err(e) => {
            eprintln!("skipped: no GPU adapter ({e}); install mesa-vulkan-drivers for lavapipe");
            return None;
        }
    };
    let descriptor = wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..wgpu::DeviceDescriptor::default() };
    match pollster::block_on(adapter.request_device(&descriptor)) {
        Ok(pair) => Some(pair),
        Err(e) => {
            eprintln!("skipped: the adapter gave no device ({e})");
            None
        }
    }
}

/// A texture like the one `native()` hands out for a face layer.
fn face_texture(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("face layer"),
        size: wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
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
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let row = WIDTH as usize * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read-back"),
        size: (padded * HEIGHT as usize) as u64,
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
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let (sender, receiver) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| sender.send(r).unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receiver.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    mapped.chunks_exact(padded).flat_map(|line| line[..row].iter().copied()).collect()
}

/// The probe through `rasterizer`, with the number of validation errors wgpu raised while it ran. `native` is what the
/// rasteriser is handed; the texture is the layer's.
fn paint(
    gpu: &(wgpu::Device, wgpu::Queue),
    rasterizer: &mut VelloRasterizer,
    probe: Probe,
    native: &mut dyn Any,
) -> (bool, Option<wgpu::Error>) {
    let mut list = DrawList::new();
    probe(&mut list);
    let scope = gpu.0.push_error_scope(wgpu::ErrorFilter::Validation);
    let drawn = rasterizer.rasterize_native(&spec(), &list, native).unwrap();
    (drawn, pollster::block_on(scope.pop()))
}

/// The probe on the GPU, as premultiplied RGBA8.
fn gpu_pixels(gpu: &(wgpu::Device, wgpu::Queue), rasterizer: &mut VelloRasterizer, probe: Probe) -> Vec<u8> {
    let mut native = (gpu.0.clone(), gpu.1.clone(), face_texture(&gpu.0));
    let (drawn, error) = paint(gpu, rasterizer, probe, &mut native);
    assert!(drawn, "the rasteriser declined a tuple of (Device, Queue, Texture)");
    assert!(error.is_none(), "wgpu raised a validation error: {error:?}");
    read_back(&gpu.0, &gpu.1, &native.2)
}

/// The probe on the CPU, as premultiplied RGBA8.
fn cpu_pixels(probe: Probe) -> Vec<u8> {
    let mut list = DrawList::new();
    probe(&mut list);
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec(), &list, PX_PER_M, &mut raster).unwrap();
    raster.pixels
}

fn image(pixels: &[u8]) -> RgbaImage {
    straight_image(WIDTH, HEIGHT, pixels).unwrap()
}

/// The `p`-th percentile (0..=100) and the maximum of the channel differences over the pixels whose whole 5 x 5
/// neighbourhood the CPU raster inked. Edge pixels are left out: they are the fills' rule, and the gradient bound is
/// about the ramp (decision 7.5).
fn interior_difference(gpu: &[u8], cpu: &[u8], percentile: usize) -> (u8, u8) {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let inked = |x: usize, y: usize| cpu[(y * w + x) * 4 + 3] > 0;
    let mut diffs = Vec::new();
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            if (y - 2..=y + 2).all(|yy| (x - 2..=x + 2).all(|xx| inked(xx, yy))) {
                let i = (y * w + x) * 4;
                diffs.extend((0..4).map(|k| gpu[i + k].abs_diff(cpu[i + k])));
            }
        }
    }
    diffs.sort_unstable();
    let at = (diffs.len() * percentile / 100).min(diffs.len().saturating_sub(1));
    (diffs.get(at).copied().unwrap_or(0), diffs.last().copied().unwrap_or(0))
}

#[test]
fn the_fill_probes_match_the_cpu_raster() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let mut table = String::new();
    let mut failures = String::new();
    for (name, probe, rule) in PROBES {
        let (g, c) = (gpu_pixels(&gpu, &mut rasterizer, probe), cpu_pixels(probe));
        let ink = g.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
        assert!(ink > 8, "{name}: the GPU drew {ink} inked pixels");
        let report = compare_region(&image(&g), &image(&c), Rect::full(WIDTH, HEIGHT), rule).unwrap();
        let _ = writeln!(
            table,
            "{name}: bad fraction {:.3} %, max difference {}, PSNR {:?} dB",
            report.stats.bad_fraction * 100.0,
            report.stats.max_delta,
            report.stats.psnr_db
        );
        if !report.passes(MIN_PSNR_DB) {
            let _ = writeln!(failures, "{name}: {}", report.stats);
        }
    }
    eprintln!("{table}");
    assert!(failures.is_empty(), "probes outside their rule / {MIN_PSNR_DB} dB:\n{failures}");
}

#[test]
fn the_radial_gradient_is_within_decision_7_5() {
    let Some(gpu) = device() else { return };
    let (g, c) =
        (gpu_pixels(&gpu, &mut VelloRasterizer::new(), clipped_radial_gradient), cpu_pixels(clipped_radial_gradient));
    let (p99, max) = interior_difference(&g, &c, 99);
    eprintln!("clipped-radial-gradient: max {max}, p99 {p99} levels per channel");
    assert!(
        max <= GRADIENT_MAX && p99 <= GRADIENT_P99,
        "max {max} (limit {GRADIENT_MAX}), p99 {p99} (limit {GRADIENT_P99})"
    );
}

#[test]
fn a_repaint_with_the_same_rasteriser_gives_the_same_pixels() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let first = gpu_pixels(&gpu, &mut rasterizer, overlapping_fills);
    gpu_pixels(&gpu, &mut rasterizer, tiny_ellipse);
    assert_eq!(first, gpu_pixels(&gpu, &mut rasterizer, overlapping_fills), "a reused scene and texture left traces");
}

#[test]
fn a_native_handle_that_is_not_the_tuple_is_declined_and_nothing_is_drawn() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let (drawn, error) = paint(&gpu, &mut rasterizer, overlapping_fills, &mut 7_u32);
    assert!(!drawn, "a u32 is not a native handle");
    assert!(error.is_none(), "{error:?}");
    // A tuple in the wrong order is declined too, and the texture it holds stays empty.
    let texture = face_texture(&gpu.0);
    let (drawn, _) =
        paint(&gpu, &mut rasterizer, overlapping_fills, &mut (texture.clone(), gpu.0.clone(), gpu.1.clone()));
    assert!(!drawn);
    assert!(read_back(&gpu.0, &gpu.1, &texture).iter().all(|&b| b == 0), "a declined paint wrote pixels");
}
