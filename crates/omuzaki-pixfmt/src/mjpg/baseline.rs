//! An allocation-free baseline JPEG decoder for webcam MJPG: 8-bit Huffman-coded sequential frames (SOF0 and SOF1),
//! grey or YCbCr at 4:4:4, 4:2:2 or 4:2:0, with or without restart intervals and with or without their own Huffman
//! tables. Its buffers are sized by the first frame of a size and reused after, so a steady stream allocates nothing.
//! Anything else is [`Parsed::Unsupported`] and goes to zune-jpeg.

use super::color::{grey_to_rgb, h2v1, h2v2, ycc_to_rgb};
use super::huffman::{Bits, Huffman, standard};
use super::idct::{Block, idct, idct_dc};

/// Natural (row-major) index of each zig-zag position.
const NATURAL: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

#[derive(Clone, Copy, Default)]
struct Component {
    id: u8,
    /// Horizontal and vertical sampling factors (1 or 2 for luma, 1 for chroma).
    h: usize,
    v: usize,
    /// Quantisation, DC and AC table slots.
    tq: usize,
    td: usize,
    ta: usize,
}

/// A frame's header, read by [`Baseline::parse`].
pub(super) struct Frame {
    pub(super) width: usize,
    pub(super) height: usize,
    comps: [Component; 3],
    ncomp: usize,
    /// MCUs per restart interval (0 for none).
    restart: usize,
    /// Where the entropy-coded data starts.
    scan: usize,
}

/// What [`Baseline::parse`] found.
pub(super) enum Parsed {
    /// A frame this decoder handles.
    Frame(Frame),
    /// A valid JPEG this decoder leaves to zune-jpeg (progressive, 12-bit, CMYK, unusual sampling...).
    Unsupported,
}

struct Tables {
    dc: [Huffman; 4],
    ac: [Huffman; 4],
    quant: [[u16; 64]; 4],
}

/// The decoder's reusable state.
pub(super) struct Baseline {
    tables: Box<Tables>,
    /// One sample plane per component, padded to whole MCUs.
    planes: [Vec<u8>; 3],
    /// One upsampled row each of Cb and Cr.
    up: [Vec<u8>; 2],
}

impl std::fmt::Debug for Baseline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let plane_bytes = self.planes.iter().map(Vec::len).sum::<usize>();
        f.debug_struct("Baseline").field("plane_bytes", &plane_bytes).finish_non_exhaustive()
    }
}

fn be16(b: &[u8], at: usize) -> Result<usize, &'static str> {
    match b.get(at..at + 2) {
        Some(&[hi, lo]) => Ok(usize::from(u16::from_be_bytes([hi, lo]))),
        _ => Err("truncated header"),
    }
}

fn byte(b: &[u8], at: usize) -> Result<u8, &'static str> {
    b.get(at).copied().ok_or("truncated header")
}

/// Grows `v` to at least `len` (never shrinks, so a stream of one size allocates once).
fn grow<T: Clone + Default>(v: &mut Vec<T>, len: usize) {
    if v.len() < len {
        v.resize(len, T::default());
    }
}

impl Baseline {
    pub(super) fn new() -> Self {
        let tables = Box::new(Tables {
            dc: [Huffman::empty(), Huffman::empty(), Huffman::empty(), Huffman::empty()],
            ac: [Huffman::empty(), Huffman::empty(), Huffman::empty(), Huffman::empty()],
            quant: [[0; 64]; 4],
        });
        Self { tables, planes: Default::default(), up: Default::default() }
    }

