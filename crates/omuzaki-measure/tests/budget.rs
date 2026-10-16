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

/// A baseline JPEG with zeros after its end marker, `len` bytes in all. The decoder reads the whole input into a buffer
/// of its own, so these bytes are held twice.
fn padded_jpeg(len: usize) -> Vec<u8> {
    let mut jpeg = include_bytes!("fixtures/baseline_444.jpg").to_vec();
    jpeg.resize(len, 0);
    jpeg
}

#[test]
fn decoded_peaks_stay_within_the_header_prediction() {
    let cases = [
        ("baseline 4:2:0 JPEG", include_bytes!("fixtures/baseline_420.jpg").to_vec()),
        ("baseline 4:4:4 JPEG", include_bytes!("fixtures/baseline_444.jpg").to_vec()),
        ("progressive 4:4:4 JPEG", include_bytes!("fixtures/progressive_444.jpg").to_vec()),
        ("16-bit RGBA PNG", png16_rgba(96, 64)),
        ("grey PNG", png_grey(96, 64)),
        ("baseline JPEG padded to 20 MiB", padded_jpeg(20 << 20)),
    ];
    for (name, bytes) in cases {
        let predicted = predicted_peak(&bytes).unwrap_or_else(|| panic!("{name}: the header predicts no peak"));
        let (decoded, measured) = peak_of(|| decode(&bytes, ContentHash::of(&bytes), FIELD));
        decoded.unwrap_or_else(|error| panic!("{name}: {}", error.message));
        assert!(measured > 0, "{name}: nothing was counted");
        // The caller's buffer was allocated before the measure began, and it is part of what the prediction counts.
        let held = (measured + bytes.len()) as u64;
        assert!(
            held <= predicted,
            "{name}: the decode and its input held {held} bytes, the header predicted {predicted}"
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

/// A JPEG with `chunks` APP2 chunks of 65 519 profile bytes each (about 64 KiB apiece) after its start, the way a camera
/// writes a long profile.
fn jpeg_with_profile_chunks(chunks: usize) -> Vec<u8> {
    let fixture = include_bytes!("fixtures/baseline_444.jpg");
    let mut jpeg = fixture[..2].to_vec();
    for _ in 0..chunks {
        jpeg.extend([0xFF, 0xE2]);
        jpeg.extend(u16::try_from(2 + 14 + 65_519).unwrap().to_be_bytes());
        jpeg.extend(b"ICC_PROFILE\0\x01\x01");
        jpeg.extend(vec![0_u8; 65_519]);
    }
    jpeg.extend(&fixture[2..]);
    jpeg
}

#[test]
fn a_declared_jpeg_profile_over_the_budget_is_refused_before_a_decoder_is_built() {
    // 510 chunks: about 33 MB declared, which two copies put over the 64 MiB cap. The refusal comes from the declared
    // length alone, so the decode allocates next to nothing beyond the input it was handed.
    let jpeg = jpeg_with_profile_chunks(510);
    let (result, peak) = peak_of(|| decode(&jpeg, ContentHash::of(&jpeg), FIELD));
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.starts_with("The image is 96 × 64"), "{}", error.message);
    assert!(peak < 1 << 20, "a refused declaration allocated {peak} bytes");
}

#[test]
fn an_oversized_png_profile_is_refused_while_the_header_is_read_and_never_inflated_past_its_bound() {
    // A 40 MiB profile of zeros compresses to about 40 KiB, so the file is tiny; inflated it is over the 16 MiB a profile
    // may take. The decoder stops at the bound, so the decode holds a few times that at most, never the 40 MiB the chunk
    // claims twice over (the bare decoder took 111 MB on this file).
    let mut png = Vec::new();
    let mut encoder = PngEncoder::new(&mut png);
    encoder.set_icc_profile(vec![0_u8; 40 << 20]).unwrap();
    encoder.write_image(&pattern(8, 8, 3), 8, 8, ExtendedColorType::Rgb8).unwrap();
    assert!(png.len() < 1 << 20, "the crafted PNG is {} bytes", png.len());
    let (result, peak) = peak_of(|| decode(&png, ContentHash::of(&png), FIELD));
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.contains("inflates to more than 16 MiB"), "{}", error.message);
    assert!(peak <= 48 << 20, "an oversized profile took {peak} bytes before it was refused");
}

#[test]
fn a_profile_and_a_pixel_area_that_each_fit_are_accepted_together() {
    // A 16-bit RGBA PNG planned at about 60 MiB, with a 3 MiB profile. The profile is held twice only while it is
    // copied, before any pixel buffer exists, and once beside the pixels: 60 + 3 = 63 MiB, inside the 64 MiB cap. The
    // old single sum, 60 + 2 × 3 = 66 MiB, refused it. The profile is not a valid sRGB one, so the decode ends at the
    // colour check, past the budget: that is what shows the budget let it through.
    let mut png = Vec::new();
    let mut encoder = PngEncoder::new(&mut png);
    let mut profile = vec![0_u8; 3 << 20];
    profile[..4].copy_from_slice(&(3_u32 << 20).to_be_bytes());
    encoder.set_icc_profile(profile).unwrap();
    let side = 1_764;
    let samples: Vec<u8> =
        pattern(side, side, 4).iter().flat_map(|&byte| (u16::from(byte) * 257).to_ne_bytes()).collect();
    encoder.write_image(&samples, side, side, ExtendedColorType::Rgba16).unwrap();
    let plan = predicted_peak(&png).unwrap();
    let (cap, mib) = (64_u64 << 20, 1_u64 << 20);
    assert!(plan + 2 * (3 * mib) > cap && plan + 3 * mib <= cap, "the plan is {plan} bytes: adjust the side");
    let error = decode(&png, ContentHash::of(&png), FIELD).unwrap_err();
    assert!(error.message.contains("colour profile"), "{}", error.message);
}

#[test]
fn a_jpeg_whose_input_held_twice_is_over_the_cap_is_refused_before_a_decoder_is_built() {
    // 40 MiB of input is under the 64 MiB cap once, and over it twice: the JPEG decoder keeps a copy of all of it.
    let jpeg = padded_jpeg(40 << 20);
    let (result, peak) = peak_of(|| decode(&jpeg, ContentHash::of(&jpeg), FIELD));
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.starts_with("The image is 96 × 64"), "{}", error.message);
    assert!(peak < 1 << 20, "a refused input allocated {peak} bytes");
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

/// `png` with an sRGB `cICP` chunk before its image data, and zeros after its end until it is `len` bytes.
fn with_srgb_cicp_padded_to(png: &[u8], len: usize) -> Vec<u8> {
    let idat = png.windows(4).position(|window| window == b"IDAT").unwrap() - 4;
    let data = [1_u8, 13, 0, 1];
    let mut out = png[..idat].to_vec();
    out.extend(4_u32.to_be_bytes());
    out.extend(b"cICP");
    out.extend(data);
    out.extend(crc32(&[b"cICP".as_slice(), data.as_slice()].concat()).to_be_bytes());
    out.extend(&png[idat..]);
    out.resize(len, 0);
    out
}

#[test]
fn a_profile_beside_an_srgb_cicp_is_not_retained_past_the_budget_with_the_callers_buffer() {
    // A tiny PNG with a profile that inflates to 15 MiB, an sRGB `cICP`, and zeros after its end that make the encoded
    // input 50 MiB. Read as a PNG the input is held once, 50 MiB, and the profile the decoder would keep is 15 MiB more:
    // over the 64 MiB cap together with the pixels. The decoder is bounded to what is left, so the profile is dropped, and
    // the input plus everything the decode held stays inside the cap.
    let mut png = Vec::new();
    let mut encoder = PngEncoder::new(&mut png);
    encoder.set_icc_profile(vec![0_u8; 15 << 20]).unwrap();
    encoder.write_image(&pattern(8, 8, 3), 8, 8, ExtendedColorType::Rgb8).unwrap();
    let padded = with_srgb_cicp_padded_to(&png, 50 << 20);
    let (result, peak) = peak_of(|| decode(&padded, ContentHash::of(&padded), FIELD));
    let held = peak + padded.len();
    assert!(held <= 64 << 20, "the decode and its input held {held} bytes");
    assert!(result.is_ok(), "an sRGB cICP decides the colour, so the profile beside it is no reason to refuse");
}

#[test]
fn an_input_that_leaves_the_decoder_no_room_is_refused_not_given_a_floor() {
    // A tiny PNG with a profile that inflates to 15 MiB, padded after its end to 63.5 MiB: the input leaves under the
    // 1 MiB the decoder needs for its metadata, so the image is refused as too large. The profile floor once handed back
    // room the input had used, and the decoder was built with it. Tried beside no colour chunk and beside an sRGB `cICP`.
    let mut png = Vec::new();
    let mut encoder = PngEncoder::new(&mut png);
    encoder.set_icc_profile(vec![0_u8; 15 << 20]).unwrap();
    encoder.write_image(&pattern(8, 8, 3), 8, 8, ExtendedColorType::Rgb8).unwrap();
    let len = (63 << 20) + (1 << 19);
    let mut plain = png.clone();
    plain.resize(len, 0);
    let with_cicp = with_srgb_cicp_padded_to(&png, len);
    for padded in [plain, with_cicp] {
        let (result, peak) = peak_of(|| decode(&padded, ContentHash::of(&padded), FIELD));
        let error = result.unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid);
        assert!(error.message.starts_with("The image is 8 × 8"), "{}", error.message);
        assert!(peak + padded.len() <= 64 << 20, "the refusal held {} bytes with its input", peak + padded.len());
    }
}

