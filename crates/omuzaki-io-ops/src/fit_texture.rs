//! Resizing textures for a platform export (spec v3 §27 EX-03): [`resize`] is the resampler a platform fit uses, and
//! [`downscale_to`] is the step that shrinks a texture only when it is over the budget.
//!
//! Each output texel is the area average of the source texels it covers, weighted by how much of each one it covers.
//! The average runs where the numbers mean what they say:
//!
//! - colour ([`TextureUse::Colour`], sRGB) is converted to linear light, averaged there and encoded back. A black and
//!   white checkerboard averages to mid grey (188), where averaging the encoded values would give a dark 128.
//! - data ([`TextureUse::Data`]: normal, mask and roughness maps) is averaged as stored, because its values are
//!   numbers, not light.
//! - alpha is averaged, and colour is premultiplied by alpha while it is averaged. A transparent texel's colour then
//!   cannot darken the edge of an opaque one. An output texel that is entirely transparent takes the plain average
//!   colour of what it covers, so a filter that reads it does not pick up black.
//!
//! The sRGB curve is two tables of constants, generated once from the sRGB formula and rounded to 16-bit steps. The
//! rest is IEEE `f64` arithmetic in a fixed order, with no maths-library call on the path, so the same input gives the
//! same bytes on every target. The output is a PNG with fixed encoder settings.
//!
//! Before any pixel is decoded, the input's PNG chunks are walked from the signature to `IEND`, and each chunk's length
//! and CRC are checked. The header's size is checked before any decoder is built.

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, ImageError, ImageFormat, ImageReader, Limits, RgbaImage};
use omuzaki_io_api::{Blob, EncodedImage, ImageMime, IoError};

/// What a texture's pixels mean, which decides how they are averaged.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TextureUse {
    /// Colour in sRGB (base colour, emissive, matcap, rim): averaged in linear light.
    Colour,
    /// Values that are numbers, not light (normal, mask, roughness): averaged as stored.
    Data,
}

/// The largest side a texture may have, in pixels. The same bound `convert::texture` enforces when it reads a texture.
const MAX_SIDE: u32 = 16_384;

/// The most pixels a texture may have. At 4 bytes each that is 256 MiB of output, which bounds what a resize allocates.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// What the decoder may allocate for one image: room for a 16-bit RGBA decode of the largest allowed image.
const MAX_DECODE_BYTES: u64 = 1 << 30;

/// The eight bytes every PNG starts with.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The sRGB value of each 8-bit stored value, as a linear-light step out of 65535. Generated from the sRGB formula.
const SRGB_TO_LINEAR: [u16; 256] = [
    0, 20, 40, 60, 80, 99, 119, 139, 159, 179, 199, 219, 241, 264, 288, 313, 340, 367, 396, 427, 458, 491, 526, 562,
    599, 637, 677, 718, 761, 805, 851, 898, 947, 997, 1048, 1101, 1156, 1212, 1270, 1330, 1391, 1453, 1517, 1583, 1651,
    1720, 1790, 1863, 1937, 2013, 2090, 2170, 2250, 2333, 2418, 2504, 2592, 2681, 2773, 2866, 2961, 3058, 3157, 3258,
    3360, 3464, 3570, 3678, 3788, 3900, 4014, 4129, 4247, 4366, 4488, 4611, 4736, 4864, 4993, 5124, 5257, 5392, 5530,
    5669, 5810, 5953, 6099, 6246, 6395, 6547, 6700, 6856, 7014, 7174, 7335, 7500, 7666, 7834, 8004, 8177, 8352, 8528,
    8708, 8889, 9072, 9258, 9445, 9635, 9828, 10022, 10219, 10417, 10619, 10822, 11028, 11235, 11446, 11658, 11873,
    12090, 12309, 12530, 12754, 12980, 13209, 13440, 13673, 13909, 14146, 14387, 14629, 14874, 15122, 15371, 15623,
    15878, 16135, 16394, 16656, 16920, 17187, 17456, 17727, 18001, 18277, 18556, 18837, 19121, 19407, 19696, 19987,
    20281, 20577, 20876, 21177, 21481, 21787, 22096, 22407, 22721, 23038, 23357, 23678, 24002, 24329, 24658, 24990,
    25325, 25662, 26001, 26344, 26688, 27036, 27386, 27739, 28094, 28452, 28813, 29176, 29542, 29911, 30282, 30656,
    31033, 31412, 31794, 32179, 32567, 32957, 33350, 33745, 34143, 34544, 34948, 35355, 35764, 36176, 36591, 37008,
    37429, 37852, 38278, 38706, 39138, 39572, 40009, 40449, 40891, 41337, 41785, 42236, 42690, 43147, 43606, 44069,
    44534, 45002, 45473, 45947, 46423, 46903, 47385, 47871, 48359, 48850, 49344, 49841, 50341, 50844, 51349, 51858,
    52369, 52884, 53401, 53921, 54445, 54971, 55500, 56032, 56567, 57105, 57646, 58190, 58737, 59287, 59840, 60396,
    60955, 61517, 62082, 62650, 63221, 63795, 64372, 64952, 65535,
];

