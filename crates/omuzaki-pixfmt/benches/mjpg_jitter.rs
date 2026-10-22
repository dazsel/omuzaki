//! MJPG decode time, its tail and its allocations at the three webcam sizes (#886): the measurement behind
//! `docs/research/2026-10-mjpg-decode-allocation.md`. Frames are synthetic (a smooth gradient with fine detail, encoded
//! here at quality 90), never a camera clip.
//!
//! `cargo bench -p omuzaki-pixfmt --bench mjpg_jitter` prints one row per size: the median and p99 decode time, the
//! worst one, and the allocations and bytes allocated per decode, counted by the allocator below.

// The counting allocator; the workspace denies unsafe code by default.
// The synthetic frame and the statistics cast between integers and floats on purpose.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageEncoder};
use omuzaki_pixfmt::{MjpgDecoder, RgbOut};

struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` unchanged; the counters are the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const SIZES: [(u32, u32); 3] = [(640, 480), (1280, 720), (1920, 1080)];
const WARM_UP: usize = 20;
const RUNS: usize = 400;

/// A camera-like frame: smooth light falling across the picture plus fine texture, so the JPEG has both flat areas and
/// detail, as a face in a room does.
fn frame(width: u32, height: u32) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height {
        for x in 0..width {
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let grain = ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 27) as f32;
            let ring = ((u - 0.5).hypot(v - 0.45) * 40.0).sin() * 12.0;
            rgb.push((180.0 * (1.0 - v) + 40.0 + ring + grain) as u8);
            rgb.push((150.0 * u + 50.0 + grain) as u8);
            rgb.push((120.0 * v + 60.0 - ring) as u8);
        }
    }
    rgb
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 90)
        .write_image(&frame(width, height), width, height, ExtendedColorType::Rgb8)
        .expect("the frame encodes");
    out
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// What the decoder's allocations cost on their own: the same number of zeroed blocks of the same total size, made and
/// freed, timed over many rounds. An upper bound on what reusing scratch buffers could save per decode.
fn allocation_cost(count: f64, bytes: f64) -> Duration {
    const ROUNDS: u32 = 2000;
    let (count, each) = (count.round() as usize, (bytes / count.max(1.0)) as usize);
    let start = Instant::now();
    for _ in 0..ROUNDS {
        for _ in 0..count {
            black_box(vec![0u8; each]);
        }
    }
    start.elapsed() / ROUNDS
}

fn main() {
    // `cargo bench` passes `--bench`; `cargo test --benches` runs it once quickly to prove it builds and decodes.
    let quick = !std::env::args().any(|a| a == "--bench");
    let (warm_up, runs) = if quick { (1, 3) } else { (WARM_UP, RUNS) };
    println!("| size | JPEG | p50 | p99 | worst | allocations per decode | bytes per decode | their cost |");
    println!("|---|---|---|---|---|---|---|---|");
    for (width, height) in SIZES {
        let data = jpeg(width, height);
        let mut decoder = MjpgDecoder::new();
        let mut rgb = vec![0u8; width as usize * height as usize * 3];
        let mut decode = |rgb: &mut [u8]| {
            let mut out = RgbOut { data: rgb, width, height };
            decoder.decode(black_box(&data), &mut out).expect("decodes");
        };
        for _ in 0..warm_up {
            decode(&mut rgb);
        }
        let mut times = Vec::with_capacity(runs);
        let (allocations, bytes) = (ALLOCATIONS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed));
        for _ in 0..runs {
            let start = Instant::now();
            decode(&mut rgb);
            times.push(start.elapsed());
        }
        // `times` was reserved before the loop, so every allocation counted is the decoder's.
        let per_allocations = (ALLOCATIONS.load(Ordering::Relaxed) - allocations) as f64 / runs as f64;
        let per_bytes = (BYTES.load(Ordering::Relaxed) - bytes) as f64 / runs as f64;
        times.sort_unstable();
        println!(
            "| {width}x{height} | {} KiB | {:.2} ms | {:.2} ms | {:.2} ms | {per_allocations:.1} | {:.0} KiB | {:.1} us |",
            data.len() / 1024,
            percentile(&times, 0.5).as_secs_f64() * 1e3,
            percentile(&times, 0.99).as_secs_f64() * 1e3,
            times[times.len() - 1].as_secs_f64() * 1e3,
            per_bytes / 1024.0,
            allocation_cost(per_allocations, per_bytes).as_secs_f64() * 1e6,
        );
    }
}
