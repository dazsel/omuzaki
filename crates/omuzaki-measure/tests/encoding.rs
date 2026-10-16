//! Colour and format of a decode: an image tagged with an ICC profile or a PNG `cICP` chunk is converted to sRGB by
//! `moxcms`, an untagged one is sRGB and stays as it is, a profile the engine cannot read is refused, and any format but
//! PNG and JPEG is refused by name. Every image is made here, from code or from the fixtures in `tests/fixtures`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use ::image::codecs::png::PngEncoder;
use ::image::{ExtendedColorType, ImageEncoder};
use omuzaki_measure::image::decode;
use omuzaki_ops_api::ErrorCode;
use omuzaki_store_api::ContentHash;

const FIELD: &str = "image";

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

/// A Display P3 profile as `moxcms` writes it. The expected pixels below do not come from it: they are computed from
/// the published matrices in `p3_to_srgb`.
fn display_p3_profile() -> Vec<u8> {
    moxcms::ColorProfile::new_display_p3().encode().unwrap()
}

/// A 2 x 2 PNG of one colour, written with `profile` embedded when there is one.
fn flat_png(rgb: [u8; 3], profile: Option<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = PngEncoder::new(&mut out);
    if let Some(profile) = profile {
        encoder.set_icc_profile(profile).unwrap();
    }
    encoder.write_image(&rgb.repeat(4), 2, 2, ExtendedColorType::Rgb8).unwrap();
    out
}

fn first_pixel(bytes: &[u8]) -> [u8; 4] {
    let image = decode(bytes, ContentHash::of(bytes), FIELD).unwrap();
    [image.pixels[0], image.pixels[1], image.pixels[2], image.pixels[3]]
}

/// Display P3 (D65, sRGB transfer curve) to sRGB, from the published matrices (Display P3 to XYZ, XYZ to sRGB, both
/// D65), in `f64` and unrounded, so the test does not use the code it checks. Channels are clamped to the sRGB gamut.
fn p3_to_srgb(rgb: [u8; 3]) -> [f64; 3] {
    let decode = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let encode = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        255.0 * if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    };
    let [red, green, blue] = rgb.map(decode);
    let cie_x = 0.486_570_9 * red + 0.265_667_7 * green + 0.198_217_3 * blue;
    let cie_y = 0.228_974_6 * red + 0.691_738_5 * green + 0.079_286_9 * blue;
    let cie_z = 0.045_113_4 * green + 1.043_944_4 * blue;
    [
        encode(3.240_454_2 * cie_x - 1.537_138_5 * cie_y - 0.498_531_4 * cie_z),
        encode(-0.969_266_0 * cie_x + 1.876_010_8 * cie_y + 0.041_556_0 * cie_z),
        encode(0.055_643_4 * cie_x - 0.204_025_9 * cie_y + 1.057_225_2 * cie_z),
    ]
}

/// The tolerance on a converted channel, in 8-bit steps. Rounding the result to a byte is half a step; the profile's
/// colorants are stored in 16.16 fixed point and the colour engine interpolates its curves from a 4096-entry table, which
/// together stay under another step. Two steps is the sum, with a little room, and far below a visible difference
/// (about 0.4 of an L* unit); a missing conversion is off by 5 to 40 steps for these colours (the test checks it moved).
const TOLERANCE: f64 = 2.0;

fn assert_converted(got: [u8; 4], source: [u8; 3]) {
    let expected = p3_to_srgb(source);
    for channel in 0..3 {
        let off = (f64::from(got[channel]) - expected[channel]).abs();
        assert!(
            off <= TOLERANCE,
            "channel {channel}: got {}, expected {:.2} (source {source:?})",
            got[channel],
            expected[channel]
        );
    }
    assert_eq!(got[3], 255);
}

/// Colours inside both gamuts, so clamping plays no part, and far enough from the sRGB values that no conversion would
/// show as a clear error.
const P3_COLOURS: [[u8; 3]; 3] = [[200, 120, 60], [40, 160, 90], [90, 90, 200]];