/// For each sRGB byte from 1 to 255, the smallest linear-light step (out of 65535) at which it rounds up to that byte.
/// Encoding a step is the count of these at or below it.
const LINEAR_TO_SRGB: [u16; 255] = [
    10, 30, 50, 70, 90, 110, 130, 150, 170, 189, 209, 230, 253, 276, 301, 327, 354, 382, 412, 443, 475, 509, 544, 580,
    618, 657, 698, 740, 783, 828, 875, 923, 972, 1023, 1075, 1129, 1185, 1242, 1300, 1360, 1422, 1486, 1551, 1617,
    1685, 1755, 1827, 1900, 1975, 2052, 2130, 2210, 2292, 2376, 2461, 2548, 2637, 2727, 2820, 2914, 3010, 3108, 3208,
    3309, 3412, 3518, 3625, 3734, 3844, 3957, 4072, 4188, 4307, 4427, 4550, 4674, 4800, 4928, 5059, 5191, 5325, 5461,
    5599, 5740, 5882, 6026, 6173, 6321, 6471, 6624, 6778, 6935, 7094, 7255, 7418, 7583, 7750, 7919, 8091, 8265, 8440,
    8618, 8798, 8981, 9165, 9352, 9541, 9732, 9925, 10121, 10318, 10518, 10720, 10925, 11132, 11341, 11552, 11765,
    11981, 12199, 12420, 12643, 12868, 13095, 13325, 13557, 13791, 14028, 14267, 14508, 14752, 14998, 15247, 15498,
    15751, 16007, 16265, 16525, 16788, 17054, 17321, 17592, 17864, 18139, 18417, 18697, 18980, 19264, 19552, 19842,
    20134, 20429, 20727, 21027, 21329, 21634, 21942, 22252, 22564, 22880, 23197, 23518, 23840, 24166, 24494, 24824,
    25158, 25493, 25832, 26173, 26516, 26862, 27211, 27563, 27917, 28273, 28633, 28995, 29359, 29727, 30097, 30469,
    30845, 31223, 31603, 31987, 32373, 32762, 33153, 33547, 33944, 34344, 34747, 35152, 35560, 35970, 36384, 36800,
    37219, 37640, 38065, 38492, 38922, 39355, 39790, 40229, 40670, 41114, 41561, 42011, 42463, 42918, 43377, 43838,
    44301, 44768, 45238, 45710, 46185, 46663, 47144, 47628, 48115, 48605, 49097, 49593, 50091, 50592, 51096, 51604,
    52114, 52627, 53142, 53661, 54183, 54708, 55235, 55766, 56300, 56836, 57376, 57918, 58464, 59012, 59564, 60118,
    60675, 61236, 61799, 62366, 62935, 63508, 64083, 64662, 65244,
];

/// Resizes `image` to `width` by `height` and returns it as a PNG, whatever format it came in.
///
/// Area averaging: each output texel covers its share of the source texels, as described in the module docs. A target
/// the same size as the source keeps every texel as it is. The header's size is checked before any pixel is decoded, so
/// a header that declares an oversized image is refused without a decoder being built.
///
/// # Errors
/// [`IoError::Limit`] if the source or the target is over 16,384 pixels a side or 64 megapixels;
/// [`IoError::Malformed`] if the image is truncated, damaged or cannot be read; [`IoError::Unsupported`] for a zero-sized
/// target or a format this build cannot decode (JPEG); [`IoError::Internal`] if the PNG encoder fails.
pub fn resize(image: &EncodedImage, width: u32, height: u32, usage: TextureUse) -> Result<EncodedImage, IoError> {
    check_size(width, height)?;
    let size = source_size(image)?;
    resample(image, size, width, height, usage)
}

