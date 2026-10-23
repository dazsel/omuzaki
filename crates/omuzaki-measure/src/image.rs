//! Reads a reference or render image for a measure (design 4.1): from the store by its hash, or from a file the grants
//! allow, decoded to straight sRGB RGBA8. Only PNG and JPEG are read. The size is limited by the decoder's own `Limits`
//! (8192 px a side, 64 MiB), which refuse an oversized header before any pixel is allocated. An image tagged with a colour
//! space (an embedded ICC profile, or a PNG `cICP` chunk) is converted to sRGB with `moxcms`, the colour engine the
//! `image` crate already uses; an untagged image is sRGB.

use std::io::{Cursor, Read};

use ::image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};
use moxcms::{
    Chromaticity, CicpColorPrimaries, CicpProfile, ColorPrimaries, ColorProfile, DataColorSpace, Layout,
    MatrixCoefficients, ToneReprCurve, TransferCharacteristics, TransformOptions, XyY,
};
use omuzaki_ops_api::{FsMode, OpCtx, OpError};
use omuzaki_store_api::{ContentHash, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::messages;

/// The longest side a measure reads, in pixels (design 4.1).
pub const MAX_SIDE: u32 = 8192;

/// The most a decoded image may take, in MiB, as the straight RGBA8 the measure keeps (design 4.1, #282): `width x
/// height x 4` bytes, checked once from the header, before anything is allocated.
pub const MAX_DECODED_MIB: u32 = 64;

/// The worst-case peak while an image is decoded and converted, as a multiple of the decoded size (RGBA8, `width x
/// height x 4`): **at most 4x**. The worst case is a 16-bit RGBA image that needs a colour conversion: the decoder's
/// output (8 bytes a pixel), the same data as the 16-bit RGBA the transform reads (moved, not copied, for that layout) and
/// the transform's 16-bit output (8 bytes a pixel) are 16 bytes a pixel, or 4x, at the transform. Every other layout is
/// lower. Decoder scratch and metadata are not accounted: the contract is a limit on the decoded image, not an exact peak.
/// The decoder's `max_alloc` is set to this multiple, so a decoder that does enforce it never refuses an image the
/// decoded-size check admits.
pub const PEAK_FACTOR: u64 = 4;

/// The most bytes of a file a measure reads. A JPEG can be smaller than its pixels, and a PNG is about as large as its
/// pixels, so twice the decoded cap covers every image the decoded cap admits.
const MAX_FILE_BYTES: u64 = 2 * ((MAX_DECODED_MIB as u64) << 20);

/// Where the pixels come from. JSON: `{"asset": "b3:…"}` for an image in the store, or `{"path": "refs/front.png"}` for
/// a file the grants allow (relative paths resolve from the project folder).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ImageRef {
    /// An image already in the store, by the hash of its bytes.
    Asset {
        /// The asset's content hash.
        asset: ContentHash,
    },
    /// A file, read through the file fence (`OpCtx::fs_access`).
    File {
        /// The path as the caller wrote it.
        path: String,
    },
}

/// A decoded image: straight (not premultiplied) sRGB RGBA8, row-major, so a transparent pixel keeps its alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// `width * height` pixels as RGBA quads (`r, g, b, a` bytes), row by row from the top.
    pub pixels: Vec<u8>,
    /// The hash of the file's bytes, or the asset's hash: the name a target caches its numbers under.
    pub hash: ContentHash,
}