    /// Reads the headers up to the start of the scan, and loads the tables the scan uses.
    #[allow(clippy::similar_names)]
    pub(super) fn parse(&mut self, jpeg: &[u8]) -> Result<Parsed, &'static str> {
        if jpeg.get(..2) != Some(&[0xFF, 0xD8]) {
            return Err("not a JPEG");
        }
        let (mut have_dc, mut have_ac, mut have_q) = ([false; 4], [false; 4], [false; 4]);
        let mut frame: Option<Frame> = None;
        let mut restart = 0;
        let mut adobe_rgb = false;
        let mut i = 2;
        loop {
            // The next marker, past any fill bytes (and, as libjpeg tolerates, stray bytes before it).
            while i < jpeg.len() && jpeg[i] != 0xFF {
                i += 1;
            }
            while i + 1 < jpeg.len() && jpeg[i + 1] == 0xFF {
                i += 1;
            }
            let marker = byte(jpeg, i + 1)?;
            i += 2;
            match marker {
                0x00 | 0x01 | 0xD0..=0xD8 => continue,
                0xD9 => return Err("no image before the end"),
                _ => {}
            }
            let len = be16(jpeg, i)?;
            let seg = jpeg.get(i + 2..i + len).ok_or("truncated header")?;
            i += len;
            match marker {
                0xC0 | 0xC1 => {
                    if frame.is_some() {
                        return Err("two frame headers");
                    }
                    match read_frame(seg)? {
                        Some(f) => frame = Some(f),
                        None => return Ok(Parsed::Unsupported),
                    }
                }
                // Progressive, lossless, hierarchical and arithmetic-coded frames.
                0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return Ok(Parsed::Unsupported),
                0xC4 => self.read_huffman(seg, &mut have_dc, &mut have_ac)?,
                0xDB => self.read_quant(seg, &mut have_q)?,
                0xDD => restart = be16(seg, 0)?,
                // Adobe's APP14 with transform 0: the three components are RGB, not YCbCr.
                0xEE => adobe_rgb = seg.len() >= 12 && seg.starts_with(b"Adobe") && seg[11] == 0,
                0xDA => {
                    let mut f = frame.ok_or("scan before the frame header")?;
                    if f.ncomp == 3 && (adobe_rgb || f.comps.iter().map(|c| c.id).eq(*b"RGB")) {
                        return Ok(Parsed::Unsupported);
                    }
                    if !read_scan(seg, &mut f)? {
                        return Ok(Parsed::Unsupported);
                    }
                    f.restart = restart;
                    f.scan = i;
                    self.default_tables(&f, have_dc, have_ac, have_q)?;
                    return Ok(Parsed::Frame(f));
                }
                _ => {}
            }
        }
    }

    fn read_huffman(&mut self, mut seg: &[u8], dc: &mut [bool; 4], ac: &mut [bool; 4]) -> Result<(), &'static str> {
        while !seg.is_empty() {
            let class_slot = seg[0];
            let counts: &[u8; 16] = seg.get(1..17).and_then(|c| c.try_into().ok()).ok_or("truncated header")?;
            let n: usize = counts.iter().map(|&c| usize::from(c)).sum();
            let symbols = seg.get(17..17 + n).ok_or("truncated header")?;
            let slot = usize::from(class_slot & 0x0F);
            if slot > 3 {
                return Err("bad Huffman table slot");
            }
            match class_slot >> 4 {
                0 => {
                    self.tables.dc[slot].build(counts, symbols)?;
                    dc[slot] = true;
                }
                1 => {
                    self.tables.ac[slot].build(counts, symbols)?;
                    ac[slot] = true;
                }
                _ => return Err("bad Huffman table class"),
            }
            seg = &seg[17 + n..];
        }
        Ok(())
    }

    fn read_quant(&mut self, mut seg: &[u8], have: &mut [bool; 4]) -> Result<(), &'static str> {
        while !seg.is_empty() {
            let slot = usize::from(seg[0] & 0x0F);
            let wide = seg[0] >> 4 == 1;
            if slot > 3 || seg[0] >> 4 > 1 {
                return Err("bad quantisation table");
            }
            let size = if wide { 128 } else { 64 };
            let values = seg.get(1..1 + size).ok_or("truncated header")?;
            let q = &mut self.tables.quant[slot];
            if wide {
                for (q, b) in q.iter_mut().zip(values.as_chunks::<2>().0) {
                    *q = u16::from_be_bytes([b[0], b[1]]);
                }
            } else {
                for (q, &b) in q.iter_mut().zip(values) {
                    *q = u16::from(b);
                }
            }
            have[slot] = true;
            seg = &seg[1 + size..];
        }
        Ok(())
    }

    /// Fills the Huffman slots the scan uses but the frame did not send with the Annex K tables (slot 0 luminance,
    /// slot 1 chrominance), as MJPG expects.
    fn default_tables(&mut self, f: &Frame, dc: [bool; 4], ac: [bool; 4], q: [bool; 4]) -> Result<(), &'static str> {
        for c in &f.comps[..f.ncomp] {
            if !q[c.tq] {
                return Err("missing quantisation table");
            }
            if !dc[c.td] {
                match c.td {
                    0 => self.tables.dc[0].build(&standard::DC_LUMA_BITS, &standard::DC_LUMA_VALUES)?,
                    1 => self.tables.dc[1].build(&standard::DC_CHROMA_BITS, &standard::DC_CHROMA_VALUES)?,
                    _ => return Err("missing Huffman table"),
                }
            }
            if !ac[c.ta] {
                match c.ta {
                    0 => self.tables.ac[0].build(&standard::AC_LUMA_BITS, &standard::AC_LUMA_VALUES)?,
                    1 => self.tables.ac[1].build(&standard::AC_CHROMA_BITS, &standard::AC_CHROMA_VALUES)?,
                    _ => return Err("missing Huffman table"),
                }
            }
        }
        Ok(())
    }

    /// Decodes the scan of `f` (from [`parse`](Self::parse) on the same `jpeg`) and writes packed RGB8 to `out`, which
    /// holds at least `3 * width * height` bytes.
    pub(super) fn decode(&mut self, f: &Frame, jpeg: &[u8], out: &mut [u8]) -> Result<(), &'static str> {
        let (hmax, vmax) = (f.comps[0].h, f.comps[0].v);
        let mcus_x = f.width.div_ceil(8 * hmax);
        let mcus_y = f.height.div_ceil(8 * vmax);
        let mut strides = [0; 3];
        for (k, c) in f.comps[..f.ncomp].iter().enumerate() {
            strides[k] = mcus_x * c.h * 8;
            grow(&mut self.planes[k], strides[k] * mcus_y * c.v * 8);
        }
        let data = jpeg.get(f.scan..).ok_or("truncated scan")?;
        let mut bits = Bits::new(data);
        let mut pred = [0_i32; 3];
        let mut left = f.restart;
        let mut coef: Block = [0; 64];
        let tables = &*self.tables;
        for my in 0..mcus_y {
            for mx in 0..mcus_x {
                if f.restart > 0 {
                    if left == 0 {
                        bits.restart()?;
                        pred = [0; 3];
                        left = f.restart;
                    }
                    left -= 1;
                }
                for (k, c) in f.comps[..f.ncomp].iter().enumerate() {
                    let (dc, ac, q) = (&tables.dc[c.td], &tables.ac[c.ta], &tables.quant[c.tq]);
                    let stride = strides[k];
                    for v in 0..c.v {
                        for h in 0..c.h {
                            let has_ac = block(&mut bits, dc, ac, q, &mut pred[k], &mut coef)?;
                            let at = (my * c.v + v) * 8 * stride + (mx * c.h + h) * 8;
                            let dst = self.planes[k].get_mut(at..).ok_or("plane too small")?;
                            if has_ac {
                                idct(&coef, dst, stride);
                            } else {
                                idct_dc(coef[0], dst, stride);
                            }
                        }
                    }
                }
            }
        }
        if bits.overran() {
            return Err("data ends early");
        }
        self.write_rgb(f, &strides, out)
    }

    fn write_rgb(&mut self, f: &Frame, strides: &[usize; 3], out: &mut [u8]) -> Result<(), &'static str> {
        let (w, h) = (f.width, f.height);
        let rows = out.chunks_exact_mut(3 * w).take(h);
        let [p0, p1, p2] = &self.planes;
        let y_rows = p0.chunks(strides[0]).map(|r| &r[..w.min(r.len())]);
        if f.ncomp == 1 {
            for (o, y) in rows.zip(y_rows) {
                grey_to_rgb(y, o);
            }
            return Ok(());
        }
        let (hmax, vmax) = (f.comps[0].h, f.comps[0].v);
        let cw = w.div_ceil(hmax);
        let ch = h.div_ceil(vmax);
        let cs = strides[1];
        grow(&mut self.up[0], 2 * cw);
        grow(&mut self.up[1], 2 * cw);
        let [u0, u1] = &mut self.up;
        for (yi, (o, y)) in rows.zip(y_rows).enumerate() {
            match (hmax, vmax) {
                (1, 1) => ycc_to_rgb(y, chroma_row(p1, yi, cs, cw)?, chroma_row(p2, yi, cs, cw)?, o),
                (2, 1) => {
                    h2v1(chroma_row(p1, yi, cs, cw)?, u0);
                    h2v1(chroma_row(p2, yi, cs, cw)?, u1);
                    ycc_to_rgb(y, &u0[..w], &u1[..w], o);
                }
                _ => {
                    let near = yi / 2;
                    let far = if yi % 2 == 0 { near.saturating_sub(1) } else { (near + 1).min(ch - 1) };
                    h2v2(chroma_row(p1, near, cs, cw)?, chroma_row(p1, far, cs, cw)?, u0);
                    h2v2(chroma_row(p2, near, cs, cw)?, chroma_row(p2, far, cs, cw)?, u1);
                    ycc_to_rgb(y, &u0[..w], &u1[..w], o);
                }
            }
        }
        Ok(())
    }
}