/// Shrinks `image` so its longer side is `max_side`, keeping the aspect ratio (the shorter side is rounded to the
/// nearest pixel and is at least 1). Returns `None` when the image already fits, after decoding all of it to check it.
///
/// # Errors
/// As [`resize`]; a `max_side` of zero is [`IoError::Unsupported`].
pub fn downscale_to(image: &EncodedImage, max_side: u32, usage: TextureUse) -> Result<Option<EncodedImage>, IoError> {
    // Refused before the image is read at all: a zero budget has no output, whatever the input is.
    if max_side == 0 {
        return Err(empty_target());
    }
    let size = source_size(image)?;
    let (width, height) = size;
    if width.max(height) <= max_side {
        // Nothing to resample, but the bytes must still be the image they claim to be: a truncated payload is an error
        // here too, not a pass-through.
        decode(image, size)?;
        return Ok(None);
    }
    let (target_width, target_height) = if width >= height {
        (max_side, scaled_side(height, max_side, width))
    } else {
        (scaled_side(width, max_side, height), max_side)
    };
    resample(image, size, target_width, target_height, usage).map(Some)
}

/// The error for a target with no pixels, which is the caller's fault and is refused before any input is read.
fn empty_target() -> IoError {
    IoError::Unsupported {
        what: "a texture with no pixels".to_owned(),
        hint: Some("ask for at least one pixel on each side".to_owned()),
    }
}

/// The side that keeps the aspect: `short` scaled by `to / long`, rounded to the nearest whole pixel, and at least one.
#[allow(clippy::cast_possible_truncation)] // `short * to / long` is at most `to`, which is a `u32`
fn scaled_side(short: u32, to: u32, long: u32) -> u32 {
    let scaled = (u64::from(short) * u64::from(to) + u64::from(long) / 2) / u64::from(long);
    (scaled as u32).max(1)
}

/// Refuses a target that is empty or too large. Every output allocation goes through this.
fn check_size(width: u32, height: u32) -> Result<(), IoError> {
    if width == 0 || height == 0 {
        return Err(empty_target());
    }
    for side in [width, height] {
        if side > MAX_SIDE {
            return Err(IoError::Limit { what: "texture side", limit: u64::from(MAX_SIDE), got: u64::from(side) });
        }
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(IoError::Limit { what: "texture pixels", limit: MAX_PIXELS, got: pixels });
    }
    Ok(())
}

/// The size declared by the header of `image`, checked against the limits, with no decoder built. JPEG is refused here
/// because this build has no JPEG decoder, and a decoder is the only way to read a JPEG's size.
fn source_size(image: &EncodedImage) -> Result<(u32, u32), IoError> {
    let size = match image.mime {
        ImageMime::Png => png_header(&image.name, image.bytes.as_slice())?,
        ImageMime::Jpeg => {
            return Err(IoError::Unsupported {
                what: format!("image '{}': JPEG decoding isn't built into this release", image.name),
                hint: Some("export the texture as PNG".to_owned()),
            });
        }
    };
    check_size(size.0, size.1)?;
    Ok(size)
}

/// The width and height in the IHDR chunk of a PNG, with that chunk's CRC checked. Only the first chunk is read.
fn png_header(name: &str, bytes: &[u8]) -> Result<(u32, u32), IoError> {
    let start = PNG_SIGNATURE.len();
    if !bytes.starts_with(&PNG_SIGNATURE) {
        return Err(malformed(name, "not a PNG (the signature is wrong)"));
    }
    let (kind, data, _) = next_chunk(name, bytes, start)?;
    if &kind != b"IHDR" {
        return Err(malformed(name, "the first chunk isn't IHDR"));
    }
    let size = header_size(data).ok_or_else(|| malformed(name, "IHDR is not 13 bytes"))?;
    if size.0 == 0 || size.1 == 0 {
        return Err(malformed(name, "the header declares a zero-sized image"));
    }
    Ok(size)
}

