//! Reads a reference or render image for a measure (design 4.1): from the store by its hash, or from a file the grants
//! allow, decoded to straight sRGB RGBA8. Only PNG and JPEG are read. The size is limited by the decoder's own `Limits`
//! (8192 px a side, 64 MiB), which refuse an oversized header before any pixel is allocated. An image tagged with a colour
//! space (an embedded ICC profile, or a PNG `cICP` chunk) is converted to sRGB with `moxcms`, the colour engine the
//! `image` crate already uses; an untagged image is sRGB.

use std::io::{Cursor, Read};

use ::image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};
use moxcms::{
    CicpColorPrimaries, CicpProfile, ColorProfile, DataColorSpace, Layout, MatrixCoefficients, TransferCharacteristics,
    TransformOptions,
};
use omuzaki_ops_api::{FsMode, OpCtx, OpError};
use omuzaki_store_api::{ContentHash, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::messages;

/// The longest side a measure reads, in pixels (design 4.1).
pub const MAX_SIDE: u32 = 8192;

/// The most the decoder may allocate at once, in MiB, its output image included (design 4.1).
pub const MAX_DECODED_MIB: u32 = 64;

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

/// Reads and decodes the image `image` names (PNG 8 or 16 bit, grey, RGB or RGBA; JPEG).
///
/// `field` names the argument in errors, such as `reference.image`.
///
/// # Errors
/// - `NotFound` (the field) when the store has no asset with that hash, or the file is not there.
/// - `Unavailable` when the op's services carry no store, for an asset reference.
/// - `Denied` (the field) when the grants do not let the file be read, or the path is a link that cannot be followed.
/// - `Invalid` (the field) with [`measure-image-too-large`](messages::image_too_large) when the image is larger than
///   [`MAX_SIDE`] a side or needs more than [`MAX_DECODED_MIB`] at once, and with
///   [`measure-image-unreadable`](messages::image_unreadable) when the bytes are not a PNG or JPEG (naming the format
///   when they are another one), or a file is over [`MAX_FILE_BYTES`].
/// - `Invalid` (the field) with [`measure-image-colour-unsupported`](messages::image_colour_unsupported) when the image
///   is not sRGB.
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

/// The bytes of a stored asset. The store checks them against the hash before handing them over.
fn fetch(ctx: &OpCtx<'_>, hash: &ContentHash, field: &str) -> Result<Vec<u8>, OpError> {
    let store = ctx.service::<dyn Store>()?;
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
    // Not every decoder enforces `max_alloc` before it allocates its output (the PNG decoder checks its own buffers, not
    // the output image), so the limits' own accounting is applied to the buffers this decode will hold: the decoder's
    // output, and the RGBA8 copy the measure keeps beside it. An oversized header is refused here, before either exists.
    let (width, height) = decoder.dimensions();
    let mut budget = limits();
    budget
        .check_dimensions(width, height)
        .and_then(|()| budget.reserve(decoder.total_bytes()))
        .and_then(|()| budget.reserve(u64::from(width) * u64::from(height) * 4))
        .map_err(|error| decode_error(bytes, field, error))?;
    let icc = decoder.icc_profile().map_err(|error| unreadable(field, &error))?;
    let cicp = if format == ImageFormat::Png { png_cicp(bytes) } else { None };
    let dynamic = DynamicImage::from_decoder(decoder).map_err(|error| decode_error(bytes, field, error))?;
    let rgba = dynamic.into_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = to_srgb(rgba.into_raw(), cicp, icc, field)?;
    Ok(Rgba8 { width, height, pixels, hash })
}

/// The `cICP` code points of a PNG, as the `png` decoder validated them (it drops one out of order or with a bad CRC).
fn png_cicp(bytes: &[u8]) -> Option<CicpProfile> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let reader = decoder.read_info().ok()?;
    let code = reader.info().coding_independent_code_points?;
    Some(CicpProfile {
        color_primaries: CicpColorPrimaries::try_from(code.color_primaries).ok()?,
        transfer_characteristics: TransferCharacteristics::try_from(code.transfer_function).ok()?,
        matrix_coefficients: MatrixCoefficients::try_from(code.matrix_coefficients).ok()?,
        full_range: code.is_video_full_range_image,
    })
}

/// Converts straight RGBA8 `pixels` from the colour space the file declares to sRGB. A `cICP` declaration outranks an
/// embedded profile (PNG colour chunk precedence); with neither, the pixels are sRGB already. Only a profile `moxcms`
/// cannot parse or transform is refused.
fn to_srgb(pixels: Vec<u8>, cicp: Option<CicpProfile>, icc: Option<Vec<u8>>, field: &str) -> Result<Vec<u8>, OpError> {
    let srgb = ColorProfile::new_srgb();
    let source = match (cicp, icc) {
        (Some(code), _) => {
            let is_srgb = code.color_primaries == CicpColorPrimaries::Bt709
                && code.transfer_characteristics == TransferCharacteristics::Srgb
                && code.full_range;
            if is_srgb {
                return Ok(pixels);
            }
            let mut profile = ColorProfile::new_srgb();
            profile.update_rgb_colorimetry_from_cicp(code);
            if profile.cicp.is_none() || !code.full_range {
                return Err(colour_unsupported(field, "a cICP colour declaration the colour engine cannot transform"));
            }
            profile
        }
        (None, Some(icc)) => ColorProfile::new_from_slice(&icc).map_err(|error| {
            colour_unsupported(field, &format!("an embedded colour profile that cannot be read ({error})"))
        })?,
        (None, None) => return Ok(pixels),
    };
    if source.color_space != DataColorSpace::Rgb {
        return Err(colour_unsupported(field, "an embedded colour profile that is not RGB"));
    }
    let cannot = |error: moxcms::CmsError| {
        colour_unsupported(field, &format!("a colour profile that cannot be converted ({error})"))
    };
    let transform =
        source.create_transform_8bit(Layout::Rgba, &srgb, Layout::Rgba, TransformOptions::default()).map_err(cannot)?;
    let mut out = vec![0_u8; pixels.len()];
    transform.transform(&pixels, &mut out).map_err(cannot)?;
    Ok(out)
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

/// The decoder's limits (design 4.1, #282): the sides and the most it may allocate at once, its output included.
fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(u64::from(MAX_DECODED_MIB) << 20);
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
