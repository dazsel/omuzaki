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
/// Huffman tables for a JPEG. Measured at about 45 KiB for the old PNG path and just over 64 KiB for the `png` decoder
/// used directly (its row buffers and window), and allowed for at 128 KiB.
const DECODER_STATE_BYTES: u64 = 128 << 10;

/// The most a PNG's profile may inflate to when the encoded input is `input_len` bytes: the bound every PNG gets, and no
/// more than a quarter of what the cap leaves beside the input and the decoder's state. Half of that is for the copy phase
/// (two copies of the profile), and the inflate buffer of the decoder grows by doubling, so it can briefly hold twice the
/// bound while it fills. A profile past the bound is dropped by the decoder, never retained.
///
/// `kept` is what the decoder holds of the file's other chunks while it reads the header.
///
/// `None` when the input leaves less than [`PROFILE_BOUND_FLOOR`], the least the decoder needs for its metadata (text
/// chunks): the image is refused as too large. A floor applied after the subtraction would hand back room the input has
/// already used.
fn profile_bound(input_len: u64, kept: u64) -> Option<u64> {
    let room =
        max_decoded_bytes().saturating_sub(input_len.saturating_add(DECODER_STATE_BYTES).saturating_add(kept)) / 4;
    let bound = max_profile_bytes().min(room);
    (bound >= PROFILE_BOUND_FLOOR).then_some(bound)
}

/// The least a decoder is given for its metadata: below this the image is refused, never decoded.
const PROFILE_BOUND_FLOOR: u64 = 1 << 20;

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
/// A PNG is read with the `png` decoder itself, and its colour is taken only from that decoder's validated metadata, so a
/// chunk the decoder ignores is ignored here too. Its text chunks are not parsed at all: they carry no colour, and each
/// parsed record costs memory the allocation limit does not count.
///
/// This is the decode [`load`] runs, public so the allocation budget can be measured on its own (`tests/budget.rs`).
///
/// # Errors
/// The `Invalid` refusals of [`load`]; `hash` is recorded as the image's name in the result.
pub fn decode(bytes: &[u8], hash: ContentHash, field: &str) -> Result<Rgba8, OpError> {
    let format = guess(bytes).map_err(|reason| refuse_unreadable(field, reason))?;
    if format == ImageFormat::Png { decode_png(bytes, hash, field) } else { decode_jpeg(bytes, hash, field) }
}

/// The `png` decoder over `bytes`, its header read. Text chunks are skipped. Palette and transparency are expanded; a
/// 16-bit sample is kept as it is. `bound` caps what the decoder allocates for its own data (the inflated profile).
/// `with_profile` false skips the `iCCP` chunk altogether, for the pixel pass, which has already judged the colour.
fn open_png(bytes: &[u8], bound: u64, with_profile: bool) -> Result<png::Reader<Cursor<&[u8]>>, png::DecodingError> {
    let limits = png::Limits { bytes: usize::try_from(bound).unwrap_or(usize::MAX) };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(!with_profile);
    decoder.set_transformations(png::Transformations::EXPAND);
    decoder.read_info()
}

/// A PNG's header as the decoder reads it: what the decode will hold, and what its colour is.
struct PngHeader {
    plan: Plan,
    /// What the file declares when it is not sRGB.
    non_srgb: Option<String>,
}

