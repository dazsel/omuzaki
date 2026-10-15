//! Reading an image's pixel size from its header, and the universal limits of llm-api D2.

use crate::message::ImageMime;

/// The longest side an image may have, in pixels (evals.md §6.3).
pub const MAX_IMAGE_SIDE: u32 = 768;
/// The most bytes an image may have.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes)
}

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 2).and_then(|b| <[u8; 2]>::try_from(b).ok()).map(|b| u32::from(u16::from_be_bytes(b)))
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 2).and_then(|b| <[u8; 2]>::try_from(b).ok()).map(|b| u32::from(u16::from_le_bytes(b)))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
}

/// The `(width, height)` of a JPEG: the first start-of-frame marker holds them.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xff {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        match marker {
            0xff => at += 1,
            // Standalone markers have no length.
            0x00 | 0x01 | 0xd0..=0xd8 => at += 2,
            // Start of frame (not the table, arithmetic-conditioning or lossless-extension markers).
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                return be16(bytes, at + 7).zip(be16(bytes, at + 5));
            }
            _ => at += 2 + usize::try_from(be16(bytes, at + 2)?).ok()?,
        }
    }
    None
}

/// The `(width, height)` of a WebP, from its first chunk (`VP8 `, `VP8L` or `VP8X`).
fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    match bytes.get(12..16)? {
        b"VP8 " => Some((le16(bytes, 26)? & 0x3fff, le16(bytes, 28)? & 0x3fff)),
        b"VP8L" => {
            let packed = u32::from_le_bytes(<[u8; 4]>::try_from(bytes.get(21..25)?).ok()?);
            Some(((packed & 0x3fff) + 1, ((packed >> 14) & 0x3fff) + 1))
        }
        b"VP8X" => Some((le24(bytes, 24)? + 1, le24(bytes, 27)? + 1)),
        _ => None,
    }
}

/// The pixel size of an image from its header, or `None` if the header is not readable.
pub fn size(mime: ImageMime, bytes: &[u8]) -> Option<(u32, u32)> {
    match mime {
        ImageMime::Png => be32(bytes, 16).zip(be32(bytes, 20)),
        ImageMime::Gif => le16(bytes, 6).zip(le16(bytes, 8)),
        ImageMime::Jpeg => jpeg_size(bytes),
        ImageMime::Webp => webp_size(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b
    }

    #[test]
    fn png_and_gif_headers_are_read() {
        assert_eq!(size(ImageMime::Png, &png(640, 480)), Some((640, 480)));
        let gif = [b'G', b'I', b'F', b'8', b'9', b'a', 0x20, 0x01, 0x10, 0x00];
        assert_eq!(size(ImageMime::Gif, &gif), Some((288, 16)));
        assert_eq!(size(ImageMime::Png, &[1, 2, 3]), None);
    }

    #[test]
    fn a_jpeg_start_of_frame_is_found_past_other_segments() {
        // SOI, an APP0 segment of length 4, then SOF0: length 17, precision 8, height 300, width 500.
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08, 0x01, 0x2c, 0x01, 0xf4, 0x03]);
        assert_eq!(size(ImageMime::Jpeg, &jpeg), Some((500, 300)));
        assert_eq!(size(ImageMime::Jpeg, &[0xff, 0xd8]), None);
    }

    #[test]
    fn the_three_webp_flavours_are_read() {
        let mut lossy = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0".to_vec();
        lossy.extend_from_slice(&[0, 0, 0, 0x9d, 0x01, 0x2a, 0x20, 0x03, 0x58, 0x02]);
        assert_eq!(size(ImageMime::Webp, &lossy), Some((800, 600)));
        let mut lossless = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0".to_vec();
        let packed: u32 = 0x63 | (0x31 << 14);
        lossless.push(0x2f);
        lossless.extend_from_slice(&packed.to_le_bytes());
        assert_eq!(size(ImageMime::Webp, &lossless), Some((100, 50)));
        let mut extended = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0".to_vec();
        extended.extend_from_slice(&[0, 0, 0, 0]);
        extended.extend_from_slice(&[0x3f, 0x01, 0x00, 0xef, 0x00, 0x00]);
        assert_eq!(size(ImageMime::Webp, &extended), Some((320, 240)));
    }
}
