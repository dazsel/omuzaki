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
//! The output is a PNG with fixed encoder settings. The arithmetic is `f64` in a fixed order. The sRGB curve comes from
//! `powf`, which can differ in its last bit between platforms; that can flip a value that lands exactly on a rounding
//! boundary, so bytes are guaranteed identical on one build, and across builds only as far as `powf` agrees.

use std::io::Cursor;

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

/// Resizes `image` to `width` by `height` and returns it as a PNG, whatever format it came in.
///
/// Area averaging: each output texel covers its share of the source texels, as described in the module docs. A target
/// the same size as the source keeps every texel as it is. The size is checked before any pixel is decoded, so a
/// header that declares an oversized image is refused without allocating for it.
///
/// # Errors
/// [`IoError::Limit`] if the source or the target is over 16,384 pixels a side or 64 megapixels;
/// [`IoError::Malformed`] if the image is truncated or cannot be read; [`IoError::Unsupported`] for a zero-sized target
/// or a format this build cannot decode; [`IoError::Internal`] if the PNG encoder fails.
pub fn resize(image: &EncodedImage, width: u32, height: u32, usage: TextureUse) -> Result<EncodedImage, IoError> {
    check_size(width, height)?;
    let source = decode(image)?;
    let pixels = if source.dimensions() == (width, height) {
        source.into_raw()
    } else {
        average(&source, width, height, &Transfer::new(usage))
    };
    encode(&image.name, width, height, &pixels)
}

/// Shrinks `image` so its longer side is `max_side`, keeping the aspect ratio (the shorter side is rounded to the
/// nearest pixel and is at least 1). Returns `None` when the image already fits.
///
/// # Errors
/// As [`resize`]; a `max_side` of zero is [`IoError::Unsupported`].
pub fn downscale_to(image: &EncodedImage, max_side: u32, usage: TextureUse) -> Result<Option<EncodedImage>, IoError> {
    let (width, height) = source_size(image)?;
    if width.max(height) <= max_side {
        return Ok(None);
    }
    let (target_width, target_height) = if width >= height {
        (max_side, scaled_side(height, max_side, width))
    } else {
        (scaled_side(width, max_side, height), max_side)
    };
    resize(image, target_width, target_height, usage).map(Some)
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
        return Err(IoError::Unsupported {
            what: "a texture with no pixels".to_owned(),
            hint: Some("ask for at least one pixel on each side".to_owned()),
        });
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

/// A reader over the encoded bytes of `image`, typed by its declared format.
fn reader(image: &EncodedImage) -> ImageReader<Cursor<&[u8]>> {
    let format = match image.mime {
        ImageMime::Png => ImageFormat::Png,
        ImageMime::Jpeg => ImageFormat::Jpeg,
    };
    ImageReader::with_format(Cursor::new(image.bytes.as_slice()), format)
}

/// The size declared by the header of `image`, read without decoding any pixels, and checked against the limits.
fn source_size(image: &EncodedImage) -> Result<(u32, u32), IoError> {
    let (width, height) = reader(image).into_dimensions().map_err(|e| read_error(&image.name, e))?;
    check_size(width, height)?;
    Ok((width, height))
}

/// Decodes `image` to 8-bit RGBA. Sixteen-bit samples are reduced to eight, as the image crate does.
fn decode(image: &EncodedImage) -> Result<RgbaImage, IoError> {
    let (width, height) = source_size(image)?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    let mut reader = reader(image);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| match e {
        ImageError::Limits(_) => IoError::Limit {
            what: "decoded texture memory",
            limit: MAX_DECODE_BYTES,
            // Sixteen-bit RGBA is the most a decode of this size can need.
            got: u64::from(width) * u64::from(height) * 8,
        },
        other => read_error(&image.name, other),
    })?;
    Ok(decoded.into_rgba8())
}

/// Maps an image crate error to the io error a caller sees: a format this build cannot decode is `Unsupported`, and
/// everything else (a truncated or damaged file) is `Malformed`.
fn read_error(name: &str, error: ImageError) -> IoError {
    match error {
        ImageError::Unsupported(_) => IoError::Unsupported { what: format!("image '{name}': {error}"), hint: None },
        other => IoError::Malformed { message: format!("image '{name}': {other}"), at: None },
    }
}

/// Encodes RGBA8 `pixels` as a PNG with fixed settings, so the same pixels always give the same bytes.
fn encode(name: &str, width: u32, height: u32, pixels: &[u8]) -> Result<EncodedImage, IoError> {
    let mut bytes = Vec::new();
    PngEncoder::new_with_quality(&mut bytes, CompressionType::Default, FilterType::Adaptive)
        .write_image(pixels, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| IoError::Internal(format!("texture '{name}' can't be written as PNG: {e}")))?;
    Ok(EncodedImage::new(name, ImageMime::Png, Blob::new(bytes)))
}