/// Reads and decodes the image `image` names (PNG 8 or 16 bit, grey, RGB or RGBA; JPEG) to straight sRGB RGBA8.
///
/// `field` names the argument in errors, such as `reference.image`.
///
/// **Size.** The decoded image (`width x height x 4`, the RGBA8 kept) may not exceed [`MAX_DECODED_MIB`] MiB, and no side
/// may exceed [`MAX_SIDE`] px; both are checked once from the header, before any pixel is allocated. The worst-case peak
/// while a tagged image is converted is [`PEAK_FACTOR`] times the decoded size.
///
/// **Colour.** An image tagged with a colour space is converted to sRGB by `moxcms`, at its source precision (16-bit
/// stays 16-bit until the last step): an embedded ICC profile (RGB, or grayscale on a grayscale image), a PNG `cICP`
/// chunk, or a PNG's `gAMA` and `cHRM` chunks (primaries from `cHRM` or sRGB's, a gamma curve from `gAMA` or sRGB's; the
/// `sRGB` chunk or a gamma near 1/2.2 with sRGB primaries is sRGB as it is). An untagged image is sRGB.
///
/// # Errors
/// - `NotFound` (the field) when the store has no asset with that hash, or the file is not there.
/// - `Unavailable` when the op's services carry no store, for an asset reference.
/// - `Denied` (the field) when the grants do not let the file be read, or the path is a link that cannot be followed.
/// - `Invalid` (the field) with [`measure-image-too-large`](messages::image_too_large) when the image is larger than
///   [`MAX_SIDE`] a side or [`MAX_DECODED_MIB`] decoded.
/// - `Invalid` (the field) with [`measure-image-unreadable`](messages::image_unreadable) when the bytes are not a PNG or
///   JPEG (naming the format when they are another one), a file is over [`MAX_FILE_BYTES`], the image's colour
///   information cannot be read (a profile the decoder dropped or could not assemble, colour chunks that fail to
///   decode), or it is a CMYK JPEG, which is refused with or without a profile.
/// - `Invalid` (the field) with [`measure-image-colour-unsupported`](messages::image_colour_unsupported) when the colour
///   engine cannot parse or transform the declared colour space.
/// - `Internal` for a store that fails (corrupt bytes, an I/O error) or a file that cannot be read after the check.
///
/// An asset is fetched whole before its size is checked: the store has no size query or bounded read yet, so the asset
/// path has no cap before the fetch (#1876).
pub fn load(ctx: &OpCtx<'_>, image: &ImageRef, field: &str) -> Result<Rgba8, OpError> {
    let (bytes, hash) = source(ctx, image, field)?;
    decode(&bytes, hash, field)
}

/// The bytes of the image, and the hash they are known by.
fn source(ctx: &OpCtx<'_>, image: &ImageRef, field: &str) -> Result<(Vec<u8>, ContentHash), OpError> {
    match image {
        ImageRef::Asset { asset } => Ok((fetch(ctx, asset, field)?, *asset)),
        ImageRef::File { path } => {
            let bytes = read_file(ctx, path, field)?;
            let hash = ContentHash::of(&bytes);
            Ok((bytes, hash))
        }
    }
}

/// The bytes of a stored asset. Its stored size is checked against [`MAX_FILE_BYTES`] first, so a hash that names a
/// large video or model is refused without being read; the store checks the bytes against the hash as it hands them
/// over.
fn fetch(ctx: &OpCtx<'_>, hash: &ContentHash, field: &str) -> Result<Vec<u8>, OpError> {
    let store = ctx.service::<dyn Store>()?;
    let size = store.size(hash).map_err(|error| at(field, store_error(hash, error)))?;
    if size > MAX_FILE_BYTES {
        return Err(refuse_unreadable(field, format!("the asset is larger than {} MiB", MAX_FILE_BYTES >> 20)));
    }
    store.get(hash).map_err(|error| at(field, store_error(hash, error)))
}

/// What a store failure means to the op: an asset that is not there is the caller's to fix; anything else is the
/// store's, and the op cannot do better than report it.
fn store_error(hash: &ContentHash, error: StoreError) -> OpError {
    match error {
        StoreError::Missing(_) => OpError::not_found(format!("no image asset {hash}")),
        StoreError::Corrupt(_) => OpError::internal(format!("image asset {hash} is corrupt in the store")),
        StoreError::Io(message) => OpError::internal(format!("the store could not read image asset {hash}: {message}")),
    }
}