/// The width and height in IHDR data, which is 13 bytes: two big-endian `u32`s, then five one-byte fields.
fn header_size(data: &[u8]) -> Option<(u32, u32)> {
    let [w0, w1, w2, w3, h0, h1, h2, h3, _, _, _, _, _] = *data else {
        return None;
    };
    Some((u32::from_be_bytes([w0, w1, w2, w3]), u32::from_be_bytes([h0, h1, h2, h3])))
}

/// Walks every chunk of the PNG in `bytes`, checking each length and CRC, and that the chunks are in order: IHDR first,
/// at least one IDAT, IEND last. Bytes after IEND are ignored, as PNG readers ignore them.
fn validate_png(name: &str, bytes: &[u8]) -> Result<(), IoError> {
    let mut at = PNG_SIGNATURE.len();
    let mut first = true;
    let mut seen_data = false;
    loop {
        let (kind, data, end) = next_chunk(name, bytes, at)?;
        if first && &kind != b"IHDR" {
            return Err(malformed(name, "the first chunk isn't IHDR"));
        }
        first = false;
        match &kind {
            b"IDAT" => seen_data = true,
            b"IEND" if !data.is_empty() => return Err(malformed(name, "IEND has data")),
            b"IEND" if !seen_data => {
                return Err(malformed(name, "there is no image data (no IDAT chunk) before IEND"));
            }
            b"IEND" => return Ok(()),
            _ => {}
        }
        at = end;
    }
}

/// The chunk that starts at `at`: its type, its data and the offset just past its CRC. Fails if the chunk runs past the
/// end of `bytes` or its CRC does not match.
fn next_chunk<'a>(name: &str, bytes: &'a [u8], at: usize) -> Result<([u8; 4], &'a [u8], usize), IoError> {
    let Some(head) = bytes.get(at..at + 8) else {
        return Err(malformed(name, "the file ends before its IEND chunk"));
    };
    let [l0, l1, l2, l3, k0, k1, k2, k3] = *head else {
        return Err(malformed(name, "the file ends before its IEND chunk"));
    };
    let length = u64::from(u32::from_be_bytes([l0, l1, l2, l3]));
    let end = u64::try_from(at).unwrap_or(u64::MAX).saturating_add(12 + length);
    let Some(stop) = usize::try_from(end).ok().filter(|&stop| stop <= bytes.len()) else {
        return Err(malformed(name, "a chunk runs past the end of the file"));
    };
    let kind = [k0, k1, k2, k3];
    let data = &bytes[at + 8..stop - 4];
    let stored = u32::from_be_bytes([bytes[stop - 4], bytes[stop - 3], bytes[stop - 2], bytes[stop - 1]]);
    if crc32(&bytes[at + 4..stop - 4]) != stored {
        return Err(malformed(name, &format!("the CRC of a {} chunk doesn't match", String::from_utf8_lossy(&kind))));
    }
    Ok((kind, data, stop))
}

/// The CRC-32 PNG chunks carry (ISO 3309, reflected polynomial), table-driven.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc = CRC_TABLE[usize::from(((crc ^ u32::from(byte)) & 0xFF) as u8)] ^ (crc >> 8);
    }
    !crc
}

/// The table [`crc32`] steps through: the CRC of every byte value.
const CRC_TABLE: [u32; 256] = crc_table();

/// Builds [`CRC_TABLE`] at compile time.
#[allow(clippy::cast_possible_truncation)] // `n` is below 256
const fn crc_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut crc = n as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            bit += 1;
        }
        table[n] = crc;
        n += 1;
    }
    table
}

/// An error that names the image and says what is wrong with it.
fn malformed(name: &str, what: &str) -> IoError {
    IoError::Malformed { message: format!("image '{name}': {what}"), at: None }
}

/// Decodes `image` to 8-bit RGBA, after validating its chunks through `IEND`. Sixteen-bit samples are reduced to eight,
/// as the image crate does. `size` is the header's size, which the decoder's allocation is bounded by.
fn decode(image: &EncodedImage, size: (u32, u32)) -> Result<RgbaImage, IoError> {
    validate_png(&image.name, image.bytes.as_slice())?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    let mut reader = ImageReader::with_format(std::io::Cursor::new(image.bytes.as_slice()), ImageFormat::Png);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| match e {
        ImageError::Limits(_) => IoError::Limit {
            what: "decoded texture memory",
            limit: MAX_DECODE_BYTES,
            // Sixteen-bit RGBA is the most a decode of this size can need.
            got: u64::from(size.0) * u64::from(size.1) * 8,
        },
        ImageError::Unsupported(_) => IoError::Unsupported { what: format!("image '{}': {e}", image.name), hint: None },
        other => malformed(&image.name, &other.to_string()),
    })?;
    Ok(decoded.into_rgba8())
}

