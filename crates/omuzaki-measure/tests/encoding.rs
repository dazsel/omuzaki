//! The colour and format refusals of a decode: a measure reads sRGB PNG and JPEG, refuses any other colour and any other
//! format by name, and reads an untagged image as sRGB. Every image is made here, from code or from the fixtures in
//! `tests/fixtures`, which are small JPEGs of the synthetic pattern made by PIL.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use ::image::codecs::png::PngEncoder;
use ::image::{ExtendedColorType, ImageEncoder};
use omuzaki_measure::image::decode;
use omuzaki_ops_api::ErrorCode;
use omuzaki_store_api::ContentHash;

const FIELD: &str = "image";

const CMYK_JPEG: &[u8] = include_bytes!("fixtures/cmyk.jpg");
const SRGB_PRIMARIES: [u32; 8] = [31_270, 32_900, 64_000, 33_000, 30_000, 60_000, 15_000, 6_000];

fn png_2x2() -> Vec<u8> {
    let mut out = Vec::new();
    let pixels = [9_u8, 8, 7, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    PngEncoder::new(&mut out).write_image(&pixels, 2, 2, ExtendedColorType::Rgb8).unwrap();
    out
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

/// One PNG chunk: its length, its kind, its data and its CRC.
fn chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    out.extend(kind);
    out.extend(data);
    out.extend(crc32(&[kind.as_slice(), data].concat()).to_be_bytes());
    out
}

/// A PNG with `chunks` written before its image data, the way an encoder writes colour metadata.
fn with_chunks(png: &[u8], chunks: &[Vec<u8>]) -> Vec<u8> {
    let idat = png.windows(4).position(|window| window == b"IDAT").unwrap() - 4;
    let mut out = png[..idat].to_vec();
    for chunk in chunks {
        out.extend(chunk);
    }
    out.extend(&png[idat..]);
    out
}

/// Decodes `bytes` and returns the refusal's text, expecting a refusal with the `Invalid` code.
fn refusal(bytes: &[u8]) -> String {
    let error = decode(bytes, ContentHash::of(bytes), FIELD).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid, "{}", error.message);
    assert_eq!(error.field.as_deref(), Some(FIELD));
    error.message
}

#[test]
fn an_untagged_png_is_read_as_srgb() {
    assert!(decode(&png_2x2(), ContentHash::of(&png_2x2()), FIELD).is_ok());
}

#[test]
fn a_png_with_an_srgb_chunk_is_read_as_srgb_whatever_its_gamma() {
    // The PNG specification has the sRGB chunk override gAMA and cHRM, so a linear gamma beside it is not refused.
    let png = with_chunks(&png_2x2(), &[chunk(*b"gAMA", &100_000u32.to_be_bytes()), chunk(*b"sRGB", &[0])]);
    assert!(decode(&png, ContentHash::of(&png), FIELD).is_ok());
}

#[test]
fn a_png_with_the_srgb_gamma_and_primaries_is_read_as_srgb() {
    let gamma = chunk(*b"gAMA", &45_455u32.to_be_bytes());
    let primaries: Vec<u8> = SRGB_PRIMARIES.iter().flat_map(|value| value.to_be_bytes()).collect();
    let png = with_chunks(&png_2x2(), &[gamma, chunk(*b"cHRM", &primaries)]);
    assert!(decode(&png, ContentHash::of(&png), FIELD).is_ok());
}

#[test]
fn a_linear_gamma_png_is_refused_naming_the_gamma() {
    let png = with_chunks(&png_2x2(), &[chunk(*b"gAMA", &100_000u32.to_be_bytes())]);
    let message = refusal(&png);
    assert_eq!(message, "The image is not sRGB (a gamma of 1.00). Convert it to sRGB and try again.");
}

#[test]
fn a_png_with_other_primaries_is_refused() {
    // Red moved to (0.7, 0.3): the Rec. 2020 corner, not sRGB's (0.64, 0.33).
    let mut primaries = SRGB_PRIMARIES;
    primaries[2] = 70_000;
    primaries[3] = 30_000;
    let bytes: Vec<u8> = primaries.iter().flat_map(|value| value.to_be_bytes()).collect();
    let png = with_chunks(&png_2x2(), &[chunk(*b"cHRM", &bytes)]);
    assert!(refusal(&png).contains("primaries other than sRGB's"));
}

#[test]
fn a_cmyk_jpeg_is_refused_naming_cmyk() {
    assert!(refusal(CMYK_JPEG).contains("CMYK colour"));
}

#[test]
fn a_tiff_is_refused_by_name_before_it_is_read() {
    let tiff = [b"II*\0\x08\0\0\0".as_slice(), &[0; 16]].concat();
    assert!(refusal(&tiff).contains("it is a TIFF file, and a measure reads only PNG and JPEG"));
}

#[test]
fn a_bmp_is_refused_by_name_before_it_is_read() {
    let bmp = [b"BM".as_slice(), &[0; 52]].concat();
    assert!(refusal(&bmp).contains("it is a BMP file, and a measure reads only PNG and JPEG"));
}
