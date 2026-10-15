//! What a PNG or JPEG header says before any pixel is decoded: a JPEG's frame (its size and sampling, which set the
//! coefficient buffers a decoder keeps) and a PNG's or an ICC profile's colour metadata (which says whether the pixels are
//! sRGB). Every read is bounded by the slice it came from, so a truncated or crafted header yields `None`, not a panic.

use ::image::ImageFormat;

use crate::icc;

/// A JPEG frame header: the size, and each component's horizontal and vertical sampling factors.
pub(crate) struct JpegFrame {
    /// The width in pixels.
    pub(crate) width: u32,
    /// The height in pixels.
    pub(crate) height: u32,
    /// `(h, v)` for each component, in the frame's order.
    pub(crate) sampling: Vec<(u8, u8)>,
}

/// The first frame header of a JPEG, or `None` when there is none before the scan (or the bytes are cut short).
pub(crate) fn jpeg_frame(bytes: &[u8]) -> Option<JpegFrame> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2;
    loop {
        // Markers may be padded with 0xFF fill bytes; the marker is the byte after the last fill byte.
        while *bytes.get(at)? == 0xFF {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        match marker {
            0x00 | 0x01 | 0xD0..=0xD8 => continue, // stuffed or standalone markers: no length field
            0xD9 | 0xDA => return None,            // end of image, or the scan, before any frame header
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]));
        if length < 2 {
            return None;
        }
        let segment = bytes.get(at + 2..at + length)?;
        // SOF0..SOF15, except DHT (C4), JPG (C8) and DAC (CC), which share the range.
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u32::from(u16::from_be_bytes([*segment.get(1)?, *segment.get(2)?]));
            let width = u32::from(u16::from_be_bytes([*segment.get(3)?, *segment.get(4)?]));
            let count = usize::from(*segment.get(5)?);
            let mut sampling = Vec::with_capacity(count);
            for component in 0..count {
                let factors = *segment.get(7 + 3 * component)?;
                let (h, v) = (factors >> 4, factors & 0x0F);
                if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                    return None;
                }
                sampling.push((h, v));
            }
            return Some(JpegFrame { width, height, sampling });
        }
        at += length;
    }
}

/// The length of a JPEG's embedded ICC profile as its APP2 chunks declare it, summed, read before any of the profile is
/// copied. `None` when there is no profile or the segments are cut short.
pub(crate) fn jpeg_icc_declared_len(bytes: &[u8]) -> Option<u64> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let (mut at, mut total, mut found) = (2, 0_u64, false);
    loop {
        while *bytes.get(at)? == 0xFF {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        match marker {
            0x00 | 0x01 | 0xD0..=0xD8 => continue,
            0xD9 | 0xDA => return found.then_some(total),
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]));
        if length < 2 {
            return None;
        }
        let segment = bytes.get(at + 2..at + length)?;
        // An APP2 chunk: `ICC_PROFILE\0`, its sequence number and count (14 bytes), then its share of the profile.
        if marker == 0xE2 && segment.starts_with(b"ICC_PROFILE\0") && segment.len() >= 14 {
            total += (segment.len() - 14) as u64;
            found = true;
        }
        at += length;
    }
}

/// The colour a PNG declares before its pixels: the `sRGB` chunk, the gamma and the primaries, when the file has them.
#[derive(Default)]
pub(crate) struct PngColour {
    /// Whether the file has an `sRGB` chunk.
    pub(crate) srgb: bool,
    /// The `gAMA` chunk's gamma times 100 000 (sRGB is 45 455), when present.
    pub(crate) gamma: Option<u32>,
    /// The `cHRM` chunk's eight values (white x and y, red, green, blue), times 100 000, when present.
    pub(crate) chromaticities: Option<[u32; 8]>,
}

