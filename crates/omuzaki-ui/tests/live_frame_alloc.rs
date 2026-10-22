//! What the Live viewport's frame path allocates once it is warm (the §10 budgets: no allocation or locking in the
//! real-time path), counted on the thread that draws.
//!
//! Two numbers, because they are different kinds of number:
//!
//! - **Showing the picture to egui** (`NativePresenter::present`) makes no GPU call once the texture is registered, so
//!   its count does not depend on the driver: exactly 0.
//! - **The whole frame** (the stage's tick and render on wgpu, the presenter, the latency sample, the publish) goes
//!   through the GPU driver, and a driver's own allocations differ (CI's software Vulkan makes more than hardware: #2222
//!   pinned 30 on one machine and CI made 65). So it is pinned as a ceiling that software Vulkan stays under and
//!   hardware can only beat, with a steadiness check that the count does not grow while the app runs.
//!
//! Counted on the measuring thread only, so the perform thread and the driver's own threads can't make it flaky.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};

use egui::Context;
use omuzaki_core::glam::UVec2;
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{AlphaEncoding, Renderer, TargetDesc};
use omuzaki_track_mock::FakeTracker;
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::live::viewport::{NativePresenter, Presenter, kit_stage, published};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, RealClock};

struct Counting;

thread_local! {
    // `const` init with no destructor: reading or writing them never allocates, so they are safe inside the
    // allocator. Both are per thread, so tests running in parallel can't count each other's allocations.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn count() {
    if MEASURING.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations `work` makes on this thread.
fn allocations<T>(work: impl FnOnce() -> T) -> (usize, T) {
    let before = COUNT.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    let out = work();
    MEASURING.with(|m| m.set(false));
    (COUNT.with(Cell::get) - before, out)
}

#[test]
fn the_counter_sees_an_allocation_on_the_measuring_thread() {
    let (count, boxed) = allocations(|| std::hint::black_box(Box::new([0_u8; 64])));
    drop(boxed);
    assert!(count >= 1);
}

#[test]
fn showing_the_picture_to_egui_allocates_nothing_once_the_texture_is_registered() {
    let gpu = support::render_state();
    let mut renderer = WgpuRenderer::from_device(&gpu.adapter, gpu.device.clone(), gpu.queue.clone()).unwrap();
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(64, 64), alpha: AlphaEncoding::Premultiplied }).unwrap();
    let mut presenter = NativePresenter::new(gpu);
    let ctx = Context::default();

    // The first picture registers the texture: a view, a sampler and a bind group. The counter must see that.
    let (first, id) = allocations(|| presenter.present(&ctx, &mut renderer, target).unwrap());
    assert!(first > 0, "registering a texture allocates, so a count of 0 below means something");
    let (steady, ()) = allocations(|| {
        for _ in 0..200 {
            assert_eq!(presenter.present(&ctx, &mut renderer, target).unwrap(), id);
        }
    });
    println!("registering: {first} allocations; 200 steady frames: {steady}");
    assert_eq!(steady, 0, "a frame that changes nothing about the target allocates nothing here");
}

/// The most allocations one warm frame may make on this thread. Software Vulkan measures about 450 here (a debug
/// build); this is twice that, so another software driver's version stays under it, and a hardware driver, which
/// allocates less, always does. It catches a frame that starts allocating in proportion to something (a loop of
/// allocations per mesh or per light); the presenter's own 0 is pinned exactly by the test above.
const FRAME_CEILING: usize = 900;

/// Rendered frames left out as warm-up, then measured, and the width of the windows steadiness compares.
const WARM_UP: usize = 10;
const MEASURED: usize = 36;
const WINDOW: usize = 12;

#[test]
fn a_warm_frame_stays_under_the_allocation_ceiling_and_does_not_grow() {
    omuzaki_ui::link();
    let gpu = support::render_state();
    let size = UVec2::new(160, 200);
    let mut renderer = WgpuRenderer::from_device(&gpu.adapter, gpu.device.clone(), gpu.queue.clone()).unwrap();
    let stage = kit_stage(&mut renderer, size).expect("the kit stage starts");
    let base = LiveScene::base_pose(&stage);
    let mut app = LiveApp::start(
        Box::new(FakeTracker::scripted(Vec::new())),
        Box::new(RealClock::new()),
        Transparency::Fallback,
        base,
    )
    .expect("the live slice starts");
    app.attach_viewport(renderer, stage, size, Box::new(NativePresenter::new(gpu))).expect("attached");
    let ctx = Context::default();
    let drawn = |app: &mut LiveApp| app.viewport_mut().unwrap().scene().last_frame().map(|frame| frame.tick.index);

    // Frames that rendered, each with its allocation count: `show` draws only when the perform thread has a new frame.
    let mut counts = Vec::with_capacity(MEASURED);
    let (mut frames, deadline) = (0_usize, Instant::now() + Duration::from_secs(120));
    while counts.len() < MEASURED && Instant::now() < deadline {
        let before = drawn(&mut app);
        let (made, ()) = allocations(|| app.show(&ctx));
        if drawn(&mut app) == before {
            // Paces the loop; what it waits for is the perform thread's next frame.
            std::thread::sleep(Duration::from_millis(2));
        } else {
            frames += 1;
            if frames > WARM_UP {
                counts.push(made);
            }
        }
    }
    assert_eq!(counts.len(), MEASURED, "the perform thread kept supplying frames");
    assert!(published(&ctx).is_some());

    let worst = *counts.iter().max().unwrap();
    let best = *counts.iter().min().unwrap();
    let (early, late) = (counts[..WINDOW].iter().sum::<usize>(), counts[MEASURED - WINDOW..].iter().sum::<usize>());
    println!(
        "warm frames: min {best}, max {worst} allocations; first {WINDOW} total {early}, last {WINDOW} total {late}"
    );
    assert!(worst <= FRAME_CEILING, "a warm frame made {worst} allocations (ceiling {FRAME_CEILING}): {counts:?}");
    // Steady: the count does not creep up as the app runs (a leak-shaped growth), with room for allocator noise.
    assert!(late <= early + early / 5 + 30, "the count grew from {early} to {late} over {MEASURED} frames: {counts:?}");
    app.shutdown().expect("a clean shutdown");
}
