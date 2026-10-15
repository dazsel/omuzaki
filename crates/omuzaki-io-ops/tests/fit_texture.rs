//! Texture resizing for a platform export: the arithmetic a platform budget depends on, checked on synthetic images.
//!
//! Every input is built here. Expected values are either exact, or within one step where the reason is named at the
//! assertion (linear light is not a whole number of 8-bit steps).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    reason = "test fixtures: a panic is a clear failure, and the gradients wrap by design"
)]

use std::io::Cursor;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, ImageFormat, ImageReader, Rgba, RgbaImage};
use omuzaki_io_api::{Blob, EncodedImage, ImageMime, IoError};
use omuzaki_io_ops::fit_texture::{TextureUse, downscale_to, resize};

/// `image` as a PNG named `name`, with the image crate's default encoder settings.
fn png(name: &str, image: &RgbaImage) -> EncodedImage {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(image.as_raw(), image.width(), image.height(), ExtendedColorType::Rgba8)
        .expect("a PNG of a valid RGBA buffer encodes");
    EncodedImage::new(name, ImageMime::Png, Blob::new(bytes))
}

/// The pixels of a PNG `image` decoded back to RGBA8.
fn decode(image: &EncodedImage) -> RgbaImage {
    ImageReader::with_format(Cursor::new(image.bytes.as_slice()), ImageFormat::Png)
        .decode()
        .expect("the output is a PNG")
        .into_rgba8()
}

/// A 2 by 2 black and white checkerboard.
fn checkerboard() -> RgbaImage {
    RgbaImage::from_fn(2, 2, |x, y| if (x + y) % 2 == 0 { Rgba([0, 0, 0, 255]) } else { Rgba([255, 255, 255, 255]) })
}

/// A 4096 by 4096 gradient with a different ramp on each channel, so every output texel differs.
fn gradient() -> RgbaImage {
    RgbaImage::from_fn(4096, 4096, |x, y| Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]))
}

#[test]
fn a_colour_checkerboard_averages_in_linear_light() {
    let out = decode(&resize(&png("check", &checkerboard()), 1, 1, TextureUse::Colour).expect("resizes"));
    // Half of linear light is 0.5, which is 188 in sRGB (187.5 before rounding). Averaging the encoded values would
    // give 128. The tolerance of one step is for the rounding of the table.
    let value = out.get_pixel(0, 0)[0];
    assert!(value.abs_diff(188) <= 1, "colour mid grey was {value}, expected 188");
    assert_eq!(out.get_pixel(0, 0)[3], 255, "opaque in, opaque out");
}

#[test]
fn a_data_checkerboard_averages_as_stored() {
    let out = decode(&resize(&png("check", &checkerboard()), 1, 1, TextureUse::Data).expect("resizes"));
    // 127.5 rounds half away from zero to 128. Data has no curve, so the tolerance of one step only covers the
    // rounding the rest of the pipeline may add.
    let value = out.get_pixel(0, 0)[0];
    assert!(value.abs_diff(128) <= 1, "data mid value was {value}, expected 128");
}

#[test]
fn an_opaque_blue_next_to_a_transparent_red_is_blue_at_half_alpha() {
    let image = RgbaImage::from_fn(2, 1, |x, _| if x == 0 { Rgba([0, 0, 255, 255]) } else { Rgba([255, 0, 0, 0]) });
    let out = decode(&resize(&png("edge", &image), 1, 1, TextureUse::Colour).expect("resizes"));
    // Premultiplied: the transparent red contributes no colour, so the colour is blue, not purple. Alpha is the mean of
    // 255 and 0, which is 127.5 and rounds to 128. Every value here is exact, so the test is exact.
    assert_eq!(out.get_pixel(0, 0).0, [0, 0, 255, 128]);
}

#[test]
fn a_fully_transparent_output_takes_the_neighbours_colour_not_black() {
    let image = RgbaImage::from_fn(2, 1, |x, _| if x == 0 { Rgba([255, 0, 0, 0]) } else { Rgba([0, 255, 0, 0]) });
    let out = decode(&resize(&png("clear", &image), 1, 1, TextureUse::Colour).expect("resizes"));
    // No alpha to divide by, so the plain linear average of red and green is used: (0.5, 0.5, 0) in linear light,
    // which is about (188, 188, 0). Black here is the dark fringe the averaging exists to avoid.
    let [r, g, b, a] = out.get_pixel(0, 0).0;
    assert!(r.abs_diff(188) <= 1 && g.abs_diff(188) <= 1, "fringe colour was ({r}, {g}, {b})");
    assert_eq!(b, 0);
    assert_eq!(a, 0);
}

