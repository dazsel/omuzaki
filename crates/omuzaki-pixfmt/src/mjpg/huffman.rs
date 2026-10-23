//! Huffman tables and the entropy-coded bit reader (ITU T.81 §F.2.2).

// Codec arithmetic: every cast is of a value bounded by construction (a code of at most 16 bits, a clamped or
// shifted sample, a table index under 512), and the tests hold each fast path to its scalar reference.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

/// Bits looked up in one step: every code this long or shorter decodes from one table read.
const FAST_BITS: u32 = 9;

/// The tables T.81 Annex K suggests, which an MJPG frame uses without sending them (it has no DHT segment).
pub(super) mod standard {
    /// Luminance DC: code counts per length 1..=16.
    pub const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    /// Luminance DC: the symbols in code order.
    pub const DC_LUMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    /// Chrominance DC: code counts.
    pub const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
    /// Chrominance DC: symbols.
    pub const DC_CHROMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    /// Luminance AC: code counts.
    pub const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125];
    /// Luminance AC: symbols.
    pub const AC_LUMA_VALUES: [u8; 162] = [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71,
        0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
        0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37,
        0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
        0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3,
        0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
        0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
        0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    /// Chrominance AC: code counts.
    pub const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119];
    /// Chrominance AC: symbols.
    pub const AC_CHROMA_VALUES: [u8; 162] = [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22,
        0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1,
        0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36,
        0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
        0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
        0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
        0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
        0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
}

/// One Huffman table, built in place: rebuilding it for the next frame allocates nothing.
pub(super) struct Huffman {
    /// For each `FAST_BITS`-bit prefix: `(length << 8) | symbol` when a code of at most `FAST_BITS` bits starts it, 0
    /// otherwise.
    fast: [u16; 1 << FAST_BITS],
    /// For AC tables, each `FAST_BITS`-bit prefix that holds a whole code and its value bits: `(value << 16) | (run <<
    /// 8) | bits used`, 0 otherwise. One lookup then decodes a coefficient.
    fast_ac: [i32; 1 << FAST_BITS],
    /// The largest code of each length (index 1..=16), -1 when there is none.
    max_code: [i32; 17],
    /// For each length: the index in `values` of its first code, minus that code.
    offset: [i32; 17],
    values: [u8; 256],
}

impl Huffman {
    pub(super) const fn empty() -> Self {
        Self {
            fast: [0; 1 << FAST_BITS],
            fast_ac: [0; 1 << FAST_BITS],
            max_code: [-1; 17],
            offset: [0; 17],
            values: [0; 256],
        }
    }

    /// Builds the table from a DHT's code counts per length (1..=16) and its symbols.
    pub(super) fn build(&mut self, counts: &[u8; 16], symbols: &[u8]) -> Result<(), &'static str> {
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if total > 256 || symbols.len() < total {
            return Err("bad Huffman table");
        }
        self.fast = [0; 1 << FAST_BITS];
        self.values[..total].copy_from_slice(&symbols[..total]);
        let mut code: i32 = 0;
        let mut k: i32 = 0;
        for len in 1..=16_u32 {
            let n = i32::from(counts[len as usize - 1]);
            if n == 0 {
                self.max_code[len as usize] = -1;
            } else {
                self.offset[len as usize] = k - code;
                for _ in 0..n {
                    // Codes of one length must fit in it: a table that overflows is corrupt.
                    if code >= (1 << len) {
                        return Err("bad Huffman table");
                    }
                    if len <= FAST_BITS {
                        // Every prefix that starts with this code decodes to it.
                        let shift = FAST_BITS - len;
                        let first = (code as usize) << shift;
                        let entry = ((len as u16) << 8) | u16::from(self.values[k as usize]);
                        self.fast[first..first + (1 << shift)].fill(entry);
                    }
                    code += 1;
                    k += 1;
                }
                self.max_code[len as usize] = code - 1;
            }
            code <<= 1;
        }
        self.build_fast_ac();
        Ok(())
    }

    fn build_fast_ac(&mut self) {
        for (prefix, entry) in self.fast_ac.iter_mut().enumerate() {
            *entry = 0;
            let fast = self.fast[prefix];
            if fast == 0 {
                continue;
            }
            let (len, rs) = (u32::from(fast >> 8), fast & 0xFF);
            let (run, size) = (u32::from(rs >> 4), u32::from(rs & 0x0F));
            if size == 0 || len + size > FAST_BITS {
                continue;
            }
            // The value bits follow the code inside the prefix.
            let raw = ((prefix as u32) >> (FAST_BITS - len - size)) & ((1 << size) - 1);
            let raw = raw as i32;
            let value = if raw < (1 << (size - 1)) { raw - (1 << size) + 1 } else { raw };
            *entry = (value << 16) | ((run as i32) << 8) | (len + size) as i32;
        }
    }

    /// Decodes one AC coefficient whose code and value fit in the fast table: `(run, value)`, or `None` when the
    /// next code does not (the caller then decodes it symbol by symbol).
    #[inline]
    pub(super) fn decode_ac_fast(&self, bits: &mut Bits<'_>) -> Option<(usize, i32)> {
        bits.ensure(16);
        let entry = self.fast_ac[bits.peek(FAST_BITS) as usize];
        if entry == 0 {
            return None;
        }
        bits.consume((entry & 0xFF) as u32);
        Some((((entry >> 8) & 0xFF) as usize, entry >> 16))
    }

    /// Decodes one symbol.
    #[inline]
    pub(super) fn decode(&self, bits: &mut Bits<'_>) -> Result<u8, &'static str> {
        bits.ensure(16);
        let entry = self.fast[bits.peek(FAST_BITS) as usize];
        if entry != 0 {
            bits.consume(u32::from(entry >> 8));
            return Ok(entry as u8);
        }
        let code16 = bits.peek(16) as i32;
        for len in FAST_BITS + 1..=16 {
            let code = code16 >> (16 - len);
            if code <= self.max_code[len as usize] {
                bits.consume(len);
                let at = (self.offset[len as usize] + code) as usize;
                return self.values.get(at).copied().ok_or("bad Huffman code");
            }
        }
        Err("bad Huffman code")
    }
}