#[test]
fn a_display_p3_profile_converts_to_srgb() {
    for source in P3_COLOURS {
        let got = first_pixel(&flat_png(source, Some(display_p3_profile())));
        assert_converted(got, source);
        let moved = (0..3).map(|c| (i32::from(got[c]) - i32::from(source[c])).abs()).max().unwrap();
        assert!(moved >= 5, "{source:?} barely moved ({got:?}): the profile was ignored");
    }
}

#[test]
fn a_display_p3_profile_in_a_jpeg_converts_too() {
    let jpeg = include_bytes!("fixtures/baseline_444.jpg");
    let plain = decode(jpeg, ContentHash::of(jpeg), FIELD).unwrap();
    let tagged = with_icc_jpeg(jpeg, &display_p3_profile());
    let converted = decode(&tagged, ContentHash::of(&tagged), FIELD).unwrap();
    assert_eq!(plain.width, converted.width);
    assert_ne!(plain.pixels, converted.pixels, "the profile changed nothing");
    // Pixel (0, 0): the JPEG's own decode, converted by the same maths.
    assert_converted(
        [converted.pixels[0], converted.pixels[1], converted.pixels[2], 255],
        [plain.pixels[0], plain.pixels[1], plain.pixels[2]],
    );
}

#[test]
fn a_cicp_tagged_png_converts_to_srgb() {
    // cICP primaries 12 (Display P3, D65), transfer 13 (sRGB curve), matrix 0, full range.
    for source in P3_COLOURS {
        let png = with_chunks(&flat_png(source, None), &[chunk(*b"cICP", &[12, 13, 0, 1])]);
        assert_converted(first_pixel(&png), source);
    }
}

#[test]
fn a_cicp_outranks_an_embedded_profile() {
    // The profile says Display P3, the cICP says sRGB: the cICP wins, so nothing moves.
    let png = with_chunks(&flat_png([200, 120, 60], Some(display_p3_profile())), &[chunk(*b"cICP", &[1, 13, 0, 1])]);
    assert_eq!(first_pixel(&png), [200, 120, 60, 255]);
}

#[test]
fn an_untagged_image_and_an_srgb_cicp_stay_unchanged() {
    assert_eq!(first_pixel(&flat_png([200, 120, 60], None)), [200, 120, 60, 255]);
    let srgb = with_chunks(&flat_png([200, 120, 60], None), &[chunk(*b"cICP", &[1, 13, 0, 1])]);
    assert_eq!(first_pixel(&srgb), [200, 120, 60, 255]);
}

#[test]
fn a_profile_the_colour_engine_cannot_read_is_refused() {
    let png = flat_png([200, 120, 60], Some(vec![7; 300]));
    let message = decode(&png, ContentHash::of(&png), FIELD).unwrap_err();
    assert_eq!(message.code, ErrorCode::Invalid);
    assert!(message.message.starts_with("The image's colour could not be converted to sRGB"), "{}", message.message);
}

#[test]
fn a_tiff_is_refused_by_name_before_it_is_read() {
    let tiff = [b"II*\0\x08\0\0\0".as_slice(), &[0; 16]].concat();
    let error = decode(&tiff, ContentHash::of(&tiff), FIELD).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.contains("it is a TIFF file, and a measure reads only PNG and JPEG"), "{}", error.message);
}

#[test]
fn a_bmp_is_refused_by_name_before_it_is_read() {
    let bmp = [b"BM".as_slice(), &[0; 52]].concat();
    let error = decode(&bmp, ContentHash::of(&bmp), FIELD).unwrap_err();
    assert!(error.message.contains("it is a BMP file, and a measure reads only PNG and JPEG"), "{}", error.message);
}

/// The sRGB encoding of a linear value in [0, 1], in 8-bit steps, unrounded.
fn srgb_encode(linear: f64) -> f64 {
    255.0 * if linear <= 0.003_130_8 { 12.92 * linear } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 }
}

