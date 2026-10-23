//! Proves the conversions allocate nothing: 100 frames at 1280x720 for NV12, YUY2, BGRA and RGB, and MJPG at 720p and
//! 1080p once the decoder has seen the frame size.
//! Same counting allocator as `omuzaki-anim-mock`'s hot-loop test: it counts only the measuring thread's allocations, so
//! the test harness's own threads cannot make it flaky.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "support/jpeg_encoder.rs"]
mod jpeg_encoder;

use jpeg_encoder::{Options, Sampling, encode, picture};
use omuzaki_pixfmt::{MjpgDecoder, Plane, RgbOut, Yuv, bgra_to_rgb, nv12_to_rgb, rgb_to_rgb, yuy2_to_rgb};

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

/// Allocations made by `f` on this thread.
fn count(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

#[test]
fn mjpg_at_720p_and_1080p_allocates_nothing_after_the_first_frame() {
    // Two pictures per size, so the steady state covers frames of different content and length.
    let configs = [
        Options { sampling: Sampling::S422, restart: 0, dht: false, quality: 85 },
        Options { sampling: Sampling::S420, restart: 8, dht: true, quality: 90 },
    ];
    let mut dec = MjpgDecoder::new();
    let mut frames = Vec::new();
    for (w, h) in [(1280_usize, 720_usize), (1920, 1080)] {
        let pictures = [picture(w, h, 1), picture(w, h, 2)];
        for o in configs {
            let jpegs = pictures.each_ref().map(|p| encode(p, w, h, o));
            let mut buf = vec![0_u8; w * h * 3];
            let mut decode = |jpeg: &[u8]| {
                let mut out = RgbOut { data: &mut buf, width: w as u32, height: h as u32 };
                dec.decode(jpeg, &mut out).unwrap();
            };
            decode(&jpegs[0]); // warm-up: the first frame of a size sizes the planes
            let allocations = count(|| {
                for i in 0..10 {
                    decode(&jpegs[i % 2]);
                }
            });
            assert_eq!(allocations, 0, "{w}x{h} {o:?}: {allocations} allocations in 10 frames");
            frames.push((w, h, jpegs[1].clone()));
        }
    }
    // Back down to 720p and up again: the planes only grow, so a mode change after both sizes allocates nothing.
    let mut bufs: Vec<Vec<u8>> = frames.iter().map(|(w, h, _)| vec![0_u8; w * h * 3]).collect();
    let allocations = count(|| {
        for ((w, h, jpeg), buf) in frames.iter().zip(&mut bufs).rev() {
            let mut out = RgbOut { data: buf, width: *w as u32, height: *h as u32 };
            dec.decode(jpeg, &mut out).unwrap();
        }
    });
    assert_eq!(allocations, 0, "switching between 1080p and 720p allocated");
    assert_eq!(dec.fallbacks(), 0, "every frame took the allocation-free path");
}
