//! The decoded budget. A counting global allocator measures the peak a decode holds at once on the measuring thread; it
//! must never exceed the peak the header predicts, and an image whose predicted peak is over the cap is refused before any
//! decoder runs. Counters are per thread (a thread-local flag, as `crates/omuzaki-stage/tests/perform.rs` uses), so no
//! lock is needed: the tests run in parallel, and one thread never sees another's allocations.
//!
//! The fixtures in `tests/fixtures` are small JPEGs of the synthetic pattern, generated with PIL (the generator is in
//! the doc comment of [`fixture_generator`]). The PNGs are encoded here from the same kind of pattern.

// The counting allocator wraps `System` and is the only `unsafe` here: a test measures the decode with it, and nothing
// in the library depends on it.
#![allow(unsafe_code, reason = "a counting global allocator wraps System, for the budget test only")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ::image::codecs::png::PngEncoder;
use ::image::{ExtendedColorType, ImageEncoder};
use omuzaki_measure::image::{decode, predicted_peak};
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

/// The synthetic pattern as an encoder's input: a gradient with a flat square, `channels` bytes a pixel.
fn pattern(width: u32, height: u32, channels: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height) as usize * channels);
    for y in 0..height {
        for x in 0..width {
            let inside = (width / 4..3 * width / 4).contains(&x) && (height / 4..3 * height / 4).contains(&y);
            let red = u8::try_from(x * 255 / width).unwrap();
            let green = u8::try_from(y * 255 / height).unwrap();
            let pixel = if inside { [20, 200, 60, 255] } else { [red, green, 128, 255] };
            out.extend(&pixel[..channels]);
        }
    }
    out
}

/// A 16-bit RGBA PNG of the pattern, in the native byte order the encoder takes for 16-bit samples.
fn png16_rgba(width: u32, height: u32) -> Vec<u8> {
    let samples: Vec<u8> =
        pattern(width, height, 4).iter().flat_map(|&byte| (u16::from(byte) * 257).to_ne_bytes()).collect();
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(&samples, width, height, ExtendedColorType::Rgba16).unwrap();
    out
}

fn png_grey(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(&pattern(width, height, 1), width, height, ExtendedColorType::L8).unwrap();
    out
}

/// A progressive JPEG frame header for a 4096 × 4096 4:4:4 image, cut off before its scan: enough for the header alone to
/// say how much a decode would take, and nothing for a decoder to read.
fn progressive_4096_header() -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xC2, 0x00, 0x11, 8, 0x10, 0x00, 0x10, 0x00, 3];
    out.extend([1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
    out
}

#[test]
fn decoded_peaks_stay_within_the_header_prediction() {
    let cases = [
        ("baseline 4:2:0 JPEG", include_bytes!("fixtures/baseline_420.jpg").to_vec()),
        ("baseline 4:4:4 JPEG", include_bytes!("fixtures/baseline_444.jpg").to_vec()),
        ("progressive 4:4:4 JPEG", include_bytes!("fixtures/progressive_444.jpg").to_vec()),
        ("16-bit RGBA PNG", png16_rgba(96, 64)),
        ("grey PNG", png_grey(96, 64)),
    ];
    for (name, bytes) in cases {
        let predicted = predicted_peak(&bytes).unwrap_or_else(|| panic!("{name}: the header predicts no peak"));
        let (decoded, measured) = peak_of(|| decode(&bytes, ContentHash::of(&bytes), FIELD));
        decoded.unwrap_or_else(|error| panic!("{name}: {}", error.message));
        assert!(measured > 0, "{name}: nothing was counted");
        assert!(
            measured as u64 <= predicted,
            "{name}: the decode held {measured} bytes, the header predicted {predicted}"
        );
    }
}

#[test]
fn a_progressive_header_predicted_over_the_cap_is_refused_before_any_decoder_runs() {
    // 4096 × 4096 at 4:4:4 keeps 96 MiB of coefficients, over the 64 MiB cap before the RGBA8 is counted.
    let bytes = progressive_4096_header();
    let predicted = predicted_peak(&bytes).expect("the frame header is read");
    assert!(predicted > 64 << 20, "predicted {predicted}");
    let (refused, measured) = peak_of(|| decode(&bytes, ContentHash::of(&bytes), FIELD));
    let error = refused.unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.contains("4,096 × 4,096"), "{}", error.message);
    assert!(measured < 1 << 20, "the refusal allocated {measured} bytes");
}

/// The JPEG fixtures are the kinds their names say, and small. Their generator is the PIL script in the doc comment on
/// [`fixture_generator`]; the test does not run it (PIL is not a dependency), it checks what the bytes are.
#[test]
fn the_jpeg_fixtures_are_the_kinds_they_are_named_and_stay_small() {
    let progressive = include_bytes!("fixtures/progressive_444.jpg");
    let baseline = include_bytes!("fixtures/baseline_444.jpg");
    let frame_marker = |bytes: &[u8], marker: u8| bytes.windows(2).any(|pair| pair == [0xFF, marker]);
    assert!(frame_marker(progressive, 0xC2), "progressive_444.jpg is not a progressive frame");
    assert!(frame_marker(baseline, 0xC0), "baseline_444.jpg is not a baseline frame");
    for bytes in [
        &progressive[..],
        &baseline[..],
        include_bytes!("fixtures/baseline_420.jpg"),
        include_bytes!("fixtures/cmyk.jpg"),
    ] {
        assert_eq!(&bytes[..2], [0xFF, 0xD8], "a fixture is not a JPEG");
        assert!(bytes.len() < 4 * 1024, "a fixture is {} bytes; the fixtures stay a few KB", bytes.len());
    }
}

/// The generator for the JPEG fixtures, run with PIL in `tests/fixtures`; the pattern is the one the PNGs above use, a
/// gradient with a flat square.
///
/// ```python
/// from PIL import Image
/// def pattern(mode):
///     img = Image.new('RGB', (96, 64)); px = img.load()
///     for y in range(64):
///         for x in range(96):
///             inside = 24 <= x < 72 and 16 <= y < 48
///             px[x, y] = (x * 255 // 95, y * 255 // 63, 128) if not inside else (20, 200, 60)
///     return img.convert(mode) if mode != 'RGB' else img
/// pattern('RGB').save('baseline_420.jpg', 'JPEG', quality=90, subsampling=2, optimize=False)
/// pattern('RGB').save('baseline_444.jpg', 'JPEG', quality=90, subsampling=0, optimize=False)
/// pattern('RGB').save('progressive_444.jpg', 'JPEG', quality=90, subsampling=0, progressive=True)
/// pattern('CMYK').save('cmyk.jpg', 'JPEG', quality=90)
/// ```
#[allow(dead_code, reason = "documentation: the generator the fixtures were made with")]
fn fixture_generator() {}