/// The bytes of a file the grants allow. The path is checked for reading, then opened only through the checked path,
/// which refuses a link at every step, so a file swapped for a link after the check cannot be read in its place. The
/// read stops one byte past [`MAX_FILE_BYTES`], so an oversized file is refused without being read to its end.
fn read_file(ctx: &OpCtx<'_>, path: &str, field: &str) -> Result<Vec<u8>, OpError> {
    let checked = ctx.fs_access(path, FsMode::Read).map_err(|error| at(field, error))?;
    let file = checked.open_read().map_err(|error| at(field, error))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| OpError::internal(format!("reading {checked}: {error}")))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(refuse_unreadable(field, format!("the file is larger than {} MiB", MAX_FILE_BYTES >> 20)));
    }
    Ok(bytes)
}

/// Decodes `bytes` to straight sRGB RGBA8 under the decoder's limits, then converts a tagged colour space to sRGB.
///
/// # Errors
/// The `Invalid` refusals of [`load`]; `hash` is recorded as the image's name in the result.
pub fn decode(bytes: &[u8], hash: ContentHash, field: &str) -> Result<Rgba8, OpError> {
    let format = guess(bytes).map_err(|reason| refuse_unreadable(field, reason))?;
    let mut input = reader(bytes).map_err(|error| unreadable(field, &error))?;
    input.limits(limits());
    let mut decoder = input.into_decoder().map_err(|error| decode_error(bytes, field, error))?;
    // The decoded-size limit, once, from the header: the RGBA8 the measure keeps may not exceed it. The decoder's own
    // `max_alloc` (a multiple of it, see PEAK_FACTOR) is the decoder's business.
    let (width, height) = decoder.dimensions();
    let mut size_limit = limits();
    size_limit.max_alloc = Some(u64::from(MAX_DECODED_MIB) << 20);
    size_limit
        .check_dimensions(width, height)
        .and_then(|()| size_limit.reserve(u64::from(width) * u64::from(height) * RGBA_BYTES))
        .map_err(|error| decode_error(bytes, field, error))?;
    // The colour metadata is read by the same decoders that decode the pixels, and any failure to read it refuses the image:
    // a fallback to "no profile" would let a file whose colour could not be read pass as untagged sRGB.
    let unreadable_colour = || refuse_unreadable(field, "couldn't read the image's colour information".to_owned());
    let (icc, cmyk) = if format == ImageFormat::Jpeg {
        let (cmyk, icc) = jpeg_colour(bytes).ok_or_else(unreadable_colour)?;
        (icc, cmyk)
    } else {
        (decoder.icc_profile().map_err(|_| unreadable_colour())?, false)
    };
    if cmyk {
        // The decoder hands back RGB it has already made from the inks, with no way to get the ink values a CMYK profile
        // needs, so no CMYK image can be converted faithfully: its colours cannot be known.
        return Err(refuse_unreadable(
            field,
            "CMYK images aren't supported for measuring; save it as RGB (sRGB or Display P3) first".to_owned(),
        ));
    }
    let profile_missing = icc.is_none();
    let declared = if format == ImageFormat::Png {
        png_colour(bytes, icc).ok_or_else(unreadable_colour)?
    } else {
        Declared::from_icc(icc)
    };
    // A file that declares an embedded profile the decoder could not give (a PNG `iCCP` that is corrupt or past the limit;
    // JPEG APP2 chunks that do not number consistently) has colour this cannot read: refused, never read as untagged. Only
    // a PNG `cICP` the decoder accepted decides the colour anyway (PNG colour chunk precedence).
    let has_profile_tag = if format == ImageFormat::Jpeg {
        jpeg_declares_profile(bytes).ok_or_else(|| {
            refuse_unreadable(field, "its segments do not follow the JPEG layout (bytes between markers)".to_owned())
        })?
    } else {
        png_declares_profile(bytes)
    };
    if profile_missing && has_profile_tag && !matches!(declared, Declared::Cicp(_)) {
        return Err(refuse_unreadable(
            field,
            "its embedded colour profile is present but cannot be read (corrupt, incomplete or too large)".to_owned(),
        ));
    }
    let dynamic = DynamicImage::from_decoder(decoder).map_err(|error| decode_error(bytes, field, error))?;
    let pixels = to_srgb(dynamic, declared, field)?;
    Ok(Rgba8 { width, height, pixels, hash })
}