/// Encodes RGBA8 `pixels` as a PNG with fixed settings, so the same pixels always give the same bytes.
fn encode(name: &str, width: u32, height: u32, pixels: &[u8]) -> Result<EncodedImage, IoError> {
    let mut bytes = Vec::new();
    PngEncoder::new_with_quality(&mut bytes, CompressionType::Default, FilterType::Adaptive)
        .write_image(pixels, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| IoError::Internal(format!("texture '{name}' can't be written as PNG: {e}")))?;
    Ok(EncodedImage::new(name, ImageMime::Png, Blob::new(bytes)))
}

/// Decodes `image`, averages it to `width` by `height` and encodes the result. `size` is the source's header size.
fn resample(
    image: &EncodedImage,
    size: (u32, u32),
    width: u32,
    height: u32,
    usage: TextureUse,
) -> Result<EncodedImage, IoError> {
    // Every caller checks the target first; a zero side here would reach the encoder as an empty image.
    check_size(width, height)?;
    let source = decode(image, size)?;
    let pixels =
        if source.dimensions() == (width, height) { source.into_raw() } else { average(&source, width, height, usage) };
    encode(&image.name, width, height, &pixels)
}

/// The working value of a stored channel value: linear light for colour, the stored fraction for data.
fn load(usage: TextureUse, stored: u8) -> f64 {
    match usage {
        TextureUse::Colour => f64::from(SRGB_TO_LINEAR[usize::from(stored)]) / 65535.0,
        TextureUse::Data => f64::from(stored) / 255.0,
    }
}

/// The stored value of a working value, clamped to the channel's range.
fn store(usage: TextureUse, working: f64) -> u8 {
    match usage {
        TextureUse::Colour => encode_linear(to_steps(working)),
        TextureUse::Data => to_byte(working),
    }
}

/// The sRGB byte of a linear-light step: the number of thresholds at or below it.
fn encode_linear(step: u16) -> u8 {
    u8::try_from(LINEAR_TO_SRGB.partition_point(|&threshold| threshold <= step)).unwrap_or(u8::MAX)
}

