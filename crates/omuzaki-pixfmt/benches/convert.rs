//! 1280x720 conversion speed on one thread. Budgets (PF-01): NV12 and YUY2 <= 2.5 ms, BGRA <= 1.5 ms, MJPG <= 8 ms.
//!
//! MJPG needs a 1280x720 JPEG, which is too big to commit: set `OMUZAKI_PIXFMT_JPEG` to one (for example a 1280x720
//! Pillow save at quality 90) and the MJPG case runs, with the allocations per decode printed. Without it the case is skipped.

// The counting allocator behind the MJPG allocation report; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_truncation)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_pixfmt::{MjpgDecoder, Plane, RgbOut, Yuv, bgra_to_rgb, nv12_to_rgb, yuy2_to_rgb};

struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
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

fn convert(c: &mut Criterion) {
    let (w, h) = (W as usize, H as usize);
    let noise = |n: usize| (0..n).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect::<Vec<u8>>();
    let (y, uv, yuy2, bgra) = (noise(w * h), noise(w * h / 2), noise(w * h * 2), noise(w * h * 4));
    let mut buf = vec![0u8; w * h * 3];
    let yuv = Yuv::default();
    let mut group = c.benchmark_group("convert_1280x720");
    group.bench_function("nv12", |b| {
        b.iter(|| {
            let mut out = RgbOut { data: &mut buf, width: W, height: H };
            nv12_to_rgb(Plane { data: &y, stride: w }, Plane { data: &uv, stride: w }, yuv, &mut out).unwrap();
            black_box(&out.data[0]);
        });
    });
    group.bench_function("yuy2", |b| {
        b.iter(|| {
            let mut out = RgbOut { data: &mut buf, width: W, height: H };
            yuy2_to_rgb(Plane { data: &yuy2, stride: w * 2 }, yuv, &mut out).unwrap();
            black_box(&out.data[0]);
        });
    });
    group.bench_function("bgra", |b| {
        b.iter(|| {
            let mut out = RgbOut { data: &mut buf, width: W, height: H };
            bgra_to_rgb(Plane { data: &bgra, stride: w * 4 }, false, &mut out).unwrap();
            black_box(&out.data[0]);
        });
    });
    if let Ok(path) = std::env::var("OMUZAKI_PIXFMT_JPEG") {
        let jpeg = std::fs::read(path).unwrap();
        let mut dec = MjpgDecoder::new();
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        dec.decode(&jpeg, &mut RgbOut { data: &mut buf, width: W, height: H }).unwrap();
        println!("mjpg: {} allocations per decode", ALLOCATIONS.load(Ordering::Relaxed) - before);
        group.bench_function("mjpg", |b| {
            b.iter(|| {
                let mut out = RgbOut { data: &mut buf, width: W, height: H };
                dec.decode(&jpeg, &mut out).unwrap();
                black_box(&out.data[0]);
            });
        });
    } else {
        println!("mjpg: skipped (set OMUZAKI_PIXFMT_JPEG to a 1280x720 JPEG)");
    }
    group.finish();
}

criterion_group!(benches, convert);
criterion_main!(benches);