/// Bytes a decoded pixel takes in the RGBA8 the measure keeps.
const RGBA_BYTES: u64 = 4;

/// Whether a PNG declares an embedded ICC profile, to tell a profile the decoder dropped from one that was never there.
/// It decides no colour. Found by the PNG chunk framing (length and kind); a malformed file fails the decode instead.
fn png_declares_profile(bytes: &[u8]) -> bool {
    let be = |at: usize| bytes.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes);
    let mut at = 8;
    while let (Some(length), Some(kind)) = (be(at), bytes.get(at + 4..at + 8)) {
        match kind {
            b"iCCP" => return true,
            b"IDAT" | b"IEND" => return false,
            _ => {}
        }
        at = at.saturating_add(12).saturating_add(length as usize);
    }
    false
}

/// Whether a JPEG declares an embedded ICC profile, the way the format defines it: the segments of the main image are
/// walked from `SOI` by their length fields up to `SOS`, and only an APP2 segment whose payload starts with
/// `ICC_PROFILE\0` counts. Nothing inside an APP1 (EXIF, where a thumbnail may carry a profile of its own) or a comment is
/// looked at. `None` when the walk meets bytes that are not a marker, or a segment that runs past the end: the file is
/// ambiguous (junk between segments), and the caller refuses it.
fn jpeg_declares_profile(bytes: &[u8]) -> Option<bool> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2;
    loop {
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        // Any number of 0xFF fill bytes may precede a marker.
        while *bytes.get(at + 1)? == 0xFF {
            at += 1;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            0x00 => return None,
            0x01 | 0xD0..=0xD8 => at += 2,
            0xD9 | 0xDA => return Some(false),
            _ => {
                let length = usize::from(u16::from_be_bytes([*bytes.get(at + 2)?, *bytes.get(at + 3)?]));
                let segment = bytes.get(at + 4..at + 2 + length.max(2))?;
                if marker == 0xE2 && segment.starts_with(b"ICC_PROFILE\0") {
                    return Some(true);
                }
                at += 2 + length.max(2);
            }
        }
    }
}

/// A JPEG's colour as `zune-jpeg` reads it from its headers: whether it has four components (CMYK or YCCK), and the ICC
/// profile as the decoder assembles it from the APP2 segments (`None` when there is none, or when the segments do not
/// number consistently, which the caller tells apart by [`jpeg_declares_profile`]). `None` when the headers cannot be read.
fn jpeg_colour(bytes: &[u8]) -> Option<(bool, Option<Vec<u8>>)> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(bytes));
    decoder.decode_headers().ok()?;
    let cmyk = matches!(decoder.input_colorspace(), Some(ColorSpace::CMYK | ColorSpace::YCCK));
    Some((cmyk, decoder.icc_profile()))
}

/// The colour space a file declares for its pixels, in the order of the PNG specification: `cICP`, an embedded profile,
/// the `sRGB` chunk, then `gAMA` and `cHRM`. With none of them the pixels are sRGB.
enum Declared {
    /// Untagged, or tagged sRGB by the `sRGB` chunk: the pixels are sRGB already.
    Srgb,
    /// A `cICP` declaration.
    Cicp(CicpProfile),
    /// An embedded ICC profile.
    Icc(Vec<u8>),
    /// A PNG's `gAMA` (the file gamma times 100 000, when present) and `cHRM` (white, red, green, blue as x, y, when
    /// present), which define its colour space when nothing else does.
    Chunks { gamma: Option<u32>, chromaticities: Option<[(f64, f64); 4]> },
}

impl Declared {
    fn from_icc(icc: Option<Vec<u8>>) -> Self {
        icc.map_or(Self::Srgb, Self::Icc)
    }
}