#[test]
fn a_million_and_a_half_empty_text_chunks_stay_inside_the_budget() {
    // 1.5 million empty `tEXt` chunks are 18 MB encoded. A decoder that parses text keeps a record for each, over 68 MiB
    // that no allocation limit counts. The text chunks are skipped, so the decode holds the input and little else.
    let mut png = Vec::new();
    PngEncoder::new(&mut png).write_image(&pattern(8, 8, 3), 8, 8, ExtendedColorType::Rgb8).unwrap();
    let idat = png.windows(4).position(|window| window == b"IDAT").unwrap() - 4;
    let mut empty = 0_u32.to_be_bytes().to_vec();
    empty.extend(b"tEXt");
    empty.extend(crc32(b"tEXt").to_be_bytes());
    let mut padded = png[..idat].to_vec();
    for _ in 0..1_500_000 {
        padded.extend(&empty);
    }
    padded.extend(&png[idat..]);
    let (result, peak) = peak_of(|| decode(&padded, ContentHash::of(&padded), FIELD));
    assert!(result.is_ok(), "{:?}", result.err());
    let held = peak + padded.len();
    assert!(held <= 64 << 20, "the decode and its input held {held} bytes");
    assert!(peak < 4 << 20, "skipping text should leave the decode almost nothing beyond the input, held {peak}");
}

