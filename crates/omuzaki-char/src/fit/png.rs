//! A minimal PNG writer for the heatmaps: 8-bit RGBA, no row filter, one fixed-Huffman deflate block. The only
//! matches it makes repeat the previous pixel (distance 4), which is what a heatmap is made of: long runs of
//! transparent agreement and solid bands of the two colours, so a 512-pixel view packs into a few kilobytes and travels
//! inline in an op's answer. It needs no dependency, and the same picture always gives the same bytes.

use super::heatmap::RgbaImage;

/// The PNG file signature.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Bytes per pixel, and so the distance of a match that repeats the previous pixel.
const PIXEL: usize = 4;
/// The shortest and longest match deflate can say.
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// The base length of each length symbol (257 + index) and its extra bits (RFC 1951, 3.2.5).
const LENGTH_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
/// The distance symbol of a distance of [`PIXEL`] bytes: distances 1 to 4 are symbols 0 to 3, with no extra bits.
const PIXEL_DISTANCE_SYMBOL: u32 = 3;
/// The end-of-block symbol.
const END_OF_BLOCK: u16 = 256;

/// Deflate's bit order: values go out least significant bit first, Huffman codes most significant bit first.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u64,
    filled: u32,
}

impl Bits {
    /// The low `count` bits of `value`, least significant first.
    fn put(&mut self, value: u32, count: u32) {
        self.acc |= u64::from(value) << self.filled;
        self.filled += count;
        while self.filled >= 8 {
            self.out.push(self.acc.to_le_bytes()[0]);
            self.acc >>= 8;
            self.filled -= 8;
        }
    }

    /// A Huffman code of `len` bits, most significant first.
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    /// The fixed-Huffman code of a literal/length symbol (RFC 1951, 3.2.6).
    fn symbol(&mut self, sym: u16) {
        let sym = u32::from(sym);
        match sym {
            0..=143 => self.code(0x30 + sym, 8),
            144..=255 => self.code(0x190 + sym - 144, 9),
            256..=279 => self.code(sym - 256, 7),
            _ => self.code(0xC0 + sym - 280, 8),
        }
    }

    /// A match of `len` bytes ([`MIN_MATCH`]..=[`MAX_MATCH`]) that repeats the previous pixel.
    #[allow(clippy::cast_possible_truncation)] // the index is below 29 and the length at most 258
    fn repeat_pixel(&mut self, len: usize) {
        // The last base not above `len`; 258 has its own symbol, so it never takes 227 plus 31.
        let i = LENGTH_BASE.iter().rposition(|&b| usize::from(b) <= len).unwrap_or(0);
        self.symbol(257 + i as u16);
        self.put((len - usize::from(LENGTH_BASE[i])) as u32, u32::from(LENGTH_EXTRA[i]));
        self.code(PIXEL_DISTANCE_SYMBOL, 5);
    }

    /// The bytes, the last one padded with zero bits.
    fn finish(mut self) -> Vec<u8> {
        if self.filled > 0 {
            self.out.push(self.acc.to_le_bytes()[0]);
        }
        self.out
    }
}