#[test]
fn a_one_pixel_feature_survives_the_resize() {
    // A single white texel in an 8 by 8 black field. Its share of the average is 1 in 64.
    let mut image = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
    image.put_pixel(3, 3, Rgba([255, 255, 255, 255]));
    let data = decode(&resize(&png("dot", &image), 1, 1, TextureUse::Data).expect("resizes"));
    // 255 / 64 is 3.98, which rounds to 4.
    assert_eq!(data.get_pixel(0, 0)[0], 4);
    let colour = decode(&resize(&png("dot", &image), 1, 1, TextureUse::Colour).expect("resizes"));
    // One part in 64 of linear light is about 33.5 in sRGB, so 34. The one-step tolerance covers the table.
    assert!(colour.get_pixel(0, 0)[0].abs_diff(34) <= 1, "colour was {}", colour.get_pixel(0, 0)[0]);
}

#[test]
fn an_unchanged_size_keeps_every_texel() {
    let image = gradient_small();
    let out = decode(&resize(&png("same", &image), 4, 4, TextureUse::Colour).expect("resizes"));
    assert_eq!(out, image);
}

#[test]
fn the_same_gradient_gives_the_same_bytes_every_run() {
    let source = png("ramp", &gradient());
    let first = resize(&source, 1024, 1024, TextureUse::Colour).expect("resizes");
    let second = resize(&source, 1024, 1024, TextureUse::Colour).expect("resizes");
    assert_eq!(first.bytes.as_slice(), second.bytes.as_slice());
    assert_eq!(decode(&first).dimensions(), (1024, 1024));
}

#[test]
fn downscale_keeps_an_image_that_already_fits() {
    let image = png("wide", &RgbaImage::from_pixel(1024, 512, Rgba([9, 9, 9, 255])));
    assert!(downscale_to(&image, 2048, TextureUse::Colour).expect("reads").is_none());
}

#[test]
fn downscale_keeps_the_aspect_when_the_longer_side_is_over() {
    let image = png("long", &RgbaImage::from_pixel(3000, 1000, Rgba([9, 9, 9, 255])));
    let out = downscale_to(&image, 1024, TextureUse::Colour).expect("reads").expect("is over the budget");
    // 1000 * 1024 / 3000 is 341.3, which rounds to 341.
    assert_eq!(decode(&out).dimensions(), (1024, 341));
}

#[test]
fn a_header_declaring_an_oversized_image_is_a_limit_and_is_not_decoded() {
    // A 1 by 1 PNG whose header is rewritten to 70,000 by 70,000, with the chunk's CRC recomputed so the header is
    // the only thing wrong. The pixel data no longer matches, so decoding would fail: the size check has to come first.
    let mut bytes = png("tiny", &RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 255]))).bytes.as_slice().to_vec();
    bytes[16..20].copy_from_slice(&70_000_u32.to_be_bytes());
    bytes[20..24].copy_from_slice(&70_000_u32.to_be_bytes());
    let crc = crc32(&bytes[12..29]);
    bytes[29..33].copy_from_slice(&crc.to_be_bytes());
    let image = EncodedImage::new("huge", ImageMime::Png, Blob::new(bytes));
    assert!(matches!(resize(&image, 16, 16, TextureUse::Colour), Err(IoError::Limit { .. })));
    assert!(matches!(downscale_to(&image, 16, TextureUse::Colour), Err(IoError::Limit { .. })));
}

#[test]
fn a_truncated_png_is_malformed() {
    let full = png("grad", &gradient_small());
    let cut = full.bytes.as_slice()[..full.bytes.as_slice().len() / 2].to_vec();
    let truncated = EncodedImage::new("grad", ImageMime::Png, Blob::new(cut));
    assert!(matches!(resize(&truncated, 2, 2, TextureUse::Colour), Err(IoError::Malformed { .. })));
    let header_only = EncodedImage::new("grad", ImageMime::Png, Blob::new(full.bytes.as_slice()[..10].to_vec()));
    assert!(matches!(resize(&header_only, 2, 2, TextureUse::Colour), Err(IoError::Malformed { .. })));
}