/// `png` with an `eXIf` chunk of `len` zero bytes before its image data.
fn with_exif(png: &[u8], len: usize) -> Vec<u8> {
    let idat = png.windows(4).position(|window| window == b"IDAT").unwrap() - 4;
    let data = vec![0_u8; len];
    let mut out = png[..idat].to_vec();
    out.extend(u32::try_from(len).unwrap().to_be_bytes());
    out.extend(b"eXIf");
    out.extend(&data);
    out.extend(crc32(&[b"eXIf".as_slice(), data.as_slice()].concat()).to_be_bytes());
    out.extend(&png[idat..]);
    out
}

#[test]
fn exif_at_the_cap_is_accepted_inside_the_budget_and_over_it_is_refused() {
    // The decoder's own buffers for EXIF are its business, so the measure caps the input at 16 KiB and allows a fixed
    // amount for a block at the cap, inside the decoder's state allowance.
    let base = png_grey(96, 64);
    let at_cap = with_exif(&base, 16 << 10);
    let predicted = predicted_peak(&at_cap).unwrap();
    let (result, peak) = peak_of(|| decode(&at_cap, ContentHash::of(&at_cap), FIELD));
    result.unwrap();
    let held = (peak + at_cap.len()) as u64;
    assert!(held <= predicted, "the decode and its input held {held} bytes, the header predicted {predicted}");
    for len in [(16 << 10) + 1, 8 << 20, (4 << 20) + 1] {
        let png = with_exif(&base, len);
        let (result, peak) = peak_of(|| decode(&png, ContentHash::of(&png), FIELD));
        let error = result.unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid);
        assert!(
            error.message.contains(&format!("EXIF block too large: {len} bytes, limit 16 KiB")),
            "{}",
            error.message
        );
        assert!(peak < 1 << 20, "a refused EXIF block allocated {peak} bytes");
    }
    // The cap is on the sum of the blocks, not on each.
    let two = with_exif(&with_exif(&base, 10 << 10), 10 << 10);
    assert!(decode(&two, ContentHash::of(&two), FIELD).unwrap_err().message.contains("20480 bytes"));
}

