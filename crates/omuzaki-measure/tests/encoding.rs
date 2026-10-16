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

/// An ICC profile of the given colour: RGB and XYZ, the three primaries, a D50 white and the sRGB curve (as a parametric
/// curve) on each channel. Its description is left out, since the decision reads the colour data, not the name.
#[allow(clippy::cast_possible_truncation, reason = "fixed-point numbers of the profile are small")]
fn icc_profile(red: [f64; 3], green: [f64; 3], blue: [f64; 3]) -> Vec<u8> {
    let s15 = |value: f64| ((value * 65_536.0).round() as i32).to_be_bytes();
    let xyz = |value: [f64; 3]| [b"XYZ \0\0\0\0".to_vec(), value.iter().flat_map(|&v| s15(v)).collect()].concat();
    let curve = [
        b"para\0\0\0\0\0\x03\0\0".to_vec(),
        [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045].iter().flat_map(|&v| s15(v)).collect(),
    ]
    .concat();
    let tags = [
        (*b"rXYZ", xyz(red)),
        (*b"gXYZ", xyz(green)),
        (*b"bXYZ", xyz(blue)),
        (*b"wtpt", xyz([0.9642, 1.0, 0.8249])),
        (*b"rTRC", curve.clone()),
        (*b"gTRC", curve.clone()),
        (*b"bTRC", curve),
    ];
    let mut out = vec![0; 128];
    out[16..20].copy_from_slice(b"RGB ");
    out[20..24].copy_from_slice(b"XYZ ");
    out.extend(u32::try_from(tags.len()).unwrap().to_be_bytes());
    let mut offset = 132 + 12 * tags.len();
    for (signature, data) in &tags {
        out.extend(signature);
        out.extend(u32::try_from(offset).unwrap().to_be_bytes());
        out.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
        offset += data.len();
    }
    for (_, data) in &tags {
        out.extend(data);
    }
    let length = u32::try_from(out.len()).unwrap().to_be_bytes();
    out[..4].copy_from_slice(&length);
    out
}

/// A JPEG with `profile` embedded in an APP2 segment right after its start, the way a camera or an editor writes it.
fn with_icc_jpeg(jpeg: &[u8], profile: &[u8]) -> Vec<u8> {
    let mut app2 = b"ICC_PROFILE\0\x01\x01".to_vec();
    app2.extend(profile);
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, 0xE2]);
    out.extend(u16::try_from(app2.len() + 2).unwrap().to_be_bytes());
    out.extend(app2);
    out.extend(&jpeg[2..]);
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

#[test]
fn a_jpeg_with_the_srgb_profile_is_read_as_srgb() {
    let srgb = icc_profile([0.4361, 0.2225, 0.0139], [0.3851, 0.7169, 0.0971], [0.1431, 0.0606, 0.7141]);
    let jpeg = with_icc_jpeg(include_bytes!("fixtures/baseline_444.jpg"), &srgb);
    assert!(decode(&jpeg, ContentHash::of(&jpeg), FIELD).is_ok());
}

#[test]
fn a_jpeg_with_a_display_p3_profile_is_refused_naming_the_profile() {
    let p3 = icc_profile([0.5151, 0.2412, -0.0011], [0.2919, 0.6922, 0.0419], [0.1572, 0.0666, 0.7841]);
    let jpeg = with_icc_jpeg(include_bytes!("fixtures/baseline_444.jpg"), &p3);
    assert!(refusal(&jpeg).contains("an embedded colour profile that is not sRGB"));
}

#[test]
fn a_jpeg_whose_declared_profile_would_exceed_the_budget_is_refused_before_it_is_copied() {
    // 510 APP2 chunks of 65 519 profile bytes each: about 33 MB declared. Two copies of it are over the 64 MiB budget,
    // so the refusal comes from the declared length, before `icc_profile` copies any of it.
    let fixture = include_bytes!("fixtures/baseline_444.jpg");
    let mut jpeg = fixture[..2].to_vec();
    for _ in 0..510 {
        jpeg.extend([0xFF, 0xE2]);
        jpeg.extend(u16::try_from(2 + 14 + 65_519).unwrap().to_be_bytes());
        jpeg.extend(b"ICC_PROFILE\0\x01\x01");
        jpeg.extend(vec![0_u8; 65_519]);
    }
    jpeg.extend(&fixture[2..]);
    let message = refusal(&jpeg);
    assert!(message.starts_with("The image is 96 × 64"), "{message}");
}