/// Reads a PNG's header with the decoder, profile included, and plans the decode from it.
///
/// A scan of the chunk headers first finds out what is there, and decides no colour. The locked `png` keeps an `eXIf`
/// chunk twice (the chunk buffer and the clone it stores) and has no setting to skip it, so twice its length is counted
/// in the peak, as the budget counts everything the decoder holds, rather than refusing the EXIF. It also buffers a whole
/// `iCCP` chunk before it inflates it, which is counted while the header is read.
fn png_header(bytes: &[u8]) -> Result<PngHeader, String> {
    let input = bytes.len() as u64;
    let scan = header::png_scan(bytes);
    let exif = 2 * scan.exif_bytes;
    let Some(bound) = profile_bound(input, exif + scan.iccp_bytes) else {
        // The input and what the decoder keeps of it leave no room: the peak is over the cap whatever the pixels are, so
        // the caller refuses it as too large. The size is read from the IHDR chunk, not through a decoder, which would
        // inflate the profile it was refused room for.
        let (width, height) = header::png_size(bytes).ok_or("its header is cut short")?;
        return Ok(PngHeader { plan: Plan { width, height, input, peak: max_decoded_bytes() + 1 }, non_srgb: None });
    };
    let reader = open_png(bytes, bound, true).map_err(|error| error.to_string())?;
    let info = reader.info();
    // The decoder drops a profile past its bound, or one that is out of order or will not inflate, without an error. A
    // file that has an `iCCP` chunk and yields no profile has colour this check cannot read: refused, never read as
    // untagged. Only a `cICP` chunk the decoder itself accepted decides the colour anyway (PNG colour chunk precedence).
    if scan.iccp && info.icc_profile.is_none() && info.coding_independent_code_points.is_none() {
        return Err(format!(
            "its embedded colour profile is present but unreadable: it is corrupt, out of order or inflates to more than {} MiB",
            (bound + (1 << 19)) >> 20
        ));
    }
    let (width, height) = (info.width, info.height);
    let (color, depth) = reader.output_color_type();
    let output = (color.samples() * (depth as usize / 8)) as u64;
    // The PNG decoder fills a frame-sized scratch buffer alongside its output, and keeps the rows it filters against,
    // which grow with the width.
    let scratch = u64::from(width) * u64::from(height) * output + 4 * u64::from(width) * output;
    let pixels = u64::from(width) * u64::from(height);
    // The encoded input is held through the decode too, so it is part of the peak, and so is the EXIF the decoder keeps.
    // The profile is not: the pixel pass skips it, and the reader that held it is dropped before that pass opens.
    let peak = DECODER_STATE_BYTES + scratch + pixels * (output + RGBA_BYTES) + input + exif;
    Ok(PngHeader { plan: Plan { width, height, input, peak }, non_srgb: header::png_non_srgb(info) })
}

fn decode_png(bytes: &[u8], hash: ContentHash, field: &str) -> Result<Rgba8, OpError> {
    let PngHeader { plan, non_srgb } = png_header(bytes).map_err(|reason| refuse_unreadable(field, reason))?;
    if !fits(plan.width, plan.height, plan.peak) {
        return Err(too_large(field, plan.width, plan.height));
    }
    if let Some(found) = non_srgb {
        return Err(colour_unsupported(field, &found));
    }
    let png_error = |error: png::DecodingError| match error {
        png::DecodingError::LimitsExceeded => too_large(field, plan.width, plan.height),
        other => refuse_unreadable(field, other.to_string()),
    };
    let mut reader = open_png(bytes, max_decoded_bytes(), false).map_err(png_error)?;
    let size = reader.output_buffer_size().ok_or_else(|| too_large(field, plan.width, plan.height))?;
    let mut frame = vec![0_u8; size];
    let output = reader.next_frame(&mut frame).map_err(png_error)?;
    let pixels = rgba8(&frame[..output.buffer_size()], output.color_type, output.bit_depth)
        .ok_or_else(|| refuse_unreadable(field, "its pixel layout is not one a measure reads".to_owned()))?;
    Ok(Rgba8 { width: output.width, height: output.height, pixels, hash })
}