/// Reads the entropy-coded segment MSB first: drops the stuffed zero after each 0xFF and stops at a marker, after
/// which it supplies zero bits. Reading into those zeros means the data ended early, which [`overran`](Self::overran)
/// reports.
pub(super) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    /// Valid bits at the top of `acc`.
    n: u32,
    /// Zero bytes supplied after a marker or the end of the data; they sit at the bottom of `acc`.
    padding: u32,
    stopped: bool,
}

impl<'a> Bits<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, acc: 0, n: 0, padding: 0, stopped: false }
    }

    #[inline]
    fn ensure(&mut self, want: u32) {
        if self.n < want {
            self.refill();
        }
    }

    fn refill(&mut self) {
        // Fast path: the next eight bytes hold no 0xFF (no stuffing, no marker), so the whole bytes that fit go in at
        // once.
        if !self.stopped
            && let Some(&word) = self.data.get(self.pos..self.pos + 8).and_then(|w| <&[u8; 8]>::try_from(w).ok())
        {
            let w = u64::from_be_bytes(word);
            let inverted = !w;
            let has_ff = inverted.wrapping_sub(0x0101_0101_0101_0101) & !inverted & 0x8080_8080_8080_8080 != 0;
            if !has_ff {
                let bytes = (64 - self.n) / 8;
                let kept = if bytes >= 8 { w } else { w & !(u64::MAX >> (8 * bytes)) };
                self.acc |= kept >> self.n;
                self.n += 8 * bytes;
                self.pos += bytes as usize;
                return;
            }
        }
        while self.n <= 56 {
            let byte = if self.stopped { None } else { self.next_byte() };
            let byte = byte.unwrap_or_else(|| {
                self.stopped = true;
                self.padding += 1;
                0
            });
            self.acc |= u64::from(byte) << (56 - self.n);
            self.n += 8;
        }
    }

    /// The next data byte, or `None` at a marker or the end. A marker is left unread for [`restart`](Self::restart).
    #[inline]
    fn next_byte(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos)?;
        if byte != 0xFF {
            self.pos += 1;
            return Some(byte);
        }
        if self.data.get(self.pos + 1) == Some(&0) {
            self.pos += 2;
            return Some(0xFF);
        }
        None
    }

    #[inline]
    fn peek(&self, k: u32) -> u32 {
        (self.acc >> (64 - k)) as u32
    }

    #[inline]
    fn consume(&mut self, k: u32) {
        self.acc <<= k;
        self.n = self.n.saturating_sub(k);
    }

    /// Reads `s` (0..=15) more bits as a signed coefficient (T.81 F.2.2.1, EXTEND).
    #[inline]
    pub(super) fn receive_extend(&mut self, s: u8) -> i32 {
        if s == 0 {
            return 0;
        }
        let s = u32::from(s.min(15));
        self.ensure(s);
        let v = self.peek(s) as i32;
        self.consume(s);
        if v < (1 << (s - 1)) { v - (1 << s) + 1 } else { v }
    }

    /// Whether more bits were read than the data held.
    pub(super) fn overran(&self) -> bool {
        self.n < self.padding * 8
    }

    /// Skips to the restart marker that ends an interval, past it, and starts the next interval on a byte boundary.
    pub(super) fn restart(&mut self) -> Result<(), &'static str> {
        if self.overran() {
            return Err("data ends early");
        }
        // Bytes this reader has buffered but not used are the interval's padding. Rewinding to them is not needed:
        // the marker search starts where reading stopped.
        loop {
            match self.data.get(self.pos..self.pos + 2) {
                Some(&[0xFF, 0xD0..=0xD7]) => {
                    self.pos += 2;
                    break;
                }
                // Fill bytes before a marker, and unread data (tolerated, as libjpeg does).
                Some(&[0xFF, 0xFF]) => self.pos += 1,
                Some(&[0xFF, 0x00]) => self.pos += 2,
                Some(&[0xFF, _]) => return Err("expected a restart marker"),
                Some(_) => self.pos += 1,
                None => return Err("data ends early"),
            }
        }
        self.acc = 0;
        self.n = 0;
        self.padding = 0;
        self.stopped = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(counts: &[u8; 16], symbols: &[u8]) -> Huffman {
        let mut h = Huffman::empty();
        h.build(counts, symbols).unwrap();
        h
    }

    #[test]
    fn the_standard_tables_are_the_ones_libjpeg_writes() {
        use standard::{
            AC_CHROMA_BITS, AC_CHROMA_VALUES, AC_LUMA_BITS, AC_LUMA_VALUES, DC_CHROMA_BITS, DC_CHROMA_VALUES,
            DC_LUMA_BITS, DC_LUMA_VALUES,
        };
        // Pillow (libjpeg-turbo) writes the Annex K tables when it does not optimise: read them from a fixture.
        let jpeg = include_bytes!("../../tests/fixtures/64x48_420.jpg");
        let mut found = Vec::new();
        let mut i = 2;
        while i + 4 <= jpeg.len() && jpeg[i + 1] != 0xDA {
            let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
            if jpeg[i + 1] == 0xC4 {
                let seg = &jpeg[i + 4..i + 2 + len];
                let mut j = 0;
                while j < seg.len() {
                    let counts: [u8; 16] = seg[j + 1..j + 17].try_into().unwrap();
                    let n: usize = counts.iter().map(|&c| usize::from(c)).sum();
                    found.push((seg[j], counts, seg[j + 17..j + 17 + n].to_vec()));
                    j += 17 + n;
                }
            }
            i += 2 + len;
        }
        let expected = [
            (0x00, DC_LUMA_BITS, DC_LUMA_VALUES.to_vec()),
            (0x10, AC_LUMA_BITS, AC_LUMA_VALUES.to_vec()),
            (0x01, DC_CHROMA_BITS, DC_CHROMA_VALUES.to_vec()),
            (0x11, AC_CHROMA_BITS, AC_CHROMA_VALUES.to_vec()),
        ];
        assert_eq!(found, expected);
    }

    #[test]
    fn codes_decode_through_both_paths() {
        // The AC luminance table has codes from 2 to 16 bits: 0xFFF? codes take the slow path.
        let h = table(&standard::AC_LUMA_BITS, &standard::AC_LUMA_VALUES);
        // 00 -> 0x01 (2 bits), 1010 -> 0x00 (EOB, 4 bits), then the last 16-bit code 1111111111111110 -> 0xfa.
        let data = [0b0010_1011, 0xFF, 0x00, 0b1111_1110, 0b0000_0000];
        let mut bits = Bits::new(&data);
        assert_eq!(h.decode(&mut bits), Ok(0x01));
        assert_eq!(h.decode(&mut bits), Ok(0x00));
        // Two bits used of `11` + 0xFF + `1111111 0`: realign by taking the 2 remaining bits of the first byte.
        assert_eq!(bits.receive_extend(2), 3);
        assert_eq!(h.decode(&mut bits), Ok(0xfa));
        assert!(!bits.overran());
    }

    #[test]
    fn reading_past_the_end_is_an_overrun_and_restart_finds_the_marker() {
        let data = [0xAB, 0xFF, 0xD3, 0xCD];
        let mut bits = Bits::new(&data);
        assert_eq!(bits.receive_extend(8), 0xAB);
        assert!(!bits.overran());
        assert!(bits.restart().is_ok());
        assert_eq!(bits.receive_extend(8), 0xCD);
        assert!(!bits.overran());
        bits.receive_extend(1);
        assert!(bits.overran());
        assert!(bits.restart().is_err());
    }

    #[test]
    fn an_overfull_table_is_refused() {
        let mut h = Huffman::empty();
        assert!(h.build(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[1, 2, 3]).is_err());
        assert!(h.build(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2], &[1]).is_err());
    }
}
