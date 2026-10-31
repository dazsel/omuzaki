//! Reads the images parity compares: from the store by hash, from a file the grants allow, or inline as base64, decoded
//! to straight sRGB RGBA8. PNG and JPEG only, at most 8192 px a side and 64 MiB decoded, refused from the header before
//! any pixel is allocated. An image with an embedded RGB colour profile is converted to sRGB with `moxcms` (8-bit), so
//! a screenshot tagged Display P3 compares in the same colours as an untagged render; an untagged image is sRGB.
//!
//! This is a smaller sibling of `omuzaki-measure`'s reader (which also reads `cICP` and 16-bit profiles): parity links
//! into the Apps on its own, so it does not depend on the measure crate.

use std::io::{Cursor, Read as _};

use base64::Engine as _;
use image::{DynamicImage, ImageDecoder as _, ImageFormat, ImageReader, Limits};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};
use omuzaki_ops_api::{FsMode, OpCtx, OpError};
use omuzaki_store_api::{ContentHash, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The longest side read, in pixels.
pub const MAX_SIDE: u32 = 8192;
/// The most a decoded image may take, in MiB (RGBA8).
pub const MAX_DECODED_MIB: u64 = 64;
/// The most bytes of a file or an asset read.
const MAX_FILE_BYTES: u64 = 2 * (MAX_DECODED_MIB << 20);

/// Where the pixels come from: `{"asset": "<hash>"}` (what `render.preview` returns), `{"path": "refs/front.png"}`
/// (relative paths resolve from the project folder), or `{"data": "<base64 PNG or JPEG>"}` (a `data:` URL prefix is
/// allowed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ImageRef {
    /// An image in the store.
    Asset {
        /// The asset's content hash.
        asset: ContentHash,
    },
    /// A file, read through the file fence.
    File {
        /// The path as the caller wrote it.
        path: String,
    },
    /// The image's bytes, base64.
    Data {
        /// Base64 PNG or JPEG bytes, optionally as a `data:image/png;base64,` URL.
        data: String,
    },
}

/// A decoded image: straight sRGB RGBA8, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// `width * height` RGBA quads.
    pub pixels: Vec<u8>,
    /// The hash of the image's bytes (the asset's hash for an asset).
    pub hash: ContentHash,
}

/// Reads and decodes `image`. `field` names the argument in errors.
///
/// # Errors
/// `Invalid` for an image that can't be read, decoded or converted, or is too large; `NotFound` for a missing asset;
/// the file fence's errors for a path outside the grants; `Internal` for a failing store.
pub fn load(ctx: &OpCtx<'_>, image: &ImageRef, field: &str) -> Result<Rgba8, OpError> {
    let (bytes, hash) = match image {
        ImageRef::Asset { asset } => {
            let store = ctx.service::<dyn Store>()?;
            let too_big = store.size(asset).map_err(|e| store_error(asset, e))? > MAX_FILE_BYTES;
            if too_big {
                return Err(OpError::invalid(
                    field,
                    format!("asset {asset} is larger than {} MiB", MAX_FILE_BYTES >> 20),
                ));
            }
            (store.get(asset).map_err(|e| store_error(asset, e))?, *asset)
        }
        ImageRef::File { path } => {
            let checked = ctx.fs_access(path, FsMode::Read)?;
            let mut bytes = Vec::new();
            checked
                .open_read()?
                .take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| OpError::internal(format!("reading {checked}: {e}")))?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err(OpError::invalid(field, format!("{path} is larger than {} MiB", MAX_FILE_BYTES >> 20)));
            }
            let hash = ContentHash::of(&bytes);
            (bytes, hash)
        }
        ImageRef::Data { data } => {
            let text = data.split_once("base64,").map_or(data.as_str(), |(_, b)| b).trim();
            if text.len() as u64 > MAX_FILE_BYTES * 4 / 3 {
                return Err(OpError::invalid(field, "the inline image is too large")
                    .hint("store it first, pass {\"asset\": …}"));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|e| OpError::invalid(field, format!("the inline image is not base64: {e}")))?;
            let hash = ContentHash::of(&bytes);
            (bytes, hash)
        }
    };
    decode(&bytes, hash, field)
}

fn store_error(hash: &ContentHash, error: StoreError) -> OpError {
    match error {
        StoreError::Missing(_) => OpError::not_found(format!("no image asset {hash}")),
        other => OpError::internal(format!("the store could not read image asset {hash}: {other}")),
    }
}

/// Decodes PNG or JPEG bytes to straight sRGB RGBA8 under the limits, converting an embedded RGB profile to sRGB.
///
/// # Errors
/// `Invalid` for bytes that are not a PNG or JPEG, are too large, or carry a profile that can't be converted.
pub fn decode(bytes: &[u8], hash: ContentHash, field: &str) -> Result<Rgba8, OpError> {
    let unreadable = |why: String| OpError::invalid(field, format!("the image can't be read: {why}"));
    let mut reader =
        ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| unreadable(e.to_string()))?;
    if !matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg)) {
        return Err(unreadable("only PNG and JPEG are read".to_owned()));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some((MAX_DECODED_MIB << 20) * 4);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| unreadable(e.to_string()))?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) * 4 > MAX_DECODED_MIB << 20 {
        return Err(OpError::invalid(field, format!("{width}x{height} is larger than {MAX_DECODED_MIB} MiB decoded"))
            .hint(format!("at most {MAX_SIDE} px a side")));
    }
    let icc = decoder.icc_profile().map_err(|e| unreadable(e.to_string()))?;
    let dynamic = DynamicImage::from_decoder(decoder).map_err(|e| unreadable(e.to_string()))?;
    let gray = !dynamic.color().has_color();
    let rgba = dynamic.into_rgba8().into_raw();
    let pixels = match icc {
        Some(icc) if !gray => to_srgb(&icc, rgba).map_err(|why| {
            OpError::invalid(field, format!("its colour profile can't be converted: {why}"))
                .hint("save it as sRGB first")
        })?,
        _ => rgba,
    };
    Ok(Rgba8 { width, height, pixels, hash })
}

/// RGBA8 in the profile `icc` to sRGB.
fn to_srgb(icc: &[u8], rgba: Vec<u8>) -> Result<Vec<u8>, String> {
    let source = ColorProfile::new_from_slice(icc).map_err(|e| e.to_string())?;
    if source.color_space != DataColorSpace::Rgb {
        return Ok(rgba);
    }
    let transform = source
        .create_transform_8bit(Layout::Rgba, &ColorProfile::new_srgb(), Layout::Rgba, TransformOptions::default())
        .map_err(|e| e.to_string())?;
    let mut out = vec![0u8; rgba.len()];
    transform.transform(&rgba, &mut out).map_err(|e| e.to_string())?;
    Ok(out)
}