/// The colour declaration of a PNG as the `png` decoder validated it (it drops a chunk that is out of order, repeated or
/// has a bad CRC); `image` exposes only the profile, which is passed in.
///
/// `None` when the chunks cannot be read under the same limit the pixels are decoded under: the image is refused then, not
/// read as untagged.
fn png_colour(bytes: &[u8], icc: Option<Vec<u8>>) -> Option<Declared> {
    let limit = png::Limits { bytes: usize::try_from(limits().max_alloc.unwrap_or(u64::MAX)).unwrap_or(usize::MAX) };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limit);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let reader = decoder.read_info().ok()?;
    let info = reader.info();
    if let Some(code) = info.coding_independent_code_points
        && let (Ok(color_primaries), Ok(transfer_characteristics), Ok(matrix_coefficients)) = (
            CicpColorPrimaries::try_from(code.color_primaries),
            TransferCharacteristics::try_from(code.transfer_function),
            MatrixCoefficients::try_from(code.matrix_coefficients),
        )
    {
        return Some(Declared::Cicp(CicpProfile {
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            full_range: code.is_video_full_range_image,
        }));
    }
    if info.coding_independent_code_points.is_some() {
        // A `cICP` whose code points `moxcms` does not know: the colour is declared and cannot be converted.
        return Some(Declared::Icc(Vec::new()));
    }
    if let Some(icc) = icc {
        return Some(Declared::Icc(icc));
    }
    if info.srgb.is_some() {
        return Some(Declared::Srgb);
    }
    let gamma = info.gama_chunk.map(png::ScaledFloat::into_scaled);
    let chromaticities = info.chrm_chunk.map(|chrm| {
        [chrm.white, chrm.red, chrm.green, chrm.blue]
            .map(|(x, y)| (f64::from(x.into_value()), f64::from(y.into_value())))
    });
    Some(if gamma.is_none() && chromaticities.is_none() {
        Declared::Srgb
    } else {
        Declared::Chunks { gamma, chromaticities }
    })
}

/// The sRGB file gamma (1 / 2.2 nearly, `gAMA` 45 455) and how far a file's may stray and still be called sRGB: two units
/// of rounding in the fourth decimal either way of the nominal 1 / 2.2.
const SRGB_GAMMA: u32 = 45_455;
const GAMMA_TOLERANCE: u32 = 500;

/// The sRGB primaries and white point as `cHRM` writes them (x, y for white, red, green, blue), and the rounding a file's
/// four-decimal values may differ by.
const SRGB_CHROMATICITIES: [(f64, f64); 4] = [(0.3127, 0.3290), (0.64, 0.33), (0.30, 0.60), (0.15, 0.06)];
const CHROMATICITY_TOLERANCE: f64 = 0.0015;

/// The profile a PNG's `gAMA` and `cHRM` define, or `None` when they describe sRGB (a `gAMA` near 1 / 2.2 and primaries
/// within rounding of sRGB's, or either one absent): the pixels need no conversion then. A missing `cHRM` means sRGB's
/// primaries; a missing `gAMA` means sRGB's curve. A gamma near sRGB's beside other primaries keeps the sRGB curve.
#[allow(clippy::cast_possible_truncation, reason = "the colour engine works in f32; a chunk's values have five digits")]
fn chunk_profile(gamma: Option<u32>, chromaticities: Option<[(f64, f64); 4]>) -> Option<ColorProfile> {
    let srgb_gamma = gamma.is_none_or(|g| g.abs_diff(SRGB_GAMMA) <= GAMMA_TOLERANCE);
    let srgb_primaries = chromaticities.is_none_or(|c| {
        c.iter().zip(SRGB_CHROMATICITIES).all(|(got, want)| {
            (got.0 - want.0).abs() <= CHROMATICITY_TOLERANCE && (got.1 - want.1).abs() <= CHROMATICITY_TOLERANCE
        })
    });
    if srgb_gamma && srgb_primaries {
        return None;
    }
    let mut profile = ColorProfile::new_srgb();
    // `new_srgb` carries an sRGB `cICP`, and the engine takes the transfer curve from a `cICP` over the curves set below.
    profile.cicp = None;
    if let Some([white, red, green, blue]) = chromaticities {
        let point = |(x, y): (f64, f64)| Chromaticity { x: x as f32, y: y as f32 };
        profile.update_rgb_colorimetry(
            XyY { x: white.0, y: white.1, yb: 1.0 },
            ColorPrimaries { red: point(red), green: point(green), blue: point(blue) },
        );
    }
    if let (false, Some(gamma)) = (srgb_gamma, gamma) {
        // The file gamma is the exponent from linear light to the stored value, so the decoding curve is its inverse.
        let curve = ToneReprCurve::Parametric(vec![(100_000.0 / f64::from(gamma)) as f32]);
        profile.red_trc = Some(curve.clone());
        profile.green_trc = Some(curve.clone());
        profile.blue_trc = Some(curve);
    }
    Some(profile)
}

