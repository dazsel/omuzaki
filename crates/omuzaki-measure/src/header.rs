//! What a PNG or JPEG header says before any pixel is decoded: a JPEG's frame (its size and sampling, which set the
//! coefficient buffers a decoder keeps) and a PNG's or an ICC profile's colour metadata (which says whether the pixels are
//! sRGB). Every read is bounded by the slice it came from, so a truncated or crafted header yields `None`, not a panic.

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

/// What a PNG's chunk headers say is there, found without reading any chunk's data. It decides nothing about colour: the
/// profile and every other colour chunk are read only from the decoder's own validated metadata. It exists because the
/// decoder cannot report what it dropped (a profile past its bound, out of order or corrupt) and because it keeps some
/// chunks whole, which the allocation budget must count.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PngScan {
    /// Whether any `iCCP` chunk is present, wherever it sits.
    pub(crate) iccp: bool,
    /// The summed lengths of every `iCCP` chunk: the decoder buffers each whole before it inflates the profile.
    pub(crate) iccp_bytes: u64,
    /// The summed lengths of every `eXIf` chunk. The locked `png` (0.18) buffers the chunk and keeps a clone of it as the
    /// image's EXIF metadata, so it holds each twice, and has no setting to skip it. Every other chunk it keeps is a fixed
    /// small size (`PLTE`, `tRNS`, `sBIT`, `bKGD`, `pHYs`, `gAMA`, `cHRM`, `sRGB`, `cICP`, `mDCV`, `cLLI`, the animation
    /// chunks) or is skipped (`tEXt`, `zTXt`, `iTXt` by setting, and every unknown ancillary chunk).
    pub(crate) exif_bytes: u64,
}

/// Walks every chunk header of a PNG up to `IEND`, reading only lengths and kinds: no CRC, no data. A length past the end
/// of the bytes ends the walk.
pub(crate) fn png_scan(bytes: &[u8]) -> PngScan {
    let mut scan = PngScan::default();
    let mut at = 8; // past the signature
    while let (Some(length), Some(kind)) = (be_u32(bytes, at), bytes.get(at + 4..at + 8)) {
        match kind {
            b"iCCP" => {
                scan.iccp = true;
                scan.iccp_bytes += u64::from(length);
            }
            b"eXIf" => scan.exif_bytes += u64::from(length),
            b"IEND" => break,
            _ => {}
        }
        at = at.saturating_add(12).saturating_add(length as usize);
    }
    scan
}

/// The width and height a PNG's IHDR chunk declares (the first chunk, at byte 16), or `None` when the bytes end first.
pub(crate) fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    Some((be_u32(bytes, 16)?, be_u32(bytes, 20)?))
}

/// The `cICP` code points that mean sRGB: BT.709 primaries, the sRGB transfer function, RGB (identity matrix), full range.
const CICP_SRGB: [u8; 4] = [1, 13, 0, 1];

/// The sRGB gamma, times 100 000, and how far a file's gamma may stray from it: the two agree to within 0.005.
pub(crate) const SRGB_GAMMA: u32 = 45_455;
const GAMMA_TOLERANCE: u32 = 500;

/// The sRGB primaries and white point, times 100 000, and how far a file's chromaticities may stray from them.
const SRGB_CHROMATICITIES: [u32; 8] = [31_270, 32_900, 64_000, 33_000, 30_000, 60_000, 15_000, 6_000];
const CHROMATICITY_TOLERANCE: u32 = 100;

/// What a PNG declares about its colour when it is not sRGB, read from the decoder's own validated metadata and nowhere
/// else, so that wherever the decoder ignores a chunk (out of order, repeated, a bad CRC, malformed) this does too. The
/// order is the PNG specification's: `cICP`, then an embedded profile (which must be proven sRGB), then the `sRGB` chunk,
/// then `gAMA` and `cHRM`. A file with none is untagged, and is read as sRGB, as the web and most image tools read it.
pub(crate) fn png_non_srgb(info: &png::Info<'_>) -> Option<String> {
    if let Some(cicp) = info.coding_independent_code_points {
        let code = [
            cicp.color_primaries,
            cicp.transfer_function,
            cicp.matrix_coefficients,
            u8::from(cicp.is_video_full_range_image),
        ];
        return (code != CICP_SRGB).then(|| {
            format!("a cICP colour declaration (primaries {}, transfer {}, matrix {})", code[0], code[1], code[2])
        });
    }
    if let Some(profile) = info.icc_profile.as_deref() {
        return (!icc::is_srgb(profile)).then(|| "an embedded colour profile that is not sRGB".to_owned());
    }
    if info.srgb.is_some() {
        return None;
    }
    if let Some(gamma) =
        info.gama_chunk.map(png::ScaledFloat::into_scaled).filter(|gamma| gamma.abs_diff(SRGB_GAMMA) > GAMMA_TOLERANCE)
    {
        return Some(format!("a gamma of {:.2}", f64::from(gamma) / 100_000.0));
    }
    let primaries_match = info.chrm_chunk.is_none_or(|chrm| {
        let got = [chrm.white, chrm.red, chrm.green, chrm.blue].into_iter().flat_map(|(x, y)| [x, y]);
        got.zip(SRGB_CHROMATICITIES).all(|(got, want)| got.into_scaled().abs_diff(want) <= CHROMATICITY_TOLERANCE)
    });
    (!primaries_match).then(|| "primaries other than sRGB's".to_owned())
}

/// What a JPEG declares about its colour when it is not sRGB: an embedded profile that is not proven sRGB, or CMYK. A JPEG
/// with neither is read as sRGB.
pub(crate) fn jpeg_non_srgb(bytes: &[u8], icc: Option<&[u8]>) -> Option<String> {
    if let Some(profile) = icc {
        return (!icc::is_srgb(profile)).then(|| "an embedded colour profile that is not sRGB".to_owned());
    }
    let frame = jpeg_frame(bytes)?;
    (frame.sampling.len() == 4).then(|| "CMYK colour".to_owned())
}

/// Reads a big-endian `u32` at `at`, or `None` when it is past the end.
fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(bytes.get(at..end)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::{PngScan, jpeg_frame, png_scan};

    #[test]
    fn the_scan_finds_profiles_and_exif_anywhere_up_to_iend_from_lengths_and_kinds_only() {
        let chunk = |kind: &[u8; 4], length: u32| {
            let mut out = length.to_be_bytes().to_vec();
            out.extend(kind);
            out.extend(vec![0; length as usize + 4]);
            out
        };
        let png = |chunks: &[Vec<u8>]| [vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A], chunks.concat()].concat();
        let scan = png_scan(&png(&[
            chunk(b"IHDR", 13),
            chunk(b"PLTE", 3),
            chunk(b"iCCP", 5),
            chunk(b"IDAT", 3),
            chunk(b"eXIf", 7),
            chunk(b"eXIf", 2),
            chunk(b"iCCP", 4),
            chunk(b"IEND", 0),
            chunk(b"eXIf", 100),
        ]));
        assert_eq!(scan, PngScan { iccp: true, iccp_bytes: 9, exif_bytes: 9 });
        assert_eq!(png_scan(&png(&[chunk(b"IHDR", 13), chunk(b"IDAT", 3)])), PngScan::default());
        // A forged length stops the walk without a panic.
        assert_eq!(png_scan(&png(&[u32::MAX.to_be_bytes().iter().chain(b"iCCP").copied().collect()])).exif_bytes, 0);
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