/// `raw` as one final fixed-Huffman deflate block whose only matches repeat the previous pixel.
fn deflate(raw: &[u8]) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.put(1, 1); // the final block
    bits.put(1, 2); // fixed Huffman codes
    let mut i = 0;
    while i < raw.len() {
        let run = if i < PIXEL {
            0
        } else {
            (0..MAX_MATCH.min(raw.len() - i)).take_while(|&k| raw[i + k] == raw[i + k - PIXEL]).count()
        };
        if run >= MIN_MATCH {
            bits.repeat_pixel(run);
            i += run;
        } else {
            bits.symbol(u16::from(raw[i]));
            i += 1;
        }
    }
    bits.symbol(END_OF_BLOCK);
    bits.finish()
}

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
    // zlib: deflate with a 32 KiB window, no dictionary, the default level's check bits.
    let mut z = vec![0x78, 0x01];
    z.extend(deflate(&raw));
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

    /// The distance base and extra bits of each distance symbol (RFC 1951, 3.2.5), for the test's inflater.
    const DIST_BASE: [usize; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097,
        6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u32; 30] =
        [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

    /// Reads deflate's bits back, least significant first.
    struct Reader<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl Reader<'_> {
        fn bit(&mut self) -> u32 {
            let b = (self.bytes[self.at / 8] >> (self.at % 8)) & 1;
            self.at += 1;
            u32::from(b)
        }
        fn value(&mut self, count: u32) -> u32 {
            (0..count).fold(0, |v, i| v | (self.bit() << i))
        }
        /// `count` more bits of a Huffman code, most significant first.
        fn code(&mut self, mut code: u32, count: u32) -> u32 {
            for _ in 0..count {
                code = (code << 1) | self.bit();
            }
            code
        }
        fn symbol(&mut self) -> u32 {
            let c = self.code(0, 7);
            if c <= 0b001_0111 {
                return 256 + c;
            }
            let c = self.code(c, 1);
            match c {
                0x30..=0xBF => c - 0x30,
                0xC0..=0xC7 => 280 + c - 0xC0,
                _ => 144 + self.code(c, 1) - 0x190,
            }
        }
    }

    /// An inflater for the fixed-Huffman blocks deflate writes with any distance: an independent decoder, so the
    /// round trip checks the bit order, the codes and the matches.
    fn inflate(z: &[u8]) -> Vec<u8> {
        assert_eq!(&z[..2], &[0x78, 0x01]);
        let mut r = Reader { bytes: &z[2..], at: 0 };
        let mut out: Vec<u8> = Vec::new();
        loop {
            let last = r.bit();
            assert_eq!(r.value(2), 1, "a fixed-Huffman block");
            loop {
                let sym = r.symbol();
                match sym {
                    0..=255 => out.push(u8::try_from(sym).unwrap()),
                    256 => break,
                    _ => {
                        let i = usize::try_from(sym - 257).unwrap();
                        let extra = usize::try_from(r.value(u32::from(LENGTH_EXTRA[i]))).unwrap();
                        let len = usize::from(LENGTH_BASE[i]) + extra;
                        let d = usize::try_from(r.code(0, 5)).unwrap();
                        let dist = DIST_BASE[d] + usize::try_from(r.value(DIST_EXTRA[d])).unwrap();
                        for _ in 0..len {
                            out.push(out[out.len() - dist]);
                        }
                    }
                }
            }
            if last == 1 {
                break;
            }
        }
        let tail = &z[2 + r.at.div_ceil(8)..];
        assert_eq!(tail, adler32(&out).to_be_bytes(), "the Adler-32 after the block");
        out
    }

    /// The raw scanlines of a PNG this module wrote (its one IDAT, inflated).
    fn scanlines(png: &[u8]) -> Vec<u8> {
        let idat = png.windows(4).position(|w| w == b"IDAT").unwrap();
        let len = usize::try_from(u32::from_be_bytes(png[idat - 4..idat].try_into().unwrap())).unwrap();
        inflate(&png[idat + 4..idat + 4 + len])
    }

    #[test]
    fn the_checksums_match_their_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn deflate_round_trips_runs_literals_and_every_length_symbol() {
        let mut data: Vec<u8> = (0..=255).collect();
        // A run of every length 0..=600 after a fresh pixel, so each length symbol and its extra bits appear, and
        // runs longer than one match.
        for (n, len) in (0..=600_usize).enumerate() {
            let [lo, hi, ..] = n.to_le_bytes();
            let pixel = [lo, hi, 7, 255];
            data.extend_from_slice(&pixel);
            data.extend(pixel.iter().cycle().take(len));
        }
        let mut z = vec![0x78, 0x01];
        z.extend(deflate(&data));
        z.extend_from_slice(&adler32(&data).to_be_bytes());
        assert_eq!(inflate(&z), data);
        assert!(z.len() < data.len() / 20, "{} bytes for {}", z.len(), data.len());
    }

    #[test]
    fn a_heatmap_encodes_to_a_png_with_its_size_that_decodes_to_its_pixels() {
        let model = Silhouette::new(64, 48, (0..64 * 48).map(|i| (i % 64) < 40).collect()).unwrap();
        let target = Silhouette::new(64, 48, (0..64 * 48).map(|i| (i % 64) > 8 && (i % 64) < 36).collect()).unwrap();
        let map = heatmap(&model, &target).unwrap();
        let png = encode(&map);
        assert_eq!(&png[..8], &SIGNATURE);
        assert_eq!(&png[16..20], &u32::try_from(map.width()).unwrap().to_be_bytes());
        assert_eq!(&png[20..24], &u32::try_from(map.height()).unwrap().to_be_bytes());
        assert_eq!(png, encode(&map));
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        let rows = scanlines(&png);
        let stride = map.width() * 4 + 1;
        assert_eq!(rows.len(), stride * map.height());
        for (y, row) in rows.chunks(stride).enumerate() {
            assert_eq!(row[0], 0, "row {y} is unfiltered");
            let want: Vec<u8> =
                map.pixels()[y * map.width()..(y + 1) * map.width()].iter().flatten().copied().collect();
            assert_eq!(&row[1..], &want[..], "row {y}");
        }
    }
}