/// Converts `native` (the decoder's output, at its own precision) from the colour space the file declares to straight
/// sRGB RGBA8. The transform runs at the source precision, 16-bit for a 16-bit image and 8-bit otherwise, and the result
/// is quantised to a byte last, so a dark 16-bit sample is not rounded to zero before it is converted. A grayscale profile
/// converts a grayscale image through the engine's gray layout. Only a declaration `moxcms` cannot parse or transform is
/// refused.
fn to_srgb(native: DynamicImage, declared: Declared, field: &str) -> Result<Vec<u8>, OpError> {
    let source = match declared {
        Declared::Srgb => return Ok(native.into_rgba8().into_raw()),
        Declared::Cicp(code) => {
            let is_srgb = code.color_primaries == CicpColorPrimaries::Bt709
                && code.transfer_characteristics == TransferCharacteristics::Srgb
                && code.full_range;
            if is_srgb {
                return Ok(native.into_rgba8().into_raw());
            }
            let mut profile = ColorProfile::new_srgb();
            profile.update_rgb_colorimetry_from_cicp(code);
            // `new_srgb` carries a `cICP` of its own, so only the one just set proves the declaration was understood.
            let understood = profile.cicp.is_some_and(|set| {
                set.color_primaries == code.color_primaries
                    && set.transfer_characteristics == code.transfer_characteristics
            });
            if !understood || !code.full_range {
                return Err(colour_unsupported(field, "a cICP colour declaration the colour engine cannot transform"));
            }
            profile
        }
        Declared::Icc(icc) => ColorProfile::new_from_slice(&icc).map_err(|error| {
            colour_unsupported(field, &format!("an embedded colour profile that cannot be read ({error})"))
        })?,
        Declared::Chunks { gamma, chromaticities } => match chunk_profile(gamma, chromaticities) {
            Some(profile) => profile,
            None => return Ok(native.into_rgba8().into_raw()),
        },
    };
    let gray_image = matches!(
        native,
        DynamicImage::ImageLuma8(_)
            | DynamicImage::ImageLumaA8(_)
            | DynamicImage::ImageLuma16(_)
            | DynamicImage::ImageLumaA16(_)
    );
    let gray = match source.color_space {
        DataColorSpace::Rgb => false,
        DataColorSpace::Gray if gray_image => true,
        DataColorSpace::Gray => {
            return Err(colour_unsupported(field, "a grayscale colour profile on an image that is not grayscale"));
        }
        _ => return Err(colour_unsupported(field, "an embedded colour profile that is neither RGB nor grayscale")),
    };
    let cannot = |error: moxcms::CmsError| {
        colour_unsupported(field, &format!("a colour profile that cannot be converted ({error})"))
    };
    let srgb = ColorProfile::new_srgb();
    let options = TransformOptions::default();
    let alpha = native.color().has_alpha();
    let wide = matches!(
        native,
        DynamicImage::ImageLuma16(_)
            | DynamicImage::ImageLumaA16(_)
            | DynamicImage::ImageRgb16(_)
            | DynamicImage::ImageRgba16(_)
    );
    let layout = match (gray, alpha) {
        (true, false) => Layout::Gray,
        (true, true) => Layout::GrayAlpha,
        (false, _) => Layout::Rgba,
    };
    let channels = if gray { 1 + usize::from(alpha) } else { 4 };
    if wide {
        let samples = match (gray, alpha) {
            (true, false) => native.into_luma16().into_raw(),
            (true, true) => native.into_luma_alpha16().into_raw(),
            (false, _) => native.into_rgba16().into_raw(),
        };
        let transform = source.create_transform_16bit(layout, &srgb, Layout::Rgba, options).map_err(cannot)?;
        let mut out = vec![0_u16; samples.len() / channels * 4];
        transform.transform(&samples, &mut out).map_err(cannot)?;
        drop(samples);
        Ok(out.into_iter().map(|v| u8::try_from((u32::from(v) + 128) / 257).unwrap_or(u8::MAX)).collect())
    } else {
        let samples = match (gray, alpha) {
            (true, false) => native.into_luma8().into_raw(),
            (true, true) => native.into_luma_alpha8().into_raw(),
            (false, _) => native.into_rgba8().into_raw(),
        };
        let transform = source.create_transform_8bit(layout, &srgb, Layout::Rgba, options).map_err(cannot)?;
        let mut out = vec![0_u8; samples.len() / channels * 4];
        transform.transform(&samples, &mut out).map_err(cannot)?;
        Ok(out)
    }
}

