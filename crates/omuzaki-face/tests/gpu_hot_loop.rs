//! Steady state of the GPU rasteriser (#1433 A5 and A6, #902): after warm-up a repaint through `VelloRasterizer` allocates
//! nothing in `omuzaki-face`'s own code, only what Vello and wgpu allocate, and that count is pinned and the same on every
//! repaint, for a mouth-only change and for a change of the whole face. Also: no wgpu validation error over 100 repaints, and
//! the repaint time (CPU encode, GPU render and resolve, waited for) over 1,000 warmed repaints of each. Lavapipe is a
//! software adapter, so the times are not a GPU timing (decision 7.3); the owner's run is #1658. Without an adapter the tests
//! print why and pass.

// The GPU rasteriser exists on native targets only (#2207).
#![cfg(not(target_arch = "wasm32"))]
// A counting allocator is the only way to *prove* an allocation budget; the workspace denies unsafe code by default.
// Tests may unwrap: a panic is a clear failure.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

#[path = "support/gpu_instance.rs"]
mod gpu_instance;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

use omuzaki_core::time::Seconds;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{GpuRasterizer, LayerSections, RealPainter};
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping, FacePainter, FaceView,
    PaintParams,
};
use omuzaki_face_mock::{mock_profile, mock_projection};

/// The most allocations one warmed repaint makes on the calling thread, measured on lavapipe over 32 repaints after 50
/// warm-up ones (815 or 816 for either change: Vello's fixed cost dominates), and the spread allowed around it. The count is Vello's and wgpu's: encoding, the scenes, the views and the
/// bookkeeping are `omuzaki-face`'s own and allocate nothing (`gpu_encode.rs`). Cutting it is #902's upstream report.
const MOUTH_ONLY_ALLOCS: usize = 816;
const WHOLE_FACE_ALLOCS: usize = 816;
const SPREAD: usize = 2;
/// What the occasional repaint pays on top (one in about 30: wgpu's periodic cleanup of its resource tables, measured at +27).
const SPIKE: usize = 50;
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

/// Everything a repaint needs: a device, the layer's texture, the rasteriser and the frames it alternates between.
struct Setup {
    device: wgpu::Device,
    native: (wgpu::Device, wgpu::Queue, wgpu::Texture),
    spec: FaceLayerSpec,
    rasterizer: VelloRasterizer,
    /// The drawing and its section ranges of the neutral face, the face with the mouth open, and a face with everything
    /// moved.
    frames: [(FaceDrawing, LayerSections); 3],
    /// The frame to paint next.
    next: usize,
}

const NEUTRAL: usize = 0;
const MOUTH: usize = 1;
const WHOLE: usize = 2;

fn setup() -> Option<Setup> {
    let instance = gpu_instance::instance();
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
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let mut frame = |change: fn(&mut PaintParams)| {
        let mut params = FaceMapping::default().rest_params();
        change(&mut params);
        let mut drawing = FaceDrawing::new();
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        painter.paint(&input, &mut drawing).unwrap();
        let sections = painter.sections().layer(FaceLayerId::Base).clone();
        (drawing, sections)
    };
    let frames = [
        frame(|_| {}),
        frame(|p| p.mouth.open = 0.6),
        frame(|p| {
            p.mouth.open = 0.6;
            p.mouth.smile_left = 1.0;
            p.left_eye.close = 0.6;
            p.blush = 1.0;
            p.gaze = [0.5, 0.3].into();
            p.left_brow.height = 1.0;
        }),
    ];
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
    Some(Setup {
        native: (device.clone(), queue, texture),
        device,
        spec,
        rasterizer: VelloRasterizer::new(),
        frames,
        next: 0,
    })
}

impl Setup {
    /// Paints `frame`, a different one from the last, through the sections path.
    fn repaint(&mut self, frame: usize) {
        let (drawing, sections) = &self.frames[frame];
        let drawn = self
            .rasterizer
            .rasterize_native_sections(&self.spec, &drawing[FaceLayerId::Base], sections, &mut self.native)
            .unwrap();
        assert!(drawn, "the rasteriser declined the wgpu tuple");
    }