/// Row `row` of a chroma plane `stride` bytes wide, trimmed to the `width` real samples.
fn chroma_row(plane: &[u8], row: usize, stride: usize, width: usize) -> Result<&[u8], &'static str> {
    plane.get(row * stride..row * stride + width).ok_or("plane too small")
}

/// Reads SOF0 or SOF1. `None` for a valid frame this decoder leaves to zune-jpeg.
fn read_frame(seg: &[u8]) -> Result<Option<Frame>, &'static str> {
    let precision = byte(seg, 0)?;
    let height = be16(seg, 1)?;
    let width = be16(seg, 3)?;
    let ncomp = usize::from(byte(seg, 5)?);
    if precision != 8 || height == 0 || width == 0 || (ncomp != 1 && ncomp != 3) {
        return Ok(None);
    }
    let mut comps = [Component::default(); 3];
    for (k, c) in comps[..ncomp].iter_mut().enumerate() {
        let at = 6 + 3 * k;
        let hv = byte(seg, at + 1)?;
        let tq = usize::from(byte(seg, at + 2)?);
        let (h, v) = (usize::from(hv >> 4), usize::from(hv & 0x0F));
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 {
            return Err("bad frame header");
        }
        *c = Component { id: byte(seg, at)?, h, v, tq, td: 0, ta: 0 };
    }
    if ncomp == 1 {
        // One component is coded block by block, whatever its sampling factors say.
        comps[0].h = 1;
        comps[0].v = 1;
    } else {
        let luma = (comps[0].h, comps[0].v);
        let chroma_full = comps[1..].iter().all(|c| (c.h, c.v) == (1, 1));
        if !matches!(luma, (1, 1) | (2, 1 | 2)) || !chroma_full {
            return Ok(None);
        }
    }
    Ok(Some(Frame { width, height, comps, ncomp, restart: 0, scan: 0 }))
}

