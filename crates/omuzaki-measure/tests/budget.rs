//! The decoder limits (#282). A counting global allocator measures the peak a decode holds at once on the measuring
//! thread: a crafted header over the limits is refused before any pixel buffer is allocated. Counters are per thread (a thread-local flag, as `crates/omuzaki-stage/tests/perform.rs` uses), so no
//! lock is needed: the tests run in parallel, and one thread never sees another's allocations.

// The counting allocator wraps `System` and is the only `unsafe` here: a test measures the decode with it, and nothing
// in the library depends on it.
#![allow(unsafe_code, reason = "a counting global allocator wraps System, for the budget test only")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ::image::codecs::png::PngEncoder;
use ::image::{ExtendedColorType, ImageEncoder};
use omuzaki_measure::image::decode;
use omuzaki_ops_api::ErrorCode;
use omuzaki_store_api::ContentHash;

const FIELD: &str = "image";

thread_local! {
    // Per thread, so the tests run in parallel without a lock: only the thread that set `MEASURING` counts, and only its
    // own allocations are seen. `const` initialisers allocate nothing, so the allocator can use them.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

/// Adds `delta` bytes to the measuring thread's live count, and keeps its peak. Silent on other threads, and during
/// thread teardown, when the locals are gone.
fn note(delta: isize) {
    let _ = MEASURING.try_with(|on| {
        if on.get() {
            let _ = LIVE.try_with(|live| {
                let now = live.get() + delta;
                live.set(now);
                let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
            });
        }
    });
}

struct Counting;

// SAFETY: every call is forwarded to `System` unchanged; `note` only reads and writes thread-local counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            note(layout.size().cast_signed());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        note(-(layout.size().cast_signed()));
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            note(-(layout.size().cast_signed()));
            note(new_size.cast_signed());
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `f` on this thread and returns its result with the most bytes it held at once. Only this thread's allocations
/// are counted, so the tests may run in parallel; a frame freed inside `f` that was allocated before it lowers the live
/// count, which cannot raise the peak.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    MEASURING.with(|on| on.set(true));
    let result = f();
    MEASURING.with(|on| on.set(false));
    (result, PEAK.with(Cell::get).max(0).cast_unsigned())
}

/// The CRC-32 of a PNG chunk (ISO 3309).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A PNG that stops after its header: the size and the colour, and an empty image data chunk. It holds no pixels, so a
/// decoder that allocated for the size it declares would allocate hundreds of MiB for nothing.
fn png_header(width: u32, height: u32, depth: u8, colour: u8) -> Vec<u8> {
    let mut chunk = b"IHDR".to_vec();
    chunk.extend(width.to_be_bytes());
    chunk.extend(height.to_be_bytes());
    chunk.extend([depth, colour, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend(13_u32.to_be_bytes());
    out.extend(&chunk);
    out.extend(crc32(&chunk).to_be_bytes());
    out.extend(0_u32.to_be_bytes());
    out.extend(b"IDAT");
    out.extend(crc32(b"IDAT").to_be_bytes());
    out
}

#[test]
fn a_crafted_oversized_header_is_refused_before_any_pixel_is_allocated() {
    // 10,000 x 10,000 is over the 8192 side limit (400 MB as RGBA8); 8000 x 8000 and 5000 x 5000 are under it but over
    // the 64 MiB decoded-size limit (256 MB and 100 MB as RGBA8). None has a pixel, and none may allocate for the size it
    // claims.
    for (width, height, depth, colour) in [(10_000, 10_000, 8, 2), (8_000, 8_000, 8, 6), (5_000, 5_000, 8, 6)] {
        let png = png_header(width, height, depth, colour);
        let (result, peak) = peak_of(|| decode(&png, ContentHash::of(&png), FIELD));
        let error = result.unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid);
        assert!(
            error.message.contains("A measure reads at most 8,192 px a side and 64 MiB"),
            "{width}x{height}x{depth}: {}",
            error.message
        );
        assert!(peak < 1 << 20, "{width} x {height}: a refused header allocated {peak} bytes");
    }
}

fn solid_rgba_png(side: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let pixels = [200_u8, 120, 60, 255].repeat((side * side) as usize);
    PngEncoder::new(&mut out).write_image(&pixels, side, side, ExtendedColorType::Rgba8).unwrap();
    out
}

#[test]
fn a_solid_untagged_rgba_png_at_three_thousand_pixels_is_accepted_and_its_buffer_counted_once() {
    // 3000 x 3000 RGBA8 is 34.3 MiB decoded, under the 64 MiB limit. The decoder's buffer is that RGBA8 already, and
    // `into_rgba8` moves it, so the decode holds the image once (plus the caller's input), not twice.
    let png = solid_rgba_png(3000);
    let decoded = 3000_usize * 3000 * 4;
    let (result, peak) = peak_of(|| decode(&png, ContentHash::of(&png), FIELD));
    let image = result.unwrap();
    assert_eq!((image.width, image.height), (3000, 3000));
    assert_eq!(&image.pixels[..4], &[200, 120, 60, 255]);
    assert!(peak < decoded + (1 << 20), "the decode held {peak} bytes for a {decoded}-byte image");
}

#[test]
fn the_worst_case_colour_conversion_peak_is_within_the_documented_multiple() {
    // A 16-bit RGBA image that needs a conversion is the worst case: the decoder's 8 bytes a pixel, the same data as the
    // 16-bit input of the transform, and its 16-bit output are 16 bytes a pixel, `PEAK_FACTOR` (4) times the decoded RGBA8.
    let side = 1500_u32;
    let samples: Vec<u8> =
        (0..side * side * 4).flat_map(|i| (u16::try_from(i % 60_000).unwrap() + 100).to_ne_bytes()).collect();
    let mut png = Vec::new();
    PngEncoder::new(&mut png).write_image(&samples, side, side, ExtendedColorType::Rgba16).unwrap();
    let gamma = 100_000_u32.to_be_bytes();
    let mut chunk = 4_u32.to_be_bytes().to_vec();
    chunk.extend(b"gAMA");
    chunk.extend(gamma);
    chunk.extend(crc32(&[b"gAMA".as_slice(), gamma.as_slice()].concat()).to_be_bytes());
    let idat = png.windows(4).position(|window| window == b"IDAT").unwrap() - 4;
    let png = [&png[..idat], &chunk, &png[idat..]].concat();
    let decoded = (side * side * 4) as usize;
    let (result, peak) = peak_of(|| decode(&png, ContentHash::of(&png), FIELD));
    result.unwrap();
    let limit = usize::try_from(omuzaki_measure::image::PEAK_FACTOR).unwrap() * decoded + (1 << 20);
    assert!(peak <= limit, "the conversion held {peak} bytes; {} x the decoded {decoded} is {limit}", 4);
    assert!(peak > 3 * decoded, "the worst case should be near the multiple, not below 3x: {peak}");
}
