//! Reads a reference or render image for a measure (design 4.1): from the store by its hash, or from a file the grants
//! allow, decoded to straight sRGB RGBA8. Both paths refuse an oversized image from its header, before any pixel is
//! allocated, so a crafted file cannot make a measure allocate what it likes. Only PNG and JPEG are read, and only sRGB.

use std::io::{Cursor, Read};

use ::image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};
use omuzaki_ops_api::{FsMode, OpCtx, OpError};
use omuzaki_store_api::{ContentHash, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::header::{self, JpegFrame};
use crate::messages;

/// The longest side a measure reads, in pixels (design 4.1).
pub const MAX_SIDE: u32 = 8192;

/// The most a measure holds at once while it decodes, in MiB: the decoder's scratch, its output and the RGBA8 pixels the
/// measure keeps. The header gives all three before anything is decoded, and their sum is capped by this.
pub const MAX_DECODED_MIB: u32 = 64;

/// The most bytes of a file a measure reads. A JPEG can be smaller than its pixels, and a PNG is about as large as its
/// pixels, so twice the decoded cap covers every image the decoded cap admits.
const MAX_FILE_BYTES: u64 = 2 * ((MAX_DECODED_MIB as u64) << 20);

/// Bytes a decoded pixel takes in the RGBA8 the measure keeps.
const RGBA_BYTES: u64 = 4;

/// Bytes of one JPEG coefficient block: 64 `i16` coefficients. The decoder keeps them for every component, on whole MCUs.
const COEFFICIENT_BLOCK_BYTES: u64 = 64 * 2;

/// Bytes of a decoder's own state that does not grow with the image: its inflate window and tables for a PNG, its
/// Huffman tables for a JPEG. Measured at about 45 KiB for a PNG, and allowed for at 64 KiB.
const DECODER_STATE_BYTES: u64 = 64 << 10;

/// The decoded cap in bytes.
fn max_decoded_bytes() -> u64 {
    u64::from(MAX_DECODED_MIB) << 20
}

/// The most a PNG's embedded profile may inflate to: a quarter of the decoded cap. The decoder holds one copy and
/// `icc_profile` returns another, so two of them stay well inside the cap; real profiles are a few KiB.
fn max_profile_bytes() -> u64 {
    max_decoded_bytes() / 4
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

/// Decodes `bytes` to straight sRGB RGBA8. The header is read first: the format must be PNG or JPEG, the size and the
/// peak the decoder will hold must be within the caps, and the colour must be sRGB. Only then does the decoder run.
///
/// This is the decode [`load`] runs, public so the allocation budget can be measured on its own (`tests/budget.rs`).
///
/// # Errors
/// The `Invalid` refusals of [`load`]; `hash` is recorded as the image's name in the result.
pub fn decode(bytes: &[u8], hash: ContentHash, field: &str) -> Result<Rgba8, OpError> {
    let format = guess(bytes).map_err(|reason| refuse_unreadable(field, reason))?;
    let plan = plan(bytes, format).map_err(|reason| refuse_unreadable(field, reason))?;
    if !fits(plan.width, plan.height, plan.peak) {
        return Err(too_large(field, plan.width, plan.height));
    }
    // A JPEG decoder keeps the profile it finds while it is built, so the declared length is judged before that, and
    // before `icc_profile` copies it. A PNG profile was bounded while `plan` read its header, and a profile past the bound refused there.
    let declared = if format == ImageFormat::Jpeg { header::jpeg_icc_declared_len(bytes) } else { None };
    if let Some(declared) = declared
        && !profile_fits(&plan, declared)
    {
        return Err(too_large(field, plan.width, plan.height));
    }
    // A PNG whose `cICP` chunk decides the colour never has its profile read, and the decoder is built under the profile
    // bound too, so a large `iCCP` beside it is dropped at that bound instead of being inflated under the whole budget.
    let cicp_decides = format == ImageFormat::Png && header::png_colour(bytes).cicp.is_some();
    let mut input = reader(bytes).map_err(|error| unreadable(field, &error))?;
    input.limits(if cicp_decides { limits_with_alloc(max_profile_bytes()) } else { limits() });
    let mut decoder = input.into_decoder().map_err(|error| unreadable(field, &error))?;
    let icc = if cicp_decides { None } else { decoder.icc_profile().map_err(|error| unreadable(field, &error))? };
    // The decoder yields no profile for APP2 chunks it cannot put together (a sequence number out of range, a count that
    // disagrees). That is a file that declared a profile and gave none: refused, not read as untagged sRGB.
    if declared.is_some() && icc.is_none() {
        return Err(refuse_unreadable(
            field,
            "its embedded colour profile is incomplete or its chunks disagree".to_owned(),
        ));
    }
    if !profile_fits(&plan, icc.as_ref().map_or(0, |p| p.len() as u64)) {
        return Err(too_large(field, plan.width, plan.height));
    }
    if let Some(found) = header::non_srgb(bytes, format, icc.as_deref()) {
        return Err(colour_unsupported(field, &found));
    }
    drop(icc);
    let dynamic = DynamicImage::from_decoder(decoder).map_err(|error| match error {
        ImageError::Limits(_) => too_large(field, plan.width, plan.height),
        other => unreadable(field, &other),
    })?;
    let rgba = dynamic.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Rgba8 { width, height, pixels: rgba.into_raw(), hash })
}

/// Whether an embedded profile of `profile` bytes keeps both phases of the decode within the cap. The phases are
/// separate in time. First the profile is copied out: the decoder holds its own copy and `icc_profile` returns the
/// other, with the encoded input and the decoder's state live and no pixel buffer yet. Then the returned copy is
/// dropped and the pixels are decoded with the decoder's copy still held: the plan's peak plus one profile. Each phase
/// is checked on its own, and the profile counts twice only in the first.
fn profile_fits(plan: &Plan, profile: u64) -> bool {
    let copy_phase = plan.input + DECODER_STATE_BYTES + 2 * profile;
    let decode_phase = plan.peak + profile;
    fits(plan.width, plan.height, copy_phase.max(decode_phase))
}

/// The predicted peak of decoding `bytes`, from the header alone: the decoder's scratch, its output and the RGBA8 the
/// measure keeps. `None` when the header cannot be read or the format is not PNG or JPEG. [`decode`] refuses an image
/// whose prediction is over [`MAX_DECODED_MIB`], and the budget test checks that the measured peak never exceeds it.
pub fn predicted_peak(bytes: &[u8]) -> Option<u64> {
    let format = guess(bytes).ok()?;
    plan(bytes, format).ok().map(|plan| plan.peak)
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

/// What the header says about the decode: the size, and the peak the decoder and the measure hold together.
struct Plan {
    width: u32,
    height: u32,
    /// The bytes the encoded input takes while the decoder is built and runs: the caller's buffer, and for a JPEG the
    /// copy the decoder keeps of all of it.
    input: u64,
    peak: u64,
}

/// The plan for an image of a known format, from its header only.
fn plan(bytes: &[u8], format: ImageFormat) -> Result<Plan, String> {
    let (width, height, output, scratch) = if format == ImageFormat::Jpeg {
        let frame = header::jpeg_frame(bytes).ok_or("its frame header is missing or cut short")?;
        // The JPEG decoder outputs one byte a pixel for a grey image and three for any colour image.
        let output = if frame.sampling.len() == 1 { 1 } else { 3 };
        (frame.width, frame.height, output, coefficient_bytes(&frame))
    } else {
        // The PNG decoder inflates the `iCCP` profile while it reads the header, so the decoder is built under a limit
        // on what it may allocate for it. The `png` crate does not report a profile past that limit, or a corrupt one:
        // it drops it, and the image would read as untagged sRGB. So a file that has the chunk and yields no profile is
        // refused here, not guessed at.
        let mut input = reader(bytes).map_err(|error| error.to_string())?;
        let mut limits = Limits::default();
        limits.max_alloc = Some(max_profile_bytes());
        input.limits(limits);
        let mut decoder = input.into_decoder().map_err(|error| error.to_string())?;
        // A `cICP` chunk decides the colour on its own (PNG colour chunk precedence), so a profile beside it is not looked at:
        // one the limit dropped does not make the file unreadable.
        let colour = header::png_colour(bytes);
        if colour.iccp && colour.cicp.is_none() && decoder.icc_profile().map_err(|error| error.to_string())?.is_none() {
            return Err(format!(
                "its embedded colour profile is corrupt or inflates to more than {} MiB",
                max_profile_bytes() >> 20
            ));
        }
        let (width, height) = decoder.dimensions();
        let output = u64::from(decoder.color_type().bytes_per_pixel());
        // The PNG decoder fills a frame-sized scratch buffer alongside its output, and keeps the rows it filters
        // against, which grow with the width.
        let scratch = u64::from(width) * u64::from(height) * output + 4 * u64::from(width) * output;
        (width, height, output, scratch)
    };
    let pixels = u64::from(width) * u64::from(height);
    // The encoded input is held through the decode too, so it is part of the peak. The JPEG decoder reads all of its
    // input into a buffer of its own, so a JPEG holds it twice.
    let copies = if format == ImageFormat::Jpeg { 2 } else { 1 };
    let input = copies * bytes.len() as u64;
    let peak = DECODER_STATE_BYTES + scratch + pixels * (output + RGBA_BYTES) + input;
    Ok(Plan { width, height, input, peak })
}

/// The coefficient buffers a JPEG decoder keeps for its frame: one block of [`COEFFICIENT_BLOCK_BYTES`] for each 8×8
/// block of every component, on whole MCUs. Counted for baseline frames too, which is conservative for them.
fn coefficient_bytes(frame: &JpegFrame) -> u64 {
    let (h_max, v_max) = frame.sampling.iter().fold((1, 1), |(h, v), &(h_i, v_i)| (h.max(h_i), v.max(v_i)));
    let mcus_x = u64::from(frame.width).div_ceil(8 * u64::from(h_max));
    let mcus_y = u64::from(frame.height).div_ceil(8 * u64::from(v_max));
    let blocks: u64 = frame.sampling.iter().map(|&(h, v)| mcus_x * u64::from(h) * mcus_y * u64::from(v)).sum();
    blocks * COEFFICIENT_BLOCK_BYTES
}

/// A reader over the bytes, with the format guessed from them. Only the PNG and JPEG decoders are compiled in.
fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, ImageError> {
    Ok(ImageReader::new(Cursor::new(bytes)).with_guessed_format()?)
}

/// The decoder's own limits, the same caps the header check applies.
fn limits() -> Limits {
    limits_with_alloc(max_decoded_bytes())
}

/// The decoder's limits with `max_alloc` as the most it may allocate for its own data.
fn limits_with_alloc(max_alloc: u64) -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(max_alloc);
    limits
}

/// Whether an image of this size is within the caps: the sides, and its peak in bytes.
fn fits(width: u32, height: u32, peak: u64) -> bool {
    width <= MAX_SIDE && height <= MAX_SIDE && peak <= max_decoded_bytes()
}

/// Refuses an image larger than the caps, naming its size and the caps.
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

#[cfg(test)]
mod tests {
    use super::{MAX_SIDE, Plan, max_decoded_bytes, profile_fits};

    const MIB: u64 = 1 << 20;

    fn plan(peak: u64) -> Plan {
        Plan { width: 1, height: 1, input: 1_000, peak }
    }

    #[test]
    fn a_profile_counts_twice_only_while_it_is_copied() {
        let cap = max_decoded_bytes();
        // Small image: the copy phase is the larger. Two copies of 32 MiB are exactly the cap and a byte more is over,
        // though the decode phase (one copy over the plan) fits easily.
        let input = 1_000;
        let state = super::DECODER_STATE_BYTES;
        let exact = (cap - input - state) / 2;
        assert!(profile_fits(&plan(input + state + 5 * MIB), exact));
        assert!(!profile_fits(&plan(input + state + 5 * MIB), exact + 1));
    }

    #[test]
    fn the_copy_phase_does_not_carry_the_pixel_buffers_of_the_plan() {
        // A plan of 44 MiB with a 12 MiB profile: the old single sum was 44 + 24 = 68 MiB and refused it, but the copy
        // phase holds only the input and the decoder state beside the two profiles (about 24 MiB) and the decode phase
        // 44 + 12 = 56 MiB. Both fit.
        assert!(profile_fits(&plan(44 * MIB), 12 * MIB));
    }

    #[test]
    fn the_decode_phase_is_refused_one_byte_over_the_cap() {
        let cap = max_decoded_bytes();
        let profile = 3 * MIB;
        assert!(profile_fits(&plan(cap - profile), profile));
        assert!(!profile_fits(&plan(cap - profile + 1), profile));
        // The side limit is unaffected by the profile.
        assert!(!profile_fits(&Plan { width: MAX_SIDE + 1, height: 1, input: 0, peak: 0 }, 0));
    }
}