/// Deflate (fixed Huffman) of `length` zero bytes: one literal, then matches of up to 258 bytes at distance 1.
fn deflate_zeros(length: usize) -> Vec<u8> {
    const BASE: [usize; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227,
        258,
    ];
    const EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
    let (mut out, mut acc, mut used) = (Vec::new(), 0_u64, 0_u32);
    let mut bits = |value: u32, count: u32, msb_first: bool, out: &mut Vec<u8>| {
        for i in 0..count {
            let bit = if msb_first { (value >> (count - 1 - i)) & 1 } else { (value >> i) & 1 };
            acc |= u64::from(bit) << used;
            used += 1;
            if used == 8 {
                out.push(u8::try_from(acc).unwrap());
                acc = 0;
                used = 0;
            }
        }
    };
    bits(1, 1, false, &mut out); // final block
    bits(1, 2, false, &mut out); // fixed Huffman
    bits(0b0011_0000, 8, true, &mut out); // the literal 0
    let mut left = length - 1;
    while left > 0 {
        let run = if left >= 3 { left.min(258) } else { 0 };
        if run == 0 {
            for _ in 0..left {
                bits(0b0011_0000, 8, true, &mut out);
            }
            break;
        }
        let index = BASE.iter().rposition(|&base| base <= run).unwrap();
        let code = 257 + u32::try_from(index).unwrap();
        if code < 280 {
            bits(code - 256, 7, true, &mut out);
        } else {
            bits(0xC0 + code - 280, 8, true, &mut out);
        }
        bits(u32::try_from(run - BASE[index]).unwrap(), EXTRA[index], false, &mut out);
        bits(0, 5, true, &mut out); // distance 1
        left -= run;
    }
    bits(0, 7, true, &mut out); // end of block
    if used > 0 {
        out.push(u8::try_from(acc).unwrap());
    }
    out
}

/// An Adam7 interlaced 8-bit grey PNG of zeros, `width` by `height`.
fn interlaced_grey_zeros(width: u32, height: u32) -> Vec<u8> {
    let passes = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];
    let raw: usize = passes
        .iter()
        .map(|&(x0, y0, dx, dy)| {
            let (w, h) =
                (width.saturating_sub(x0).div_ceil(dx) as usize, height.saturating_sub(y0).div_ceil(dy) as usize);
            if w == 0 || h == 0 { 0 } else { h * (1 + w) }
        })
        .sum();
    // zlib: header, the deflate stream, and the Adler-32 of the raw bytes (all zero but one filter byte per row, also 0).
    let (mut a, mut b) = (1_u32, 0_u32);
    for _ in 0..raw {
        b = (b + a) % 65_521; // a stays 1: every byte is 0
    }
    a %= 65_521;
    let mut zlib = vec![0x78, 0x01];
    zlib.extend(deflate_zeros(raw));
    zlib.extend(((b << 16) | a).to_be_bytes());
    let chunk = |kind: &[u8; 4], data: &[u8]| {
        let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        out.extend(kind);
        out.extend(data);
        out.extend(crc32(&[kind.as_slice(), data].concat()).to_be_bytes());
        out
    };
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 0, 0, 0, 1]);
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend(chunk(b"IHDR", &ihdr));
    png.extend(chunk(b"IDAT", &zlib));
    png.extend(chunk(b"IEND", &[]));
    png
}

#[test]
fn an_interlaced_png_near_the_size_limit_stays_inside_the_budget() {
    // Adam7 decodes pass by pass into the frame buffer, with the rows of each pass besides. 3200 x 3200 grey is 10.2 MP:
    // 61 MB planned, the largest this budget admits near the limit. The measured peak, with the caller's buffer, must be
    // inside both the cap and the header's prediction.
    let png = interlaced_grey_zeros(3200, 3200);
    let predicted = predicted_peak(&png).unwrap();
    assert!(predicted <= 64 << 20, "the fixture should be within the cap, predicted {predicted}");
    let (result, peak) = peak_of(|| decode(&png, ContentHash::of(&png), FIELD));
    let image = result.unwrap();
    assert_eq!((image.width, image.height), (3200, 3200));
    let held = (peak + png.len()) as u64;
    assert!(held <= predicted && held <= 64 << 20, "held {held} bytes, predicted {predicted}");
}