    /// One repaint of the change under test: the neutral face and `other` in turn.
    fn alternate(&mut self, other: usize) {
        let frame = if self.next == 0 { other } else { NEUTRAL };
        self.next = 1 - self.next;
        self.repaint(frame);
    }

    fn warm(&mut self, other: usize) {
        for _ in 0..WARM_UP {
            self.alternate(other);
        }
        self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }
}

/// The allocations of 32 warmed repaints alternating the neutral face with `other`, checked against `pinned`.
fn check_allocations(other: usize, pinned: usize, what: &str) {
    let Some(mut s) = setup() else { return };
    s.warm(other);
    // The first repaint after the device went idle also pays for waking it, so it is not counted.
    s.alternate(other);
    let counts: Vec<usize> = (0..32).map(|_| count_allocations(|| s.alternate(other))).collect();
    eprintln!("allocations per warmed repaint, {what}: {counts:?}");
    let mut sorted = counts.clone();
    sorted.sort_unstable();
    let (median, least, most) = (sorted[sorted.len() / 2], sorted[0], sorted[sorted.len() - 1]);
    assert!(median <= pinned, "{what}: a typical repaint makes {median} allocations, pinned at {pinned}");
    assert!(median - least <= SPREAD, "{what}: the count moves between repaints: {counts:?}");
    assert!(most <= pinned + SPIKE, "{what}: {most} allocations in one repaint, more than {pinned} + {SPIKE}");
}

#[test]
fn a_warmed_mouth_only_repaint_allocates_the_pinned_count_on_every_repaint() {
    check_allocations(MOUTH, MOUTH_ONLY_ALLOCS, "mouth only");
}

#[test]
fn a_warmed_whole_face_repaint_allocates_the_pinned_count_on_every_repaint() {
    check_allocations(WHOLE, WHOLE_FACE_ALLOCS, "whole face");
}

#[test]
fn an_unchanged_repaint_allocates_nothing() {
    let Some(mut s) = setup() else { return };
    s.warm(MOUTH);
    s.repaint(NEUTRAL);
    s.repaint(NEUTRAL);
    let allocations = count_allocations(|| s.repaint(NEUTRAL));
    assert_eq!(allocations, 0, "fingerprinting the sections of an unchanged face allocated");
    assert_eq!(s.rasterizer.sections_encoded(), 0);
}

#[test]
fn a_hundred_repaints_raise_no_validation_error() {
    let Some(mut s) = setup() else { return };
    s.warm(WHOLE);
    for repaint in 0..100 {
        let scope = s.device.push_error_scope(wgpu::ErrorFilter::Validation);
        s.alternate(if repaint % 2 == 0 { MOUTH } else { WHOLE });
        let error = pollster::block_on(scope.pop());
        assert!(error.is_none(), "repaint {repaint}: {error:?}");
    }
}

/// p50 and p99 in milliseconds of 1,000 warmed repaints alternating the neutral face with `other`.
fn repaint_times(other: usize) -> Option<(f64, f64)> {
    let mut s = setup()?;
    s.warm(other);
    let mut ms: Vec<f64> = (0..1000)
        .map(|_| {
            let start = Instant::now();
            s.alternate(other);
            s.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    Some((ms[500], ms[990]))
}

#[test]
#[ignore = "prints timings and asserts nothing; ~15 s on lavapipe. Run with --ignored --nocapture (the gpu_face bench has the same numbers)"]
fn repaint_time_over_a_thousand_warmed_repaints() {
    for (what, other) in [("mouth only", MOUTH), ("whole face", WHOLE)] {
        if let Some((p50, p99)) = repaint_times(other) {
            eprintln!(
                "repaint, {what}, on lavapipe (not a GPU timing): p50 {p50:.3} ms, p99 {p99:.3} ms (budget 0.5 ms)"
            );
        }
    }
}