/// The straight RGBA8 of a decoded PNG frame: grey and grey with alpha are widened, an opaque pixel gets alpha 255, and a
/// 16-bit sample is rounded to 8 bits (`(v + 128) / 257`, which maps 0 and 65 535 to 0 and 255).
fn rgba8(frame: &[u8], color: png::ColorType, depth: png::BitDepth) -> Option<Vec<u8>> {
    use png::ColorType::{Grayscale, GrayscaleAlpha, Rgb, Rgba};
    let wide = match depth {
        png::BitDepth::Eight => false,
        png::BitDepth::Sixteen => true,
        _ => return None,
    };
    let sample = |chunk: &[u8]| -> u8 {
        if wide {
            u8::try_from((u32::from(u16::from_be_bytes([chunk[0], chunk[1]])) + 128) / 257).unwrap_or(u8::MAX)
        } else {
            chunk[0]
        }
    };
    let step = if wide { 2 } else { 1 };
    let mut out = Vec::with_capacity(frame.len() / (color.samples() * step) * 4);
    for pixel in frame.chunks_exact(color.samples() * step) {
        let mut channels = pixel.chunks_exact(step).map(sample);
        match color {
            Grayscale | GrayscaleAlpha => {
                let grey = channels.next()?;
                out.extend([grey, grey, grey, channels.next().unwrap_or(u8::MAX)]);
            }
            Rgb | Rgba => {
                out.extend([channels.next()?, channels.next()?, channels.next()?, channels.next().unwrap_or(u8::MAX)]);
            }
            png::ColorType::Indexed => return None,
        }
    }
    Some(out)
}

fn decode_jpeg(bytes: &[u8], hash: ContentHash, field: &str) -> Result<Rgba8, OpError> {
    let plan = plan_jpeg(bytes).map_err(|reason| refuse_unreadable(field, reason))?;
    if !fits(plan.width, plan.height, plan.peak) {
        return Err(too_large(field, plan.width, plan.height));
    }
    // A JPEG decoder keeps the profile it finds while it is built, so the declared length is judged before that, and
    // before `icc_profile` copies it.
    let declared = header::jpeg_icc_declared_len(bytes);
    if let Some(declared) = declared
        && !profile_fits(&plan, declared)
    {
        return Err(too_large(field, plan.width, plan.height));
    }
    let mut input = reader(bytes).map_err(|error| unreadable(field, &error))?;
    input.limits(limits());
    let mut decoder = input.into_decoder().map_err(|error| unreadable(field, &error))?;
    let icc = decoder.icc_profile().map_err(|error| unreadable(field, &error))?;
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
    if let Some(found) = header::jpeg_non_srgb(bytes, icc.as_deref()) {
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

/// Whether an embedded JPEG profile of `profile` bytes keeps both phases of the decode within the cap. The phases are
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
    match guess(bytes).ok()? {
        ImageFormat::Png => png_header(bytes).ok().map(|header| header.plan.peak),
        _ => plan_jpeg(bytes).ok().map(|plan| plan.peak),
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

/// What the header says about the decode: the size, and the peak the decoder and the measure hold together.
struct Plan {
    width: u32,
    height: u32,
    /// The bytes the encoded input takes while the decoder is built and runs: the caller's buffer, and for a JPEG the
    /// copy the decoder keeps of all of it.
    input: u64,
    peak: u64,
}

/// The plan for a JPEG, from its header only.
fn plan_jpeg(bytes: &[u8]) -> Result<Plan, String> {
    let frame = header::jpeg_frame(bytes).ok_or("its frame header is missing or cut short")?;
    // The JPEG decoder outputs one byte a pixel for a grey image and three for any colour image.
    let output = if frame.sampling.len() == 1 { 1 } else { 3 };
    let scratch = coefficient_bytes(&frame);
    let pixels = u64::from(frame.width) * u64::from(frame.height);
    // The encoded input is held through the decode too. The JPEG decoder reads all of its input into a buffer of its own,
    // so a JPEG holds it twice.
    let input = 2 * bytes.len() as u64;
    let peak = DECODER_STATE_BYTES + scratch + pixels * (output + RGBA_BYTES) + input;
    Ok(Plan { width: frame.width, height: frame.height, input, peak })
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
