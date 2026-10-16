//! A minimal PNG writer for the heatmaps: 8-bit RGBA, no filtering, zlib stored blocks. Not small, but exact, with no
//! dependency, and the same bytes for the same picture (so the store holds one copy of a heatmap).

use super::heatmap::RgbaImage;

/// The longest run a zlib stored block holds.
const STORED_MAX: usize = 65_535;
/// The PNG file signature.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The CRC-32 of `bytes` (the PNG chunk checksum).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// The Adler-32 of `bytes` (the zlib checksum).
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    // A chunk is at most 2^31 - 1 bytes; the caller's image is at most MAX_SIDE x (MAX_SIDE + legend) pixels.
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// The PNG file of `image`.
pub fn encode(image: &RgbaImage) -> Vec<u8> {
    let mut raw = Vec::with_capacity(image.height() * (image.width() * 4 + 1));
    for row in image.pixels().chunks(image.width().max(1)) {
        raw.push(0); // filter: none
        raw.extend(row.iter().flatten());
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(STORED_MAX).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        let len = u16::try_from(block.len()).unwrap_or(u16::MAX);
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&u32::try_from(image.width()).unwrap_or(u32::MAX).to_be_bytes());
    header.extend_from_slice(&u32::try_from(image.height()).unwrap_or(u32::MAX).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 bit, RGBA, deflate, no filter method, no interlace
    let mut out = SIGNATURE.to_vec();
    chunk(&mut out, *b"IHDR", &header);
    chunk(&mut out, *b"IDAT", &z);
    chunk(&mut out, *b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::Silhouette;
    use crate::fit::heatmap::heatmap;

    #[test]
    fn the_checksums_match_their_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn a_heatmap_encodes_to_a_png_with_its_size_and_is_repeatable() {
        let m = Silhouette::new(6, 4, vec![true; 24]).unwrap();
        let map = heatmap(&m, &m).unwrap();
        let png = encode(&map);
        assert_eq!(&png[..8], &SIGNATURE);
        assert_eq!(
            &png[16..24],
            &[0, 0, 0, u8::try_from(map.width()).unwrap(), 0, 0, 0, u8::try_from(map.height()).unwrap()]
        );
        assert_eq!(png, encode(&map));
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }
}
