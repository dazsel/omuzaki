//! MJPG decode time and allocations (#886, #1851): `MjpgDecoder` against zune-jpeg alone, on synthetic webcam-like
//! frames made by the test encoder (never a camera clip), at 640x480, 1280x720 and 1920x1080, 4:2:2 and 4:2:0.
//!
//! `cargo bench -p omuzaki-pixfmt --bench mjpg` prints one row per case: p50, p99 and max over the frames, and the
//! allocations per decode after warm-up. zune-jpeg gets the same frames with their Huffman tables written out, because
//! it can't decode the table-less frames webcams send.

// The counting allocator; the workspace denies unsafe code by default.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

#[path = "../tests/support/jpeg_encoder.rs"]
mod jpeg_encoder;

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jpeg_encoder::{Options, Sampling, encode, picture};
use omuzaki_pixfmt::{MjpgDecoder, RgbOut};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

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

/// Decodes the frames in turn `rounds` times after one warm-up pass; returns the sorted times and allocations per
/// decode.
fn run(frames: &[Vec<u8>], rounds: usize, mut decode: impl FnMut(&[u8])) -> (Vec<Duration>, f64) {
    for f in frames {
        decode(f);
    }
    let mut times = Vec::with_capacity(frames.len() * rounds);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..rounds {
        for f in frames {
            let t = Instant::now();
            decode(f);
            times.push(t.elapsed());
        }
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    times.sort_unstable();
    (times, allocations as f64 / (frames.len() * rounds) as f64)
}

fn row(name: &str, (times, allocs): (Vec<Duration>, f64)) {
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e3;
    println!(
        "{name:<34} p50 {:>6.2} ms  p99 {:>6.2} ms  max {:>6.2} ms  {allocs:>5.1} allocations/decode",
        at(0.5),
        at(0.99),
        at(1.0)
    );
}

fn main() {
    // `cargo bench` passes `--bench`; a test run (`cargo test --benches`) passes nothing and gets a short smoke run.
    let full = std::env::args().any(|a| a == "--bench");
    let rounds = if full { 100 } else { 1 };
    for (w, h) in [(640, 480), (1280, 720), (1920, 1080)] {
        // Four different frames, as a camera's frames differ.
        let pictures: Vec<Vec<u8>> = (1..=4).map(|seed| picture(w, h, seed)).collect();
        for sampling in [Sampling::S422, Sampling::S420] {
            let o = Options { quality: 85, sampling, restart: 0, dht: false };
            let frames: Vec<Vec<u8>> = pictures.iter().map(|p| encode(p, w, h, o)).collect();
            let with_tables: Vec<Vec<u8>> =
                pictures.iter().map(|p| encode(p, w, h, Options { dht: true, ..o })).collect();
            let kb = frames.iter().map(Vec::len).sum::<usize>() / frames.len() / 1024;
            let mut buf = vec![0_u8; w * h * 3];
            let mut dec = MjpgDecoder::new();
            let ours = run(&frames, rounds, |jpeg| {
                let mut out = RgbOut { data: &mut buf, width: w as u32, height: h as u32 };
                dec.decode(jpeg, &mut out).unwrap();
                black_box(&out.data[0]);
            });
            assert_eq!(dec.fallbacks(), 0);
            let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB).set_strict_mode(true);
            let zune = run(&with_tables, rounds, |jpeg| {
                let mut d = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
                d.decode_into(&mut buf).unwrap();
                black_box(&buf[0]);
            });
            println!("-- {w}x{h} {sampling:?}, {kb} KB a frame");
            row("MjpgDecoder (allocation-free path)", ours);
            row("zune-jpeg 0.5 decode_into", zune);
        }
    }
}