#[test]
fn a_jpeg_whose_profile_chunks_disagree_is_refused_not_read_as_untagged() {
    // A Display P3 profile in one APP2 chunk that says it is chunk 2 of 1. The decoder cannot put the profile together
    // and yields none; the file did declare one, so it is refused, where reading it as untagged would accept P3 samples
    // as sRGB.
    let p3 = icc_profile([0.5151, 0.2412, -0.0011], [0.2919, 0.6922, 0.0419], [0.1572, 0.0666, 0.7841]);
    let mut jpeg = with_icc_jpeg(include_bytes!("fixtures/baseline_444.jpg"), &p3);
    let tag = jpeg.windows(14).position(|w| w == b"ICC_PROFILE\0\x01\x01").unwrap();
    jpeg[tag + 12] = 2;
    let message = refusal(&jpeg);
    assert!(message.contains("embedded colour profile is incomplete"), "{message}");
}

#[test]
fn a_png_cicp_chunk_outranks_every_other_colour_chunk() {
    let srgb_cicp = chunk(*b"cICP", &[1, 13, 0, 1]);
    let display_p3 = chunk(*b"cICP", &[12, 13, 0, 1]);
    let bt2020_pq = chunk(*b"cICP", &[9, 16, 0, 1]);
    let srgb_chunk = chunk(*b"sRGB", &[0]);
    let linear = chunk(*b"gAMA", &100_000u32.to_be_bytes());
    // Display P3 and BT.2020 PQ are refused, naming the code points.
    let p3 = with_chunks(&png_2x2(), std::slice::from_ref(&display_p3));
    assert_eq!(
        refusal(&p3),
        "The image is not sRGB (a cICP colour declaration (primaries 12, transfer 13, matrix 0)). Convert it to sRGB and try again."
    );
    assert!(refusal(&with_chunks(&png_2x2(), &[bt2020_pq])).contains("primaries 9, transfer 16"));
    // cICP wins over a lower-precedence sRGB chunk that would have accepted the file...
    assert!(refusal(&with_chunks(&png_2x2(), &[display_p3, srgb_chunk])).contains("cICP"));
    // ...and an sRGB cICP wins over a linear gamma that would have refused it.
    let accepted = with_chunks(&png_2x2(), &[srgb_cicp.clone(), linear]);
    assert!(decode(&accepted, ContentHash::of(&accepted), FIELD).is_ok());
    // Over an embedded Display P3 profile, too.
    let profile = icc_profile([0.5151, 0.2412, -0.0011], [0.2919, 0.6922, 0.0419], [0.1572, 0.0666, 0.7841]);
    let mut tagged = Vec::new();
    let mut encoder = PngEncoder::new(&mut tagged);
    encoder.set_icc_profile(profile).unwrap();
    let pixels = [9_u8, 8, 7, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    encoder.write_image(&pixels, 2, 2, ExtendedColorType::Rgb8).unwrap();
    assert!(refusal(&tagged).contains("embedded colour profile"), "the profile alone is refused");
    let png = with_chunks(&tagged, &[srgb_cicp]);
    assert!(decode(&png, ContentHash::of(&png), FIELD).is_ok());
}

#[test]
fn the_first_cicp_chunk_is_the_one_that_counts() {
    // The decoder keeps the first `cICP` and treats a repeat as a bad ancillary chunk, so Display P3 first and sRGB second
    // is Display P3, and the reverse is sRGB.
    let display_p3 = chunk(*b"cICP", &[12, 13, 0, 1]);
    let srgb = chunk(*b"cICP", &[1, 13, 0, 1]);
    let p3_first = with_chunks(&png_2x2(), &[display_p3.clone(), srgb.clone()]);
    assert!(refusal(&p3_first).contains("primaries 12"));
    let srgb_first = with_chunks(&png_2x2(), &[srgb, display_p3]);
    assert!(decode(&srgb_first, ContentHash::of(&srgb_first), FIELD).is_ok());
}

#[test]
fn an_srgb_cicp_beside_a_large_profile_is_not_refused_for_the_profile() {
    // A 40 MiB profile of zeros compresses to about 40 KiB. It is not read when `cICP` decides the colour, so the image is
    // accepted, where a profile that large with no `cICP` is refused.
    let mut tagged = Vec::new();
    let mut encoder = PngEncoder::new(&mut tagged);
    encoder.set_icc_profile(vec![0_u8; 40 << 20]).unwrap();
    let pixels = [9_u8, 8, 7, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    encoder.write_image(&pixels, 2, 2, ExtendedColorType::Rgb8).unwrap();
    assert!(refusal(&tagged).contains("inflates to more than 16 MiB"));
    let png = with_chunks(&tagged, &[chunk(*b"cICP", &[1, 13, 0, 1])]);
    assert!(decode(&png, ContentHash::of(&png), FIELD).is_ok());
}

#[test]
fn a_large_png_with_an_srgb_cicp_still_decodes() {
    // The decoder of a PNG whose `cICP` decides the colour is built under the profile bound; that must not touch the pixel
    // decode of a big image (1500 x 1500 at 16-bit RGBA is 18 MB of output, over that bound).
    let samples = vec![0_u8; 1500 * 1500 * 8];
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(&samples, 1500, 1500, ExtendedColorType::Rgba16).unwrap();
    let png = with_chunks(&out, &[chunk(*b"cICP", &[1, 13, 0, 1])]);
    let decoded = decode(&png, ContentHash::of(&png), FIELD).expect("a big sRGB PNG decodes");
    assert_eq!((decoded.width, decoded.height), (1500, 1500));
}

/// `chunk(...)` with its CRC made wrong, the way a corrupt file has it.
fn with_bad_crc(mut chunk: Vec<u8>) -> Vec<u8> {
    let last = chunk.len() - 1;
    chunk[last] ^= 0xFF;
    chunk
}

/// A 2x2 RGB PNG that embeds a Display P3 profile.
fn display_p3_png() -> Vec<u8> {
    let profile = icc_profile([0.5151, 0.2412, -0.0011], [0.2919, 0.6922, 0.0419], [0.1572, 0.0666, 0.7841]);
    let mut out = Vec::new();
    let mut encoder = PngEncoder::new(&mut out);
    encoder.set_icc_profile(profile).unwrap();
    let pixels = [9_u8, 8, 7, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    encoder.write_image(&pixels, 2, 2, ExtendedColorType::Rgb8).unwrap();
    out
}

#[test]
fn a_colour_chunk_with_a_bad_crc_is_ignored_as_the_decoder_ignores_it() {
    // The decoder drops an ancillary chunk whose CRC is wrong, so an sRGB `cICP` with a corrupt CRC beside a Display P3
    // profile leaves the profile as the colour: refused. The same chunk with its CRC right is accepted.
    let srgb_cicp = chunk(*b"cICP", &[1, 13, 0, 1]);
    let corrupt = with_chunks(&display_p3_png(), &[with_bad_crc(srgb_cicp.clone())]);
    assert!(refusal(&corrupt).contains("embedded colour profile"), "the corrupt cICP must not decide");
    let intact = with_chunks(&display_p3_png(), &[srgb_cicp]);
    assert!(decode(&intact, ContentHash::of(&intact), FIELD).is_ok());
    // An `sRGB` chunk with a bad CRC does not override a linear gamma either.
    let linear = chunk(*b"gAMA", &100_000u32.to_be_bytes());
    let png = with_chunks(&png_2x2(), &[linear, with_bad_crc(chunk(*b"sRGB", &[0]))]);
    assert!(refusal(&png).contains("a gamma of 1.00"));
}

#[test]
fn the_first_gamma_and_the_first_primaries_are_the_ones_that_count() {
    let linear = chunk(*b"gAMA", &100_000u32.to_be_bytes());
    let srgb_gamma = chunk(*b"gAMA", &45_455u32.to_be_bytes());
    let linear_first = with_chunks(&png_2x2(), &[linear.clone(), srgb_gamma.clone()]);
    assert!(refusal(&linear_first).contains("a gamma of 1.00"));
    let srgb_first = with_chunks(&png_2x2(), &[srgb_gamma, linear]);
    assert!(decode(&srgb_first, ContentHash::of(&srgb_first), FIELD).is_ok());
    let srgb: Vec<u8> = SRGB_PRIMARIES.iter().flat_map(|value| value.to_be_bytes()).collect();
    let mut wide = SRGB_PRIMARIES;
    wide[2] = 70_000;
    wide[3] = 30_000;
    let wide: Vec<u8> = wide.iter().flat_map(|value| value.to_be_bytes()).collect();
    let wide_first = with_chunks(&png_2x2(), &[chunk(*b"cHRM", &wide), chunk(*b"cHRM", &srgb)]);
    assert!(refusal(&wide_first).contains("primaries other than sRGB's"));
    let srgb_first = with_chunks(&png_2x2(), &[chunk(*b"cHRM", &srgb), chunk(*b"cHRM", &wide)]);
    assert!(decode(&srgb_first, ContentHash::of(&srgb_first), FIELD).is_ok());
}

#[test]
fn a_malformed_srgb_chunk_declares_nothing_so_a_linear_gamma_beside_it_is_refused() {
    // The PNG specification gives `sRGB` one byte, a rendering intent from 0 to 3, and the decoder discards any other
    // payload. So a two-byte chunk, or an intent of 4, leaves the linear `gAMA` as the colour.
    let linear = chunk(*b"gAMA", &100_000u32.to_be_bytes());
    for payload in [&[0_u8, 0][..], &[4], &[]] {
        let png = with_chunks(&png_2x2(), &[linear.clone(), chunk(*b"sRGB", payload)]);
        assert!(refusal(&png).contains("a gamma of 1.00"), "an sRGB payload of {payload:?} must declare nothing");
    }
    // Each defined intent still counts.
    for intent in 0..=3_u8 {
        let png = with_chunks(&png_2x2(), &[linear.clone(), chunk(*b"sRGB", &[intent])]);
        assert!(decode(&png, ContentHash::of(&png), FIELD).is_ok(), "intent {intent}");
    }
}

#[test]
fn a_cicp_after_the_palette_is_dropped_by_the_decoder_so_it_decides_nothing() {
    // `iCCP(Display P3), PLTE, cICP(sRGB), IDAT`: the decoder discards a `cICP` that follows `PLTE`, so the profile is the
    // colour and the image is refused. Colour is read from the decoder's own metadata, never from a separate chunk walk.
    let png = with_chunks(&display_p3_png(), &[chunk(*b"PLTE", &[1, 2, 3]), chunk(*b"cICP", &[1, 13, 0, 1])]);
    assert!(refusal(&png).contains("embedded colour profile"));
    // Before the palette the same chunk decides, and the file is accepted.
    let before = with_chunks(&display_p3_png(), &[chunk(*b"cICP", &[1, 13, 0, 1]), chunk(*b"PLTE", &[1, 2, 3])]);
    assert!(decode(&before, ContentHash::of(&before), FIELD).is_ok());
}

/// A PNG with `PLTE`, then an `iCCP` whose profile inflates to 40 MiB (past the 16 MiB bound), then its data.
fn palette_then_large_profile() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = PngEncoder::new(&mut out);
    encoder.set_icc_profile(vec![0_u8; 40 << 20]).unwrap();
    encoder.write_image(&[9_u8, 8, 7, 1, 2, 3, 4, 5, 6, 7, 8, 9], 2, 2, ExtendedColorType::Rgb8).unwrap();
    let iccp = out.windows(4).position(|window| window == b"iCCP").unwrap() - 4;
    let mut late = out[..iccp].to_vec();
    late.extend(chunk(*b"PLTE", &[1, 2, 3]));
    late.extend(&out[iccp..]);
    late
}

#[test]
fn a_profile_the_decoder_drops_for_any_reason_is_refused_never_read_as_untagged() {
    // `PLTE, iCCP(large non-sRGB), IDAT`: the profile is past the decoder's bound, so it is dropped without an error and
    // the file would read as untagged. The scan saw the chunk, so the file is refused as present but unreadable.
    assert!(refusal(&palette_then_large_profile()).contains("present but unreadable"));
    // Only a `cICP` the decoder itself accepted decides the colour instead: here it follows the palette, which the
    // decoder discards, so the profile still makes the file unreadable.
    let late_cicp = {
        let png = palette_then_large_profile();
        let idat = png.windows(4).position(|window| window == b"IDAT").unwrap() - 4;
        let mut out = png[..idat].to_vec();
        out.extend(chunk(*b"cICP", &[1, 13, 0, 1]));
        out.extend(&png[idat..]);
        out
    };
    assert!(refusal(&late_cicp).contains("present but unreadable"));
}