#[test]
fn an_empty_target_is_unsupported() {
    let image = png("one", &checkerboard());
    assert!(matches!(resize(&image, 0, 1, TextureUse::Colour), Err(IoError::Unsupported { .. })));
}

/// The byte offset of the chunk of type `kind`, counted from its length field.
fn chunk_at(bytes: &[u8], kind: [u8; 4]) -> usize {
    bytes.windows(4).position(|window| window == kind).expect("the chunk is present") - 4
}

#[test]
fn a_png_cut_right_after_its_idat_header_is_malformed_even_when_it_fits() {
    let full = png("fits", &gradient_small());
    let bytes = full.bytes.as_slice();
    // Keep the IDAT length and type, then stop: the data the length promises is not there.
    let cut = EncodedImage::new("fits", ImageMime::Png, Blob::new(bytes[..chunk_at(bytes, *b"IDAT") + 8].to_vec()));
    // The 4 by 4 image fits within 8, so nothing would be resampled; the payload must still be checked.
    assert!(matches!(downscale_to(&cut, 8, TextureUse::Colour), Err(IoError::Malformed { .. })));
}

#[test]
fn a_png_without_its_iend_crc_or_its_iend_chunk_is_malformed() {
    let full = png("end", &gradient_small());
    let bytes = full.bytes.as_slice();
    let without_crc = bytes[..bytes.len() - 4].to_vec();
    let without_chunk = bytes[..chunk_at(bytes, *b"IEND")].to_vec();
    for cut in [without_crc, without_chunk] {
        let image = EncodedImage::new("end", ImageMime::Png, Blob::new(cut));
        assert!(matches!(downscale_to(&image, 8, TextureUse::Colour), Err(IoError::Malformed { .. })));
        assert!(matches!(resize(&image, 2, 2, TextureUse::Colour), Err(IoError::Malformed { .. })));
    }
}

#[test]
fn a_corrupt_chunk_fails_its_crc() {
    let full = png("crc", &gradient_small());
    let mut bytes = full.bytes.as_slice().to_vec();
    // One byte inside the image data changes, so the chunk's CRC no longer matches.
    let inside = chunk_at(&bytes, *b"IDAT") + 8 + 2;
    bytes[inside] ^= 0x40;
    let image = EncodedImage::new("crc", ImageMime::Png, Blob::new(bytes));
    assert!(matches!(resize(&image, 2, 2, TextureUse::Colour), Err(IoError::Malformed { .. })));
}

/// FNV-1a over `bytes`: a fixed hash, so the expected values in the test below are constants, not computed by it.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The decoded pixels of three gradient resizes, hashed. The arithmetic is table-driven and uses no maths-library call,
/// so these hold on every target; a change to the arithmetic changes them and must be a deliberate change.
const COLOUR_1024_HASH: u64 = 8_127_134_797_264_960_293;
const DATA_1024_HASH: u64 = 3_852_121_167_756_213_029;
const COLOUR_1000X750_HASH: u64 = 4_407_125_968_514_470_469;

#[test]
fn the_gradient_resizes_match_their_fixed_hashes_on_every_target() {
    let source = png("ramp", &gradient());
    let colour = decode(&resize(&source, 1024, 1024, TextureUse::Colour).expect("resizes"));
    let data = decode(&resize(&source, 1024, 1024, TextureUse::Data).expect("resizes"));
    let odd = decode(&resize(&source, 1000, 750, TextureUse::Colour).expect("resizes"));
    assert_eq!(fnv1a(colour.as_raw()), COLOUR_1024_HASH, "colour 1024x1024");
    assert_eq!(fnv1a(data.as_raw()), DATA_1024_HASH, "data 1024x1024");
    assert_eq!(fnv1a(odd.as_raw()), COLOUR_1000X750_HASH, "colour 1000x750");
}

/// A 4 by 4 gradient, for tests that only need some texels.
fn gradient_small() -> RgbaImage {
    RgbaImage::from_fn(4, 4, |x, y| Rgba([(x * 60) as u8, (y * 60) as u8, 128, 255]))
}

/// The CRC-32 a PNG chunk carries (ISO 3309, reflected polynomial), which the test uses to rewrite a header.
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
