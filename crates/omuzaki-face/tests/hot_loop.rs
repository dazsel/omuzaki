//! Proves `FaceSystem::update` allocates nothing once the draw lists have warmed up (spec §10 face-paint budget,
//! §39.3 standard 2): the mapping, the painter, the fingerprint comparison and the changed-layer bookkeeping, both for
//! an unchanged face and for a face whose layers change every frame.
//!
//! It lives in its own test file, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times, which makes a process-wide count flaky under load. The
//! rasteriser is a no-op: rasterising is the CPU rasteriser's budget, not the system's.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::time::Seconds;
use omuzaki_face::{FaceSystem, RealPainter};
use omuzaki_face_api::{DEFAULT_PX_PER_M, DrawList, FaceError, FaceLayerSpec, FaceRasterizer, FaceView, Raster};
use omuzaki_face_mock::{MockPainter, mock_profile, mock_projection};

struct Counting;

thread_local! {
    // Both cells are `const`-initialised without a destructor: touching them never allocates, so it is safe inside the
    // allocator. The count is per thread too, so tests running side by side can't see each other's allocations.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Runs `f` and returns how many allocations this thread made during it.
fn count_allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
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

struct NullRasterizer;

impl FaceRasterizer for NullRasterizer {
    fn rasterize(&mut self, _: &FaceLayerSpec, _: &DrawList, _: f32, _: &mut Raster) -> Result<(), FaceError> {
        Ok(())
    }
}

/// Runs 1000 frames alternating between an open and a shut left eye and returns the allocations they made.
fn allocations_over_1000_frames(mut system: FaceSystem) -> usize {
    let profile = mock_profile();
    let (open, mut shut) = (ChannelBuf::NEUTRAL, ChannelBuf::NEUTRAL);
    shut.set(Channel::EYE_BLINK_LEFT, 1.0);
    shut.set(Channel::BROW_INNER_UP, 1.0);
    let mut frame = |i: u32| {
        let buf = if i.is_multiple_of(2) { &open } else { &shut };
        system.update(buf, &profile, Seconds(f64::from(i)), FaceView::default()).unwrap();
    };
    // Warm-up: both poses, so every list and path arena reaches its working size.
    frame(0);
    frame(1);

    count_allocations(|| {
        for i in 2..1002 {
            frame(i);
        }
    })
}

#[test]
fn the_counter_sees_an_allocation() {
    let allocations = count_allocations(|| {
        black_box(Vec::<u8>::with_capacity(64));
    });
    assert_eq!(allocations, 1, "without this the zero-allocation tests below prove nothing");
}

#[test]
fn an_update_with_the_real_painter_allocates_nothing() {
    let painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let allocations = allocations_over_1000_frames(FaceSystem::new(Box::new(painter), Box::new(NullRasterizer)));
    assert_eq!(allocations, 0, "1000 face updates made {allocations} allocations");
}

#[test]
fn an_update_whose_layers_change_every_frame_allocates_nothing() {
    let system = FaceSystem::new(Box::new(MockPainter::default()), Box::new(NullRasterizer));
    let allocations = allocations_over_1000_frames(system);
    assert_eq!(allocations, 0, "1000 changing face updates made {allocations} allocations");
}