/// Rounds a unit value to a byte, clamping it first. Halves round away from zero, so 127.5 is 128.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to `0..=255` first
fn to_byte(unit: f64) -> u8 {
    (unit.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Rounds a unit value to a 16-bit linear step, clamping it first.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to `0..=65535` first
fn to_steps(unit: f64) -> u16 {
    (unit.clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// One texel's contribution to an average: colour premultiplied by alpha, alpha, and the plain colour, which is what an
/// output texel with no alpha falls back to.
#[derive(Copy, Clone, Default)]
struct Sum {
    premultiplied: [f64; 3],
    alpha: f64,
    plain: [f64; 3],
}

impl Sum {
    /// The contribution of one stored texel.
    fn of_texel(pixel: &[u8], usage: TextureUse) -> Self {
        let alpha = f64::from(pixel[3]) / 255.0;
        let plain = [0, 1, 2].map(|c| load(usage, pixel[c]));
        Self { premultiplied: plain.map(|v| v * alpha), alpha, plain }
    }

    /// Adds `other` scaled by `weight`.
    fn add(&mut self, other: &Sum, weight: f64) {
        for c in 0..3 {
            self.premultiplied[c] += weight * other.premultiplied[c];
            self.plain[c] += weight * other.plain[c];
        }
        self.alpha += weight * other.alpha;
    }
}

/// The source texels one output texel covers: the first, and the weight of each of the consecutive ones from there.
struct Taps {
    first: u32,
    weights: Vec<f64>,
}

/// The taps of every output texel along one axis, going from `source` texels to `target`.
///
/// Output `i` covers `[i * source, (i + 1) * source)` in units of `1 / target` source texels, and source texel `j` covers
/// `[j * target, (j + 1) * target)` in the same units. Every overlap is a whole number, so the weights are exact
/// before the final division. The sides are at most [`MAX_SIDE`], so the products fit a `u32`.
fn taps(source: u32, target: u32) -> Vec<Taps> {
    (0..target)
        .map(|i| {
            let lo = i * source;
            let hi = lo + source;
            let first = lo / target;
            let end = hi.div_ceil(target);
            let weights = (first..end)
                .map(|j| {
                    let (start, stop) = (j * target, (j + 1) * target);
                    let overlap = stop.min(hi).saturating_sub(start.max(lo));
                    f64::from(overlap) / f64::from(source)
                })
                .collect();
            Taps { first, weights }
        })
        .collect()
}

/// Averages `source` to `width` by `height`, one output row at a time. Each source row is scaled horizontally into a
/// line, then that line is added to the output row with its vertical weight. Only two rows of sums are alive at once.
fn average(source: &RgbaImage, width: u32, height: u32, usage: TextureUse) -> Vec<u8> {
    let columns = taps(source.width(), width);
    let rows = taps(source.height(), height);
    let stride = source.width() as usize * 4;
    let raw = source.as_raw();
    let mut line = vec![Sum::default(); width as usize];
    let mut sums = vec![Sum::default(); width as usize];
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for row in &rows {
        sums.fill(Sum::default());
        for (k, &weight) in row.weights.iter().enumerate() {
            let y = row.first as usize + k;
            horizontal(&raw[y * stride..(y + 1) * stride], &columns, usage, &mut line);
            for (sum, scaled) in sums.iter_mut().zip(&line) {
                sum.add(scaled, weight);
            }
        }
        for sum in &sums {
            out.extend_from_slice(&finish(sum, usage));
        }
    }
    out
}

/// Scales one source row into `line`: each output column is the weighted sum of the texels it covers.
fn horizontal(src_row: &[u8], columns: &[Taps], usage: TextureUse, line: &mut [Sum]) {
    for (sum, column) in line.iter_mut().zip(columns) {
        *sum = Sum::default();
        for (k, &weight) in column.weights.iter().enumerate() {
            let x = column.first as usize + k;
            sum.add(&Sum::of_texel(&src_row[x * 4..x * 4 + 4], usage), weight);
        }
    }
}

/// The stored RGBA of one output texel. Colour is un-premultiplied by its average alpha; a texel with no alpha takes
/// the plain average colour instead.
fn finish(sum: &Sum, usage: TextureUse) -> [u8; 4] {
    let rgb = if sum.alpha > 0.0 { sum.premultiplied.map(|v| v / sum.alpha) } else { sum.plain };
    [store(usage, rgb[0]), store(usage, rgb[1]), store(usage, rgb[2]), to_byte(sum.alpha)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_weights_sum_to_one_for_every_output() {
        for (source, target) in [(4096, 1024), (1000, 341), (3, 7), (1, 1024), (16_384, 1), (5, 5)] {
            for tap in taps(source, target) {
                let sum: f64 = tap.weights.iter().sum();
                assert!((sum - 1.0).abs() < 1e-12, "{source} to {target}: weights sum to {sum}");
            }
        }
    }

    #[test]
    fn a_half_pixel_rounds_up_and_a_short_side_is_at_least_one() {
        assert_eq!(scaled_side(1000, 1024, 3000), 341);
        assert_eq!(scaled_side(1, 1024, 4000), 1);
        assert_eq!(to_byte(127.5 / 255.0), 128);
    }

    #[test]
    fn the_colour_tables_hold_their_endpoints_and_rise() {
        assert_eq!(SRGB_TO_LINEAR[0], 0);
        assert_eq!(SRGB_TO_LINEAR[255], 65535);
        assert!(SRGB_TO_LINEAR.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(LINEAR_TO_SRGB.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(encode_linear(0), 0);
        assert_eq!(encode_linear(65535), 255);
        for stored in 0..=u8::MAX {
            assert_eq!(encode_linear(SRGB_TO_LINEAR[usize::from(stored)]), stored, "round trip of {stored}");
        }
    }

    #[test]
    fn crc_matches_the_standard_check_value() {
        // The CRC-32 check value of "123456789" (ISO 3309) is a published constant.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
