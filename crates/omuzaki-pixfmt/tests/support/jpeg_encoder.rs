//! A small baseline JPEG encoder for tests and benches: synthetic MJPG-like frames at any size, without committing
//! large JPEGs. Float DCT, the Annex K tables scaled by an IJG quality, 4:4:4, 4:2:2 or 4:2:0, optional restart
//! intervals, and optionally no DHT segment (as webcams send MJPG). Shared with `#[path]`; not every user calls
//! everything.
#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]

/// Chroma subsampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sampling {
    S444,
    S422,
    S420,
}

impl Sampling {
    /// The luma sampling factors (h, v).
    fn luma(self) -> (usize, usize) {
        match self {
            Self::S444 => (1, 1),
            Self::S422 => (2, 1),
            Self::S420 => (2, 2),
        }
    }
}

/// How to encode.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub quality: u8,
    pub sampling: Sampling,
    /// MCUs per restart interval, 0 for none.
    pub restart: u16,
    /// Write the Huffman tables (MJPG from a webcam leaves them out).
    pub dht: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { quality: 85, sampling: Sampling::S422, restart: 0, dht: false }
    }
}

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

const LUMA_Q: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51,
    87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103, 99,
];

const CHROMA_Q: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99,
];

const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125];
const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119];

/// The Annex K AC symbols: every (run, size) pair, in the order of the standard tables' code lengths.
fn ac_values(luma: bool) -> Vec<u8> {
    let luma_order: [u8; 162] = [
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
    let chroma_order: [u8; 162] = [
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
    if luma { luma_order.to_vec() } else { chroma_order.to_vec() }
}

/// Code and length for each symbol of a table.
struct Codes([(u16, u8); 256]);

impl Codes {
    fn new(bits: &[u8; 16], values: &[u8]) -> Self {
        let mut codes = [(0, 0); 256];
        let (mut code, mut k) = (0_u16, 0);
        for (len, &n) in bits.iter().enumerate() {
            for _ in 0..n {
                codes[usize::from(values[k])] = (code, len as u8 + 1);
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        Self(codes)
    }
}

struct Writer {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Writer {
    fn bits(&mut self, value: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                let b = self.acc as u8;
                self.out.push(b);
                if b == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    /// Pads the last byte with ones.
    fn flush(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.bits((1 << pad) - 1, pad);
        }
    }

    fn symbol(&mut self, codes: &Codes, s: u8) {
        let (code, len) = codes.0[usize::from(s)];
        self.bits(u32::from(code), u32::from(len));
    }

    fn value(&mut self, codes: &Codes, run: u8, v: i32) {
        let size = 32 - v.unsigned_abs().leading_zeros();
        self.symbol(codes, (run << 4) | size as u8);
        let bits = if v < 0 { v - 1 } else { v } as u32 & ((1 << size) - 1);
        self.bits(bits, size);
    }
}

fn scaled(table: &[u16; 64], quality: u8) -> [u16; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    table.map(|v| ((u32::from(v) * scale + 50) / 100).clamp(1, 255) as u16)
}

fn fdct(block: &[f32; 64]) -> [f32; 64] {
    let c = |u: usize| if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
    let mut cos = [[0.0_f32; 8]; 8];
    for (x, row) in cos.iter_mut().enumerate() {
        for (u, v) in row.iter_mut().enumerate() {
            *v = (((2 * x + 1) * u) as f32 * std::f32::consts::PI / 16.0).cos();
        }
    }
    let mut tmp = [0.0_f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            tmp[y * 8 + u] = (0..8).map(|x| block[y * 8 + x] * cos[x][u]).sum::<f32>() * c(u) / 2.0;
        }
    }
    let mut out = [0.0_f32; 64];
    for v in 0..8 {
        for u in 0..8 {
            out[v * 8 + u] = (0..8).map(|y| tmp[y * 8 + u] * cos[y][v]).sum::<f32>() * c(v) / 2.0;
        }
    }
    out
}

/// Encodes packed RGB8 (`width * height * 3` bytes) as a baseline JPEG.
pub fn encode(rgb: &[u8], width: usize, height: usize, o: Options) -> Vec<u8> {
    assert_eq!(rgb.len(), width * height * 3);
    let (hs, vs) = o.sampling.luma();
    // Full-resolution planes, level-shifted.
    let mut planes = [vec![0.0_f32; width * height], vec![0.0; width * height], vec![0.0; width * height]];
    for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
        let (r, g, b) = (f32::from(px[0]), f32::from(px[1]), f32::from(px[2]));
        planes[0][i] = 0.299 * r + 0.587 * g + 0.114 * b - 128.0;
        planes[1][i] = -0.168_736 * r - 0.331_264 * g + 0.5 * b;
        planes[2][i] = 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    }
    let quant = [scaled(&LUMA_Q, o.quality), scaled(&CHROMA_Q, o.quality)];
    let dc = [Codes::new(&DC_LUMA_BITS, &DC_VALUES), Codes::new(&DC_CHROMA_BITS, &DC_VALUES)];
    let ac = [Codes::new(&AC_LUMA_BITS, &ac_values(true)), Codes::new(&AC_CHROMA_BITS, &ac_values(false))];

    let mut w = Writer { out: vec![0xFF, 0xD8], acc: 0, n: 0 };
    let seg = |w: &mut Writer, marker: u8, body: &[u8]| {
        w.out.extend_from_slice(&[0xFF, marker]);
        w.out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        w.out.extend_from_slice(body);
    };
    seg(&mut w, 0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    for (slot, q) in quant.iter().enumerate() {
        let mut body = vec![slot as u8];
        body.extend(ZIGZAG.iter().map(|&n| q[n] as u8));
        seg(&mut w, 0xDB, &body);
    }
    let mut sof = vec![8];
    sof.extend_from_slice(&(height as u16).to_be_bytes());
    sof.extend_from_slice(&(width as u16).to_be_bytes());
    sof.extend_from_slice(&[3, 1, ((hs << 4) | vs) as u8, 0, 2, 0x11, 1, 3, 0x11, 1]);
    seg(&mut w, 0xC0, &sof);
    if o.dht {
        let mut body = Vec::new();
        for (class_slot, bits, values) in [
            (0x00, DC_LUMA_BITS, DC_VALUES.to_vec()),
            (0x10, AC_LUMA_BITS, ac_values(true)),
            (0x01, DC_CHROMA_BITS, DC_VALUES.to_vec()),
            (0x11, AC_CHROMA_BITS, ac_values(false)),
        ] {
            body.push(class_slot);
            body.extend_from_slice(&bits);
            body.extend_from_slice(&values);
        }
        seg(&mut w, 0xC4, &body);
    }
    if o.restart > 0 {
        seg(&mut w, 0xDD, &o.restart.to_be_bytes());
    }
    seg(&mut w, 0xDA, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);

    let sample = |plane: &[f32], x: usize, y: usize| plane[y.min(height - 1) * width + x.min(width - 1)];
    let (mcus_x, mcus_y) = (width.div_ceil(8 * hs), height.div_ceil(8 * vs));
    let mut pred = [0_i32; 3];
    let mut block = [0.0_f32; 64];
    let mut mcu = 0_usize;
    let mut rst = 0_u8;
    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if o.restart > 0 && mcu > 0 && mcu.is_multiple_of(usize::from(o.restart)) {
                w.flush();
                w.out.extend_from_slice(&[0xFF, 0xD0 + rst]);
                rst = (rst + 1) % 8;
                pred = [0; 3];
            }
            mcu += 1;
            for comp in 0..3 {
                let (bh, bv) = if comp == 0 { (hs, vs) } else { (1, 1) };
                let (fx, fy) = if comp == 0 { (1, 1) } else { (hs, vs) };
                for by in 0..bv {
                    for bx in 0..bh {
                        for (i, b) in block.iter_mut().enumerate() {
                            // The block's sample: one pixel for luma, the mean of fx x fy pixels for chroma.
                            let cx = (mx * bh + bx) * 8 + i % 8;
                            let cy = (my * bv + by) * 8 + i / 8;
                            let mut s = 0.0;
                            for dy in 0..fy {
                                for dx in 0..fx {
                                    s += sample(&planes[comp], cx * fx + dx, cy * fy + dy);
                                }
                            }
                            *b = s / (fx * fy) as f32;
                        }
                        let t = usize::from(comp != 0);
                        let coef = fdct(&block);
                        let q: Vec<i32> =
                            ZIGZAG.iter().map(|&n| (coef[n] / f32::from(quant[t][n])).round() as i32).collect();
                        let diff = q[0] - pred[comp];
                        pred[comp] = q[0];
                        let size = 32 - diff.unsigned_abs().leading_zeros();
                        w.symbol(&dc[t], size as u8);
                        let bits = if diff < 0 { diff - 1 } else { diff } as u32 & ((1_u32 << size) - 1);
                        w.bits(bits, size);
                        let mut run = 0_u8;
                        for &v in &q[1..] {
                            if v == 0 {
                                run += 1;
                                continue;
                            }
                            while run >= 16 {
                                w.symbol(&ac[t], 0xF0);
                                run -= 16;
                            }
                            w.value(&ac[t], run, v);
                            run = 0;
                        }
                        if run > 0 {
                            w.symbol(&ac[t], 0x00);
                        }
                    }
                }
            }
        }
    }
    w.flush();
    w.out.extend_from_slice(&[0xFF, 0xD9]);
    w.out
}

/// A synthetic camera-like picture: gradients, a soft disc, fine stripes and a little noise, so a frame has edges,
/// texture and flat areas like a real one.
pub fn picture(width: usize, height: usize, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    let mut noise = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state % 17) as i32 - 8
    };
    let mut rgb = Vec::with_capacity(width * height * 3);
    let (cx, cy, r) = (width as f32 * 0.5, height as f32 * 0.45, height as f32 * 0.3);
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f32 / width as f32, y as f32 / height as f32);
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let disc = ((r - d) / 6.0).clamp(0.0, 1.0);
            let stripes = if (x / 3 + y / 7).is_multiple_of(2) && fy > 0.8 { 40.0 } else { 0.0 };
            let base = [60.0 + 150.0 * fx, 40.0 + 120.0 * fy, 200.0 - 120.0 * fx];
            let skin = [235.0, 190.0, 165.0];
            for c in 0..3 {
                let v = base[c] * (1.0 - disc) + skin[c] * disc + stripes;
                rgb.push((v as i32 + noise()).clamp(0, 255) as u8);
            }
        }
    }
    rgb
}