/// A `cHRM` chunk from (white, red, green, blue) x and y, in units of 1e-5 as the chunk stores them.
fn chrm(values: [(u32, u32); 4]) -> Vec<u8> {
    let data: Vec<u8> = values.iter().flat_map(|&(x, y)| [x, y]).flat_map(u32::to_be_bytes).collect();
    chunk(*b"cHRM", &data)
}

#[test]
fn a_linear_gamma_png_is_converted_not_read_as_srgb() {
    // `gAMA` 1.0 says the stored values are linear light. Per the PNG specification that is its colour space, so 128 is
    // 0.502 linear, which sRGB stores as 188. Read as sRGB it would stay 128.
    let png = with_chunks(&flat_png([128, 64, 200], None), &[chunk(*b"gAMA", &100_000u32.to_be_bytes())]);
    let got = first_pixel(&png);
    for (channel, source) in [128_u8, 64, 200].into_iter().enumerate() {
        let expected = srgb_encode(f64::from(source) / 255.0);
        assert!(
            (f64::from(got[channel]) - expected).abs() <= TOLERANCE,
            "channel {channel}: {} vs {expected:.2}",
            got[channel]
        );
    }
    assert!(got[0] >= 186, "128 linear should be about 188 in sRGB, got {}", got[0]);
}

#[test]
fn the_srgb_equivalent_gamma_is_unchanged() {
    // 45 455 is 1 / 2.2 to the file's precision, and with no `cHRM` (or sRGB's own) there is nothing to convert.
    let gamma = chunk(*b"gAMA", &45_455u32.to_be_bytes());
    let png = with_chunks(&flat_png([128, 64, 200], None), std::slice::from_ref(&gamma));
    assert_eq!(first_pixel(&png), [128, 64, 200, 255]);
    let srgb = chrm([(31_270, 32_900), (64_000, 33_000), (30_000, 60_000), (15_000, 6_000)]);
    let with_chrm = with_chunks(&flat_png([128, 64, 200], None), &[gamma, srgb]);
    assert_eq!(first_pixel(&with_chrm), [128, 64, 200, 255]);
    // The `sRGB` chunk outranks a gamma that says otherwise.
    let chunk_wins = with_chunks(
        &flat_png([128, 64, 200], None),
        &[chunk(*b"gAMA", &100_000u32.to_be_bytes()), chunk(*b"sRGB", &[0])],
    );
    assert_eq!(first_pixel(&chunk_wins), [128, 64, 200, 255]);
}

#[test]
fn chromaticities_alone_define_the_primaries_and_the_srgb_curve_is_kept() {
    // Display P3's primaries and white point, no `gAMA`: the stored values are on sRGB's curve with P3's primaries.
    let p3 = chrm([(31_270, 32_900), (68_000, 32_000), (26_500, 69_000), (15_000, 6_000)]);
    for source in P3_COLOURS {
        let png = with_chunks(&flat_png(source, None), std::slice::from_ref(&p3));
        assert_converted(first_pixel(&png), source);
    }
}

#[test]
fn a_cmyk_jpeg_without_a_profile_is_refused() {
    let jpeg = include_bytes!("fixtures/cmyk.jpg");
    let error = decode(jpeg, ContentHash::of(jpeg), FIELD).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(
        error.message.contains("CMYK images aren't supported for measuring; save it as RGB (sRGB or Display P3) first"),
        "{}",
        error.message
    );
}

#[test]
fn a_cmyk_jpeg_with_a_profile_is_refused_the_same_way() {
    // The decoder converts the inks to RGB itself and offers no way to read them, so a CMYK profile cannot be applied.
    let tagged = with_icc_jpeg(include_bytes!("fixtures/cmyk.jpg"), &display_p3_profile());
    let error = decode(&tagged, ContentHash::of(&tagged), FIELD).unwrap_err();
    assert!(
        error.message.contains("CMYK images aren't supported for measuring; save it as RGB (sRGB or Display P3) first"),
        "{}",
        error.message
    );
}

