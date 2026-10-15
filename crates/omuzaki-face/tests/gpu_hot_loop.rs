//! Steady state of the GPU rasteriser (#1433 A6, #902): after warm-up a repaint through `VelloRasterizer` allocates nothing
//! in `omuzaki-face`'s own code, only what Vello allocates inside `render_to_texture`, and that count is pinned and the same
//! on every repaint. Also: no wgpu validation error over 100 repaints, and the repaint time (CPU encode, GPU render and
//! resolve, waited for) over 1,000 warmed repaints. Lavapipe is a software adapter, so the times are not a GPU timing
//! (decision 7.3); the owner's run is #1658. Without an adapter the tests print why and pass.

// A counting allocator is the only way to *prove* an allocation budget; the workspace denies unsafe code by default.
// Tests may unwrap: a panic is a clear failure.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

use omuzaki_core::time::Seconds;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{GpuRasterizer, RealPainter};
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping, FacePainter, FaceView,
};
use omuzaki_face_mock::{mock_profile, mock_projection};

/// The most allocations one warmed repaint of the synthetic face's Base layer makes on the calling thread. Measured on
/// lavapipe: 799 or 800 on every one of 32 repaints after 50 warm-up ones. About 742 of them are inside Vello's
/// `render_to_texture` and about 57 in wgpu recording and submitting the resolve pass; the encode, the scene and the views are
/// `omuzaki-face`'s own and allocate nothing (`gpu_encode.rs`). Cutting Vello's share is #902's upstream report.
const VELLO_STEADY_STATE_ALLOCS: usize = 801;
/// Repaints before anything is counted or timed: Vello grows its buffers over the first few dozen.
const WARM_UP: usize = 50;

struct Counting;

thread_local! {
    // `const`-initialised without destructors, so touching them never allocates (this runs inside the allocator).
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Runs `f`; returns how many allocations this thread made during it.
fn count_allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

// SAFETY: every method forwards to `System` unchanged; the counters are the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Everything a repaint needs: a device, the layer's texture, the rasteriser and the face's Base list.
struct Setup {
    device: wgpu::Device,
    native: (wgpu::Device, wgpu::Queue, wgpu::Texture),
    spec: FaceLayerSpec,
    list: DrawList,
    rasterizer: VelloRasterizer,
}

fn setup() -> Option<Setup> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(adapter) => adapter,
        Err(e) => {
            eprintln!("skipped: no GPU adapter ({e}); install mesa-vulkan-drivers for lavapipe");
            return None;
        }
    };
    let descriptor = wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..wgpu::DeviceDescriptor::default() };
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor)).ok()?;

    let profile = mock_profile();
    let params = FaceMapping::default().rest_params();
    let mut drawing = FaceDrawing::new();
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    RealPainter::new(mock_projection(), DEFAULT_PX_PER_M).paint(&input, &mut drawing).unwrap();
    let rect = FaceLayerSpec::for_layer(FaceLayerId::Base, &mock_projection(), DEFAULT_PX_PER_M).rect;
    let spec = FaceLayerSpec::for_rect(rect, FaceLayerId::Base, 256.0 / rect.width());
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
    let list = drawing[FaceLayerId::Base].clone();
    Some(Setup { native: (device.clone(), queue, texture), device, spec, list, rasterizer: VelloRasterizer::new() })
}

impl Setup {
    /// One repaint, waited for.
    fn repaint(&mut self) {
        let drawn = self.rasterizer.rasterize_native(&self.spec, &self.list, &mut self.native).unwrap();
        assert!(drawn, "the rasteriser declined the wgpu tuple");
    }

    fn warm(&mut self) {
        for _ in 0..WARM_UP {
            self.repaint();
        }
        self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }
}

#[test]
fn a_warmed_repaint_allocates_the_pinned_count_on_every_repaint() {
    let Some(mut s) = setup() else { return };
    s.warm();
    // The first repaint after the device went idle also pays for waking it (about ten more), so it is not counted.
    s.repaint();
    let counts: Vec<usize> = (0..32).map(|_| count_allocations(|| s.repaint())).collect();
    eprintln!("allocations per warmed repaint (Vello's own): {counts:?}");
    let most = counts.iter().copied().max().unwrap();
    assert!(
        most <= VELLO_STEADY_STATE_ALLOCS,
        "{most} allocations in one repaint, pinned at {VELLO_STEADY_STATE_ALLOCS}"
    );
    let least = counts.iter().copied().min().unwrap();
    assert!(most - least <= 2, "the count moves between repaints: {counts:?}");
}

#[test]
fn a_hundred_repaints_raise_no_validation_error() {
    let Some(mut s) = setup() else { return };
    s.warm();
    for repaint in 0..100 {
        let scope = s.device.push_error_scope(wgpu::ErrorFilter::Validation);
        s.repaint();
        let error = pollster::block_on(scope.pop());
        assert!(error.is_none(), "repaint {repaint}: {error:?}");
    }
}

#[test]
fn repaint_time_over_a_thousand_warmed_repaints() {
    let Some(mut s) = setup() else { return };
    s.warm();
    let mut ms: Vec<f64> = (0..1000)
        .map(|_| {
            let start = Instant::now();
            s.repaint();
            s.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    eprintln!("repaint on lavapipe (not a GPU timing): p50 {:.3} ms, p99 {:.3} ms (budget 0.5 ms)", ms[500], ms[990]);
}