/// Reads the colour chunks of a PNG, stopping at the first image data chunk. Chunk CRCs are the decoder's to check.
pub(crate) fn png_colour(bytes: &[u8]) -> PngColour {
    let mut colour = PngColour::default();
    let mut at = 8; // past the signature
    while let (Some(length), Some(kind)) = (be_u32(bytes, at), bytes.get(at + 4..at + 8)) {
        let data = bytes.get(at + 8..).and_then(|rest| rest.get(..length as usize)).unwrap_or_default();
        match kind {
            b"IDAT" | b"IEND" => break,
            b"sRGB" => colour.srgb = true,
            b"gAMA" if data.len() == 4 => colour.gamma = be_u32(data, 0),
            b"cHRM" if data.len() == 32 => {
                colour.chromaticities = Some(std::array::from_fn(|i| be_u32(data, 4 * i).unwrap_or(0)));
            }
            _ => {}
        }
        at = at.saturating_add(12).saturating_add(length as usize);
    }
    colour
}

/// The sRGB gamma, times 100 000, and how far a file's gamma may stray from it: the two agree to within 0.005.
pub(crate) const SRGB_GAMMA: u32 = 45_455;
const GAMMA_TOLERANCE: u32 = 500;

/// The sRGB primaries and white point, times 100 000, and how far a file's chromaticities may stray from them.
const SRGB_CHROMATICITIES: [u32; 8] = [31_270, 32_900, 64_000, 33_000, 30_000, 60_000, 15_000, 6_000];
const CHROMATICITY_TOLERANCE: u32 = 100;

/// What a file declares about its colour, when it is not sRGB: `Some` names what was found. A file with no colour tags is
/// untagged, and is read as sRGB, as the web and most image tools read it. An embedded profile decides on its own; a PNG
/// with an `sRGB` chunk is sRGB whatever its gamma says, as the PNG specification has it; a JPEG in CMYK is not read.
pub(crate) fn non_srgb(bytes: &[u8], format: ImageFormat, icc: Option<&[u8]>) -> Option<String> {
    if let Some(profile) = icc {
        return (!icc::is_srgb(profile)).then(|| "an embedded colour profile that is not sRGB".to_owned());
    }
    if format == ImageFormat::Jpeg {
        let frame = jpeg_frame(bytes)?;
        return (frame.sampling.len() == 4).then(|| "CMYK colour".to_owned());
    }
    let colour = png_colour(bytes);
    if colour.srgb {
        return None;
    }
    if let Some(gamma) = colour.gamma.filter(|gamma| gamma.abs_diff(SRGB_GAMMA) > GAMMA_TOLERANCE) {
        return Some(format!("a gamma of {:.2}", f64::from(gamma) / 100_000.0));
    }
    let primaries_match = colour.chromaticities.is_none_or(|chromaticities| {
        chromaticities.iter().zip(SRGB_CHROMATICITIES).all(|(got, want)| got.abs_diff(want) <= CHROMATICITY_TOLERANCE)
    });
    (!primaries_match).then(|| "primaries other than sRGB's".to_owned())
}

/// Reads a big-endian `u32` at `at`, or `None` when it is past the end.
fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(bytes.get(at..end)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::{jpeg_frame, png_colour};

    #[test]
    fn a_forged_png_chunk_length_stops_the_walk_without_a_panic() {
        // A `gAMA` whose length is four billion: its data is past the end, so it is ignored, and the walk ends.
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend(u32::MAX.to_be_bytes());
        png.extend(b"gAMA");
        png.extend([0; 4]);
        let colour = png_colour(&png);
        assert!(colour.gamma.is_none());
        assert!(!colour.srgb);
    }

    #[test]
    fn a_jpeg_segment_longer_than_the_bytes_is_refused() {
        // A SOF0 segment that claims 65 535 bytes, with four present.
        assert!(jpeg_frame(&[0xFF, 0xD8, 0xFF, 0xC0, 0xFF, 0xFF, 8, 0, 1, 0, 1]).is_none());
    }

    #[test]
    fn a_jpeg_component_count_past_the_segment_is_refused() {
        // A SOF0 whose component count is 255, with one component's bytes present.
        let frame = [0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 8, 0, 1, 0, 1, 255, 1, 0x11, 0];
        assert!(jpeg_frame(&frame).is_none());
    }

    #[test]
    fn a_jpeg_sampling_factor_of_zero_is_refused() {
        let frame = [0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 8, 0, 1, 0, 1, 1, 1, 0x01, 0];
        assert!(jpeg_frame(&frame).is_none());
    }
}