/// Reads SOS into `f`'s table slots. `false` for a scan this decoder leaves to zune-jpeg (a partial or progressive
/// scan, or components out of frame order).
fn read_scan(seg: &[u8], f: &mut Frame) -> Result<bool, &'static str> {
    let ns = usize::from(byte(seg, 0)?);
    if ns != f.ncomp {
        return Ok(false);
    }
    for k in 0..ns {
        let id = byte(seg, 1 + 2 * k)?;
        let tables = byte(seg, 2 + 2 * k)?;
        let (td, ta) = (usize::from(tables >> 4), usize::from(tables & 0x0F));
        if td > 3 || ta > 3 {
            return Err("bad scan header");
        }
        if f.comps[k].id != id {
            return Ok(false);
        }
        f.comps[k].td = td;
        f.comps[k].ta = ta;
    }
    let at = 1 + 2 * ns;
    Ok((byte(seg, at)?, byte(seg, at + 1)?, byte(seg, at + 2)?) == (0, 63, 0))
}

/// Decodes one block's coefficients, dequantised, into `coef` (natural order). Returns whether any AC coefficient was
/// coded (when none is, the block is flat and needs no transform).
#[inline]
fn block(
    bits: &mut Bits<'_>,
    dc: &Huffman,
    ac: &Huffman,
    q: &[u16; 64],
    pred: &mut i32,
    coef: &mut Block,
) -> Result<bool, &'static str> {
    *coef = [0; 64];
    let t = dc.decode(bits)?;
    if t > 11 {
        return Err("bad DC difference");
    }
    *pred = pred.wrapping_add(bits.receive_extend(t));
    coef[0] = dequantise(*pred, q[0]);
    let mut has_ac = false;
    let mut k = 1;
    while k < 64 {
        if let Some((run, value)) = ac.decode_ac_fast(bits) {
            k += run;
            if k > 63 {
                return Err("bad AC run");
            }
            coef[usize::from(NATURAL[k])] = dequantise(value, q[k]);
            has_ac = true;
            k += 1;
            continue;
        }
        let rs = ac.decode(bits)?;
        let (run, size) = (usize::from(rs >> 4), rs & 0x0F);
        if size == 0 {
            if run == 15 {
                k += 16;
                continue;
            }
            break;
        }
        k += run;
        if k > 63 {
            return Err("bad AC run");
        }
        coef[usize::from(NATURAL[k])] = dequantise(bits.receive_extend(size), q[k]);
        has_ac = true;
        k += 1;
    }
    Ok(has_ac)
}

/// A coefficient times its quantiser, as the 16 bits the transform works in (a valid JPEG never needs more; a corrupt
/// one saturates).
#[inline]
#[allow(clippy::cast_possible_truncation)] // clamped to the i16 range first
fn dequantise(v: i32, q: u16) -> i16 {
    v.saturating_mul(i32::from(q)).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
