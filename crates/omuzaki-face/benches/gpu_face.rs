//! Benchmark: the GPU face rasteriser (`VelloRasterizer`) on fills, clips and a radial gradient, against the 0.5 ms
//! face-paint budget (spec v3 §10).
//!
//! It creates its own device. Without an adapter it prints why and returns. On lavapipe (a CPU adapter) the numbers
//! check that the bench runs and are not a timing; the budget needs real hardware (decision 7.3).

// The GPU rasteriser exists on native targets only (#2207).
#![cfg(not(target_arch = "wasm32"))]

use std::any::Any;
use std::time::Instant;

use omuzaki_core::glam::Vec2;
use omuzaki_face::GpuRasterizer;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face_api::{Cmd, Colour, DrawList, FaceLayerId, FaceLayerSpec, FaceRect, Paint, Stops};

/// Repaints timed after the warm-up.
const FRAMES: usize = 200;
/// Repaints before timing, so Vello's pipelines and buffers are warm.
const WARM_UP: usize = 10;
const PX_PER_M: f32 = 1000.0;

fn v(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

/// Overlapping translucent ellipses, as in the `translucent-stack` probe.
fn fills(d: &mut DrawList, shift: f32) {
    for (cx, cy, rx, ry, colour) in [
        (-0.05, 0.02, 0.05, 0.04, Colour::rgba(220, 40, 60, 200)),
        (0.0, 0.0, 0.05, 0.05, Colour::rgba(40, 120, 230, 160)),
        (0.05, 0.02, 0.045, 0.045, Colour::rgba(250, 200, 20, 120)),
    ] {
        let path = d.path().ellipse(v(cx + shift, cy), rx, ry).finish();
        d.push(Cmd::Fill { path, paint: Paint::Solid(colour), opacity: 0.8 });
    }
}

/// A four-stop radial gradient inside an elliptical clip, as in the `clipped-radial-gradient` probe.
fn clipped_gradient(d: &mut DrawList, shift: f32) {
    let clip = d.path().ellipse(v(0.0, 0.0), 0.1, 0.06).finish();
    d.push(Cmd::PushClip(clip));
    let sheet = d.path().polyline(&[v(-0.13, -0.09), v(0.13, -0.09), v(0.13, 0.09), v(-0.13, 0.09)]).close().finish();
    let stops = Stops::new(&[
        (0.0, Colour::rgba(255, 247, 224, 255)),
        (0.3, Colour::rgba(255, 194, 60, 255)),
        (0.65, Colour::rgba(224, 64, 46, 176)),
        (1.0, Colour::rgba(42, 16, 112, 0)),
    ]);
    d.push(Cmd::Fill {
        path: sheet,
        paint: Paint::Radial { centre: v(-0.02 + shift, 0.01), radius: 0.085, stops },
        opacity: 1.0,
    });
    d.push(Cmd::PopClip);
}

/// A device and queue, or `None` after printing why there is none.
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(adapter) => adapter,
        Err(e) => {
            println!("gpu_face: no GPU adapter ({e}); nothing to measure");
            return None;
        }
    };
    // Windows' only software adapter is WARP, which dies with an access violation when Vello runs on it (#2236), so
    // these tests treat it as no adapter, like a machine with none. A real GPU on Windows still runs them.
    if cfg!(windows) && adapter.get_info().device_type == wgpu::DeviceType::Cpu {
        println!("gpu_face: the only adapter is WARP (see #2236); nothing to measure");
        return None;
    }
    let descriptor = wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..wgpu::DeviceDescriptor::default() };
    match pollster::block_on(adapter.request_device(&descriptor)) {
        Ok(pair) => Some(pair),
        Err(e) => {
            println!("gpu_face: the adapter gave no device ({e}); nothing to measure");
            None
        }
    }
}

fn main() {
    let Some((device, queue)) = device() else { return };
    let spec =
        FaceLayerSpec::for_rect(FaceRect { x0: -0.128, x1: 0.128, y0: -0.08, y1: 0.08 }, FaceLayerId::Base, PX_PER_M);
    let (width, height) = spec.size_px();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("face layer"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut native: Box<dyn Any> = Box::new((device.clone(), queue, texture));
    let mut rasterizer = VelloRasterizer::new();
    println!(
        "gpu_face: {width}x{height} px, {FRAMES} repaints (alternating two lists) after {WARM_UP} warm-up (CPU encode + GPU render + resolve)"
    );
    for (name, build) in [("fills", fills as fn(&mut DrawList, f32)), ("clipped radial gradient", clipped_gradient)] {
        // A repaint of an unchanged list does no work, so the frames alternate two lists that differ by a small shift.
        let lists = [0.0, 0.004].map(|shift| {
            let mut list = DrawList::new();
            build(&mut list, shift);
            list
        });
        let mut times_ms = Vec::with_capacity(FRAMES);
        for frame in 0..WARM_UP + FRAMES {
            let start = Instant::now();
            let drawn = rasterizer.rasterize_native(&spec, &lists[frame % 2], native.as_mut());
            if !matches!(drawn, Ok(true)) {
                println!("{name}: the rasteriser did not draw: {drawn:?}");
                return;
            }
            // The paint is done when the GPU is, not when the commands are queued.
            if let Err(e) = device.poll(wgpu::PollType::wait_indefinitely()) {
                println!("{name}: the device did not finish: {e}");
                return;
            }
            if frame >= WARM_UP {
                times_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        times_ms.sort_by(f64::total_cmp);
        println!("{name}: p50 {:.3} ms, p99 {:.3} ms", times_ms[FRAMES / 2], times_ms[FRAMES * 99 / 100]);
    }
}