#[test]
fn a_sixteen_bit_linear_sample_is_converted_before_it_is_quantised() {
    // 128 of 65 535 in linear light is 0.00195, which sRGB stores as about 6. Quantised to 8 bits first it would be 0.
    let samples: Vec<u8> = [128_u16; 12].iter().flat_map(|v| v.to_ne_bytes()).collect();
    let mut png = Vec::new();
    PngEncoder::new(&mut png).write_image(&samples, 2, 2, ExtendedColorType::Rgb16).unwrap();
    let png = with_chunks(&png, &[chunk(*b"gAMA", &100_000u32.to_be_bytes())]);
    let got = first_pixel(&png);
    let expected = srgb_encode(128.0 / 65_535.0);
    for (channel, &value) in got.iter().take(3).enumerate() {
        assert!((f64::from(value) - expected).abs() <= 1.0, "channel {channel}: {value} vs {expected:.2}");
    }
    assert!(got[0] >= 5, "got {}", got[0]);
}

#[test]
fn a_tagged_grayscale_image_converts_through_the_gray_profile() {
    // A gray profile with gamma 1.0 (linear): 128 is 0.502 linear, which sRGB stores as 188, on all three channels.
    let profile = moxcms::ColorProfile::new_gray_with_gamma(1.0).encode().unwrap();
    let mut png = Vec::new();
    let mut encoder = PngEncoder::new(&mut png);
    encoder.set_icc_profile(profile).unwrap();
    encoder.write_image(&[128_u8; 4], 2, 2, ExtendedColorType::L8).unwrap();
    let got = first_pixel(&png);
    let expected = srgb_encode(128.0 / 255.0);
    for (channel, &value) in got.iter().take(3).enumerate() {
        assert!((f64::from(value) - expected).abs() <= TOLERANCE, "channel {channel}: {value} vs {expected:.2}");
    }
    assert_eq!(got[3], 255);
}

#[test]
fn a_grayscale_profile_on_a_colour_image_is_refused() {
    let profile = moxcms::ColorProfile::new_gray_with_gamma(1.0).encode().unwrap();
    let png = flat_png([128, 64, 200], Some(profile));
    let error = decode(&png, ContentHash::of(&png), FIELD).unwrap_err();
    assert!(error.message.contains("grayscale colour profile on an image that is not grayscale"), "{}", error.message);
}

#[test]
fn a_profile_the_decoder_could_not_give_is_refused_never_read_as_untagged() {
    // A PNG whose `iCCP` will not inflate: the decoder drops it, and the file would read as untagged sRGB.
    let broken = with_chunks(&flat_png([200, 120, 60], None), &[chunk(*b"iCCP", b"p\0\0this is not zlib data")]);
    let error = decode(&broken, ContentHash::of(&broken), FIELD).unwrap_err();
    assert!(error.message.contains("present but cannot be read"), "{}", error.message);
    // A JPEG whose APP2 profile chunks disagree (sequence 2 of 1): the decoder yields none.
    let jpeg = include_bytes!("fixtures/baseline_444.jpg");
    let mut app2 = b"ICC_PROFILE\0\x02\x01".to_vec();
    app2.extend(display_p3_profile());
    let mut bad = jpeg[..2].to_vec();
    bad.extend([0xFF, 0xE2]);
    bad.extend(u16::try_from(app2.len() + 2).unwrap().to_be_bytes());
    bad.extend(app2);
    bad.extend(&jpeg[2..]);
    let error = decode(&bad, ContentHash::of(&bad), FIELD).unwrap_err();
    assert!(error.message.contains("present but cannot be read"), "{}", error.message);
    // And a `cICP` the decoder accepted decides the colour anyway, so a broken profile beside it is no reason to refuse.
    let with_cicp = with_chunks(
        &flat_png([200, 120, 60], None),
        &[chunk(*b"cICP", &[1, 13, 0, 1]), chunk(*b"iCCP", b"p\0\0this is not zlib data")],
    );
    assert_eq!(first_pixel(&with_cicp), [200, 120, 60, 255]);
}
