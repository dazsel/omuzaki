//! Reads a reference or render image for a measure (design 4.1): from the store by its hash, or from a file the grants
//! allow, decoded to straight sRGB RGBA8. Both paths refuse an oversized image from its header, before any pixel is
//! allocated, so a crafted file cannot make a measure allocate what it likes.

use std::io::{Cursor, Read};

use ::image::{ImageError, ImageReader, Limits};
use omuzaki_ops_api::{FsMode, OpCtx, OpError};
use omuzaki_store_api::{ContentHash, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::messages;

/// The longest side a measure reads, in pixels (design 4.1).
pub const MAX_SIDE: u32 = 8192;

/// The most a measure holds decoded, in MiB: its RGBA8 pixels (4 bytes each) and the decoder's own buffers are both
/// capped by it.
pub const MAX_DECODED_MIB: u32 = 64;

/// Bytes a decoded pixel takes in the RGBA8 the measure holds.
const RGBA_BYTES: u64 = 4;

/// The decoded cap in bytes.
fn max_decoded_bytes() -> u64 {
    u64::from(MAX_DECODED_MIB) << 20
}

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
    /// `width * height` pixels, row by row from the top, each `[r, g, b, a]`.
    pub pixels: Vec<[u8; 4]>,
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
///   [`MAX_SIDE`] a side or [`MAX_DECODED_MIB`] decoded, and with
///   [`measure-image-unreadable`](messages::image_unreadable) when the bytes are not a PNG or JPEG.
/// - `Internal` for a store that fails (corrupt bytes, an I/O error) or a file that cannot be read after the check.
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
/// which refuses a link at every step, so a file swapped for a link after the check cannot be read in its place.
fn read_file(ctx: &OpCtx<'_>, path: &str, field: &str) -> Result<Vec<u8>, OpError> {
    let checked = ctx.fs_access(path, FsMode::Read).map_err(|error| at(field, error))?;
    let mut file = checked.open_read().map_err(|error| at(field, error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| OpError::internal(format!("reading {checked}: {error}")))?;
    Ok(bytes)
}

/// Decodes `bytes`, refusing an oversized image from its header before the decoder allocates anything.
fn decode(bytes: &[u8], hash: ContentHash, field: &str) -> Result<Rgba8, OpError> {
    let (width, height) =
        reader(bytes).and_then(ImageReader::into_dimensions).map_err(|error| unreadable(field, &error))?;
    if !fits(width, height) {
        return Err(too_large(field, width, height));
    }
    let mut input = reader(bytes).map_err(|error| unreadable(field, &error))?;
    input.limits(limits());
    let decoded = input.decode().map_err(|error| match error {
        ImageError::Limits(_) => too_large(field, width, height),
        other => unreadable(field, &other),
    })?;
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = rgba.pixels().map(|pixel| pixel.0).collect();
    Ok(Rgba8 { width, height, pixels, hash })
}

/// A reader over the bytes, with the format guessed from them. Only the PNG and JPEG decoders are compiled in.
fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, ImageError> {
    Ok(ImageReader::new(Cursor::new(bytes)).with_guessed_format()?)
}

/// The decoder's own limits, the same caps the header check applies.
fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(max_decoded_bytes());
    limits
}

/// Whether an image of this size is within the caps: the sides, and its RGBA8 size in bytes.
fn fits(width: u32, height: u32) -> bool {
    width <= MAX_SIDE && height <= MAX_SIDE && u64::from(width) * u64::from(height) * RGBA_BYTES <= max_decoded_bytes()
}

/// Refuses an image larger than the caps, naming its size and the caps.
fn too_large(field: &str, width: u32, height: u32) -> OpError {
    let text =
        messages::image_too_large(i64::from(width), i64::from(height), i64::from(MAX_SIDE), i64::from(MAX_DECODED_MIB));
    OpError::invalid(field, messages::english(&text))
}

/// Refuses bytes the decoder cannot read, with the decoder's reason.
fn unreadable(field: &str, error: &ImageError) -> OpError {
    let text = messages::image_unreadable(error.to_string());
    OpError::invalid(field, messages::english(&text))
}

/// Names the argument at fault in an error from elsewhere (a store or a file check), keeping its code and text.
fn at(field: &str, mut error: OpError) -> OpError {
    error.field = Some(field.to_owned());
    error
}