/// How a stored 8-bit channel value maps to the value the average runs in, and back.
struct Transfer {
    usage: TextureUse,
    /// The working value of each stored value, so decoding is a table lookup.
    load: [f64; 256],
    /// The stored value of each working value in 16-bit steps, for colour. Empty for data, which needs no curve.
    encoded: Vec<u8>,
}

impl Transfer {
    /// The tables for `usage`. Computed once per resize, not per texel.
    fn new(usage: TextureUse) -> Self {
        let mut load = [0.0; 256];
        for v in 0..=u8::MAX {
            let unit = f64::from(v) / 255.0;
            load[usize::from(v)] = match usage {
                TextureUse::Colour => srgb_to_linear(unit),
                TextureUse::Data => unit,
            };
        }
        let encoded = match usage {
            TextureUse::Colour => (0..=u16::MAX).map(|i| to_byte(linear_to_srgb(f64::from(i) / 65535.0))).collect(),
            TextureUse::Data => Vec::new(),
        };
        Self { usage, load, encoded }
    }

    /// The working value of a stored channel value.
    fn load(&self, stored: u8) -> f64 {
        self.load[usize::from(stored)]
    }

    /// The stored value of a working value, clamped to the channel's range.
    fn store(&self, working: f64) -> u8 {
        match self.usage {
            TextureUse::Colour => self.encoded[to_steps(working)],
            TextureUse::Data => to_byte(working),
        }
    }
}

/// The sRGB transfer function: a stored value in `0..=1` to linear light.
fn srgb_to_linear(encoded: f64) -> f64 {
    if encoded <= 0.04045 { encoded / 12.92 } else { ((encoded + 0.055) / 1.055).powf(2.4) }
}

/// The inverse of [`srgb_to_linear`]: linear light in `0..=1` to a sRGB value in `0..=1`.
fn linear_to_srgb(linear: f64) -> f64 {
    if linear <= 0.003_130_8 { linear * 12.92 } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 }
}

/// Rounds a unit value to a byte, clamping it first. Halves round away from zero, so 127.5 is 128.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to `0..=255` first
fn to_byte(unit: f64) -> u8 {
    (unit.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The index of a unit value in the 16-bit table, clamped.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to `0..=65535` first
fn to_steps(unit: f64) -> usize {
    (unit.clamp(0.0, 1.0) * 65535.0).round() as usize
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
    fn of_texel(pixel: &[u8], transfer: &Transfer) -> Self {
        let alpha = f64::from(pixel[3]) / 255.0;
        let plain = [0, 1, 2].map(|c| transfer.load(pixel[c]));
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
fn average(source: &RgbaImage, width: u32, height: u32, transfer: &Transfer) -> Vec<u8> {
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
            horizontal(&raw[y * stride..(y + 1) * stride], &columns, transfer, &mut line);
            for (sum, scaled) in sums.iter_mut().zip(&line) {
                sum.add(scaled, weight);
            }
        }
        for sum in &sums {
            out.extend_from_slice(&finish(sum, transfer));
        }
    }
    out
}

/// Scales one source row into `line`: each output column is the weighted sum of the texels it covers.
fn horizontal(src_row: &[u8], columns: &[Taps], transfer: &Transfer, line: &mut [Sum]) {
    for (sum, column) in line.iter_mut().zip(columns) {
        *sum = Sum::default();
        for (k, &weight) in column.weights.iter().enumerate() {
            let x = column.first as usize + k;
            sum.add(&Sum::of_texel(&src_row[x * 4..x * 4 + 4], transfer), weight);
        }
    }
}

/// The stored RGBA of one output texel. Colour is un-premultiplied by its average alpha; a texel with no alpha takes
/// the plain average colour instead.
fn finish(sum: &Sum, transfer: &Transfer) -> [u8; 4] {
    let rgb = if sum.alpha > 0.0 { sum.premultiplied.map(|v| v / sum.alpha) } else { sum.plain };
    [transfer.store(rgb[0]), transfer.store(rgb[1]), transfer.store(rgb[2]), to_byte(sum.alpha)]
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
    fn the_sixteen_bit_table_endpoints_are_exact() {
        let transfer = Transfer::new(TextureUse::Colour);
        assert_eq!(transfer.store(0.0), 0);
        assert_eq!(transfer.store(1.0), 255);
        assert_eq!(transfer.load(255), 1.0);
    }
}