/// An error from building or running the decoder: a limit is the image being too large (named by its size, read from the
/// header), anything else is bytes the decoder cannot read.
fn decode_error(bytes: &[u8], field: &str, error: ImageError) -> OpError {
    match error {
        ImageError::Limits(_) => {
            let (width, height) = reader(bytes).ok().and_then(|r| r.into_dimensions().ok()).unwrap_or((0, 0));
            too_large(field, width, height)
        }
        other => unreadable(field, &other),
    }
}

/// The image's format, which must be PNG or JPEG: a format another crate enables on the shared `image` dependency is
/// refused here, before its header is read, and named in the refusal.
fn guess(bytes: &[u8]) -> Result<ImageFormat, String> {
    let format = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?
        .format()
        .ok_or_else(|| "its format is not one a measure knows".to_owned())?;
    match format {
        ImageFormat::Png | ImageFormat::Jpeg => Ok(format),
        other => {
            let name = other.extensions_str().first().map_or("unknown", |extension| *extension).to_uppercase();
            Err(format!("it is a {name} file, and a measure reads only PNG and JPEG"))
        }
    }
}

/// A reader over the bytes, with the format guessed from them. Only the PNG and JPEG decoders are compiled in.
fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, ImageError> {
    Ok(ImageReader::new(Cursor::new(bytes)).with_guessed_format()?)
}

/// The decoder's limits (design 4.1, #282): the sides, and `max_alloc` at the worst-case peak ([`PEAK_FACTOR`] times the
/// decoded-size limit).
fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some((u64::from(MAX_DECODED_MIB) << 20) * PEAK_FACTOR);
    limits
}

/// Refuses an image larger than the limits, naming its size and them.
fn too_large(field: &str, width: u32, height: u32) -> OpError {
    let text =
        messages::image_too_large(i64::from(width), i64::from(height), i64::from(MAX_SIDE), i64::from(MAX_DECODED_MIB));
    OpError::invalid(field, messages::english(&text))
}

/// Refuses bytes the decoder cannot read, with the decoder's reason.
fn unreadable(field: &str, error: &ImageError) -> OpError {
    refuse_unreadable(field, error.to_string())
}

/// Refuses an image that is not a PNG or JPEG, or cannot be read, with the reason.
fn refuse_unreadable(field: &str, reason: String) -> OpError {
    let text = messages::image_unreadable(reason);
    OpError::invalid(field, messages::english(&text))
}

/// Refuses an image that is not sRGB, naming what was found.
fn colour_unsupported(field: &str, found: &str) -> OpError {
    let text = messages::image_colour_unsupported(found.to_owned());
    OpError::invalid(field, messages::english(&text))
}

/// Names the argument at fault in an error from elsewhere (a store or a file check), keeping its code and text.
fn at(field: &str, mut error: OpError) -> OpError {
    error.field = Some(field.to_owned());
    error
}
