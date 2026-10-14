//! Proves the raw-format conversions allocate nothing: 100 frames at 1280x720 for NV12, YUY2, BGRA and RGB.
//! Same counting allocator as `omuzaki-anim-mock`'s hot-loop test: it counts only the measuring thread's allocations, so
//! the test harness's own threads cannot make it flaky.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_pixfmt::{Plane, RgbOut, Yuv, bgra_to_rgb, nv12_to_rgb, rgb_to_rgb, yuy2_to_rgb};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const W: u32 = 1280;
const H: u32 = 720;

#[test]
fn a_hundred_conversions_at_720p_allocate_nothing() {
    let (w, h) = (W as usize, H as usize);
    let y = vec![100u8; w * h];
    let uv = vec![120u8; w * h / 2];
    let yuy2 = vec![100u8; w * h * 2];
    let bgra = vec![100u8; w * h * 4];
    let rgb = vec![100u8; w * h * 3];
    let mut buf = vec![0u8; w * h * 3];
    let yuv = Yuv::default();

    let run = |buf: &mut [u8]| {
        let mut out = RgbOut { data: buf, width: W, height: H };
        nv12_to_rgb(Plane { data: &y, stride: w }, Plane { data: &uv, stride: w }, yuv, &mut out).unwrap();
        yuy2_to_rgb(Plane { data: &yuy2, stride: w * 2 }, yuv, &mut out).unwrap();
        bgra_to_rgb(Plane { data: &bgra, stride: w * 4 }, true, &mut out).unwrap();
        rgb_to_rgb(Plane { data: &rgb, stride: w * 3 }, &mut out).unwrap();
    };
    run(&mut buf); // warm-up

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for _ in 0..100 {
        run(&mut buf);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "100 rounds of four conversions made {allocations} allocations");
}
