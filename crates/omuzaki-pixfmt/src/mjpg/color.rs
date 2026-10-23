//! Chroma upsampling and YCbCr to RGB, as libjpeg does them by default: "fancy" (triangle-filter) upsampling and the
//! JFIF full-range matrix in 16-bit fixed point (`jdsample.c`, `jdcolor.c`).

// Codec arithmetic: every cast is of a value bounded by construction (a code of at most 16 bits, a clamped or
// shifted sample, a table index under 512), and the tests hold each fast path to its scalar reference.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

/// 2x horizontal upsampling of `src` into `out[..2 * src.len()]` (libjpeg's `h2v1_fancy_upsample`). Each output
/// sample is 3/4 of its nearer input and 1/4 of the further one; the two edge samples copy the edge input.
pub(super) fn h2v1(src: &[u8], out: &mut [u8]) {
    let n = src.len();
    if n == 0 || out.len() < 2 * n {
        return;
    }
    if n == 1 {
        out[..2].fill(src[0]);
        return;
    }
    let c = |i: usize| u16::from(src[i]);
    out[0] = src[0];
    out[1] = ((c(0) * 3 + c(1) + 2) >> 2) as u8;
    #[cfg(target_arch = "x86_64")]
    let from = sse2::h2v1(src, out);
    #[cfg(not(target_arch = "x86_64"))]
    let from = 1;
    for i in from..n - 1 {
        let this = c(i) * 3;
        out[2 * i] = ((this + c(i - 1) + 1) >> 2) as u8;
        out[2 * i + 1] = ((this + c(i + 1) + 2) >> 2) as u8;
    }
    out[2 * n - 2] = ((c(n - 1) * 3 + c(n - 2) + 1) >> 2) as u8;
    out[2 * n - 1] = src[n - 1];
}

/// 2x2 upsampling of one output row (libjpeg's `h2v2_fancy_upsample`): `near` is the chroma row this output row falls
/// in, `far` the chroma row on its other side (the row itself at the image's edge). Writes `out[..2 * near.len()]`.
pub(super) fn h2v2(near: &[u8], far: &[u8], out: &mut [u8]) {
    let n = near.len();
    if n == 0 || far.len() < n || out.len() < 2 * n {
        return;
    }
    // The vertical step: 3/4 of the nearer row and 1/4 of the further, kept at 4x scale.
    let c = |i: usize| u16::from(near[i]) * 3 + u16::from(far[i]);
    if n == 1 {
        out[0] = ((c(0) * 4 + 8) >> 4) as u8;
        out[1] = ((c(0) * 4 + 7) >> 4) as u8;
        return;
    }
    out[0] = ((c(0) * 4 + 8) >> 4) as u8;
    out[1] = ((c(0) * 3 + c(1) + 7) >> 4) as u8;
    #[cfg(target_arch = "x86_64")]
    let from = sse2::h2v2(near, far, out);
    #[cfg(not(target_arch = "x86_64"))]
    let from = 1;
    for i in from..n - 1 {
        let this = c(i) * 3;
        out[2 * i] = ((this + c(i - 1) + 8) >> 4) as u8;
        out[2 * i + 1] = ((this + c(i + 1) + 7) >> 4) as u8;
    }
    out[2 * n - 2] = ((c(n - 1) * 3 + c(n - 2) + 8) >> 4) as u8;
    out[2 * n - 1] = ((c(n - 1) * 4 + 7) >> 4) as u8;
}

const SCALE: u32 = 16;
const HALF: i32 = 1 << (SCALE - 1);
const CR_R: i32 = 91_881; // 1.40200
const CB_B: i32 = 116_130; // 1.77200
const CR_G: i32 = 46_802; // 0.71414
const CB_G: i32 = 22_554; // 0.34414

/// One row of full-resolution Y, Cb and Cr to packed RGB8 (`out` holds `3 * y.len()` bytes).
pub(super) fn ycc_to_rgb(y: &[u8], cb: &[u8], cr: &[u8], out: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    let done = sse2::ycc_to_rgb(y, cb, cr, out);
    #[cfg(not(target_arch = "x86_64"))]
    let done = 0;
    let rest = (3 * done).min(out.len());
    ycc_to_rgb_scalar(tail(y, done), tail(cb, done), tail(cr, done), &mut out[rest..]);
}

fn tail(p: &[u8], from: usize) -> &[u8] {
    &p[from.min(p.len())..]
}

fn ycc_to_rgb_scalar(y: &[u8], cb: &[u8], cr: &[u8], out: &mut [u8]) {
    for (((px, &y), &cb), &cr) in out.as_chunks_mut::<3>().0.iter_mut().zip(y).zip(cb).zip(cr) {
        let y = i32::from(y);
        let cb = i32::from(cb) - 128;
        let cr = i32::from(cr) - 128;
        let r = y + ((CR_R * cr + HALF) >> SCALE);
        let g = y + ((-CB_G * cb + HALF - CR_G * cr) >> SCALE);
        let b = y + ((CB_B * cb + HALF) >> SCALE);
        px[0] = r.clamp(0, 255) as u8;
        px[1] = g.clamp(0, 255) as u8;
        px[2] = b.clamp(0, 255) as u8;
    }
}

#[cfg(target_arch = "x86_64")]
mod sse2 {
    //! The colour conversion sixteen pixels at a time, with the scalar version's exact results: each 17-bit constant
    //! is split into a power of two (a shift) and a 16-bit part (`pmaddwd`), so every product is the same integer.

    use std::arch::x86_64::{
        __m128i, _mm_add_epi16, _mm_add_epi32, _mm_cvtsi128_si64, _mm_madd_epi16, _mm_packs_epi32, _mm_packus_epi16,
        _mm_set_epi64x, _mm_set1_epi16, _mm_set1_epi32, _mm_setzero_si128, _mm_slli_epi16, _mm_srai_epi32,
        _mm_srli_epi16, _mm_sub_epi16, _mm_unpackhi_epi8, _mm_unpackhi_epi16, _mm_unpackhi_epi64, _mm_unpacklo_epi8,
        _mm_unpacklo_epi16,
    };

    use super::{CB_B, CB_G, CR_G, CR_R, HALF};

    /// Converts the longest whole number of 16-pixel groups and returns how many pixels it did.
    // One of the crate's two unsafe calls (the other is the IDCT's): SSE2 is in every x86-64 CPU and in the target's
    // baseline, which the assertion below checks at compile time.
    #[allow(unsafe_code, clippy::many_single_char_names)]
    pub(super) fn ycc_to_rgb(y: &[u8], cb: &[u8], cr: &[u8], out: &mut [u8]) -> usize {
        const { assert!(cfg!(target_feature = "sse2")) };
        let n = y.len().min(cb.len()).min(cr.len()).min(out.len() / 3) / 16 * 16;
        let groups =
            y[..n].as_chunks::<16>().0.iter().zip(cb[..n].as_chunks::<16>().0).zip(cr[..n].as_chunks::<16>().0);
        for (((y, cb), cr), out) in groups.zip(out.as_chunks_mut::<48>().0) {
            // SAFETY: SSE2 is enabled for this target (checked at compile time above).
            let [r, g, b] = unsafe { group(y, cb, cr) };
            for (i, px) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                px[0] = r[i];
                px[1] = g[i];
                px[2] = b[i];
            }
        }
        n
    }

    /// The interior of [`super::h2v1`] eight inputs at a time, from `i = 1`; returns the first `i` it left over.
    #[allow(unsafe_code)]
    pub(super) fn h2v1(src: &[u8], out: &mut [u8]) -> usize {
        const { assert!(cfg!(target_feature = "sse2")) };
        let mut i = 1;
        while i + 9 <= src.len() && 2 * i + 16 <= out.len() {
            // SAFETY: SSE2 is enabled for this target (checked at compile time above).
            let bytes = unsafe { upsample8::<2>(&src[i - 1..i + 9], None, 1, 2) };
            out[2 * i..2 * i + 16].copy_from_slice(&bytes);
            i += 8;
        }
        i
    }

    /// The interior of [`super::h2v2`] eight inputs at a time, from `i = 1`; returns the first `i` it left over.
    #[allow(unsafe_code)]
    pub(super) fn h2v2(near: &[u8], far: &[u8], out: &mut [u8]) -> usize {
        const { assert!(cfg!(target_feature = "sse2")) };
        let mut i = 1;
        while i + 9 <= near.len() && i + 9 <= far.len() && 2 * i + 16 <= out.len() {
            // SAFETY: SSE2 is enabled for this target (checked at compile time above).
            let bytes = unsafe { upsample8::<4>(&near[i - 1..i + 9], Some(&far[i - 1..i + 9]), 8, 7) };
            out[2 * i..2 * i + 16].copy_from_slice(&bytes);
            i += 8;
        }
        i
    }

    /// Eight bytes from `b[at..]` as eight 16-bit lanes.
    #[target_feature(enable = "sse2")]
    fn load8(b: &[u8], at: usize) -> __m128i {
        let mut w = [0_u8; 8];
        if let Some(src) = b.get(at..at + 8) {
            w.copy_from_slice(src);
        }
        _mm_unpacklo_epi8(_mm_set_epi64x(0, i64::from_le_bytes(w)), _mm_setzero_si128())
    }

    /// Sixteen output samples for inputs `1..=8` of the ten in `near` (and `far`, for the 2x2 case, whose vertical
    /// step is `3 near + far`): `(3 c[i] + c[i - 1] + even) >> SHIFT` and `(3 c[i] + c[i + 1] + odd) >> SHIFT`.
    #[target_feature(enable = "sse2")]
    fn upsample8<const SHIFT: i32>(near: &[u8], far: Option<&[u8]>, even: i16, odd: i16) -> [u8; 16] {
        let sums = |at: usize| {
            let n = load8(near, at);
            match far {
                Some(far) => _mm_add_epi16(_mm_add_epi16(_mm_add_epi16(n, n), n), load8(far, at)),
                None => n,
            }
        };
        let (prev, this, next) = (sums(0), sums(1), sums(2));
        let three = _mm_add_epi16(_mm_add_epi16(this, this), this);
        let e = _mm_srli_epi16::<SHIFT>(_mm_add_epi16(_mm_add_epi16(three, prev), _mm_set1_epi16(even)));
        let o = _mm_srli_epi16::<SHIFT>(_mm_add_epi16(_mm_add_epi16(three, next), _mm_set1_epi16(odd)));
        store16(_mm_packus_epi16(_mm_unpacklo_epi16(e, o), _mm_unpackhi_epi16(e, o)))
    }

    #[target_feature(enable = "sse2")]
    fn load16(b: &[u8]) -> __m128i {
        let half = |at: usize| {
            let mut w = [0_u8; 8];
            if let Some(src) = b.get(at..at + 8) {
                w.copy_from_slice(src);
            }
            i64::from_le_bytes(w)
        };
        _mm_set_epi64x(half(8), half(0))
    }

    #[target_feature(enable = "sse2")]
    fn store16(v: __m128i) -> [u8; 16] {
        let mut out = [0_u8; 16];
        out[..8].copy_from_slice(&_mm_cvtsi128_si64(v).to_le_bytes());
        out[8..].copy_from_slice(&_mm_cvtsi128_si64(_mm_unpackhi_epi64(v, v)).to_le_bytes());
        out
    }

    /// `(a * ka + b * kb) >> 16` for eight 16-bit lanes, plus `bias` before the shift.
    #[target_feature(enable = "sse2")]
    fn scaled(a: __m128i, b: __m128i, ka: i32, kb: i32, bias: i32) -> __m128i {
        let k = _mm_set1_epi32((kb << 16) | (ka & 0xFFFF));
        let bias = _mm_set1_epi32(bias);
        let lo = _mm_srai_epi32::<16>(_mm_add_epi32(_mm_madd_epi16(_mm_unpacklo_epi16(a, b), k), bias));
        let hi = _mm_srai_epi32::<16>(_mm_add_epi32(_mm_madd_epi16(_mm_unpackhi_epi16(a, b), k), bias));
        _mm_packs_epi32(lo, hi)
    }

    /// Eight pixels' R, G and B as 16-bit lanes from Y, Cb - 128 and Cr - 128.
    #[target_feature(enable = "sse2")]
    fn eight(y: __m128i, cb: __m128i, cr: __m128i) -> [__m128i; 3] {
        let zero = _mm_setzero_si128();
        // R = y + (CR_R * cr + HALF) >> 16, with CR_R = 65536 + (CR_R - 65536).
        let r = _mm_add_epi16(_mm_add_epi16(y, cr), scaled(cr, zero, CR_R - 65_536, 0, HALF));
        // B = y + (CB_B * cb + HALF) >> 16, with CB_B = 131072 - (131072 - CB_B).
        let b = _mm_add_epi16(_mm_add_epi16(y, _mm_slli_epi16::<1>(cb)), scaled(cb, zero, CB_B - 131_072, 0, HALF));
        // G = y + (-CB_G * cb - CR_G * cr + HALF) >> 16, with CR_G = 65536 - (65536 - CR_G).
        let g = _mm_add_epi16(_mm_sub_epi16(y, cr), scaled(cb, cr, -CB_G, 65_536 - CR_G, HALF));
        [r, g, b]
    }

    #[target_feature(enable = "sse2")]
    fn group(y: &[u8], cb: &[u8], cr: &[u8]) -> [[u8; 16]; 3] {
        let zero = _mm_setzero_si128();
        let centre = _mm_set1_epi16(128);
        let (y, cb, cr) = (load16(y), load16(cb), load16(cr));
        let lo = eight(
            _mm_unpacklo_epi8(y, zero),
            _mm_sub_epi16(_mm_unpacklo_epi8(cb, zero), centre),
            _mm_sub_epi16(_mm_unpacklo_epi8(cr, zero), centre),
        );
        let hi = eight(
            _mm_unpackhi_epi8(y, zero),
            _mm_sub_epi16(_mm_unpackhi_epi8(cb, zero), centre),
            _mm_sub_epi16(_mm_unpackhi_epi8(cr, zero), centre),
        );
        [0, 1, 2].map(|c| store16(_mm_packus_epi16(lo[c], hi[c])))
    }
}

/// One row of grey to packed RGB8.
pub(super) fn grey_to_rgb(y: &[u8], out: &mut [u8]) {
    for (px, &v) in out.as_chunks_mut::<3>().0.iter_mut().zip(y) {
        px.fill(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_chroma_stays_flat_and_edges_copy() {
        let mut out = [0_u8; 8];
        h2v1(&[10, 10, 10, 10], &mut out);
        assert_eq!(out, [10; 8]);
        h2v1(&[0, 100], &mut out[..4]);
        assert_eq!(&out[..4], &[0, 25, 75, 100]);
        h2v2(&[0, 100], &[0, 100], &mut out[..4]);
        assert_eq!(&out[..4], &[0, 25, 75, 100]);
    }

    /// libjpeg's upsampling, written straight from `jdsample.c`.
    fn upsample_reference(c: &[u16], even: u16, odd: u16, shift: u16) -> Vec<u8> {
        (1..c.len() - 1)
            .flat_map(|i| [((c[i] * 3 + c[i - 1] + even) >> shift) as u8, ((c[i] * 3 + c[i + 1] + odd) >> shift) as u8])
            .collect()
    }

    #[test]
    fn the_fast_upsampling_is_the_scalar_one_exactly() {
        let mut seed = 7_u32;
        for n in [2, 3, 9, 10, 11, 17, 40, 641] {
            let src: Vec<u8> = (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    (seed >> 16) as u8
                })
                .collect();
            let far: Vec<u8> = src.iter().rev().copied().collect();
            let mut out = vec![0_u8; 2 * n];
            h2v1(&src, &mut out);
            let c: Vec<u16> = src.iter().map(|&v| u16::from(v)).collect();
            assert_eq!(&out[2..2 * n - 2], &upsample_reference(&c, 1, 2, 2)[..], "h2v1, {n} samples");
            h2v2(&src, &far, &mut out);
            let c: Vec<u16> = src.iter().zip(&far).map(|(&a, &b)| u16::from(a) * 3 + u16::from(b)).collect();
            assert_eq!(&out[2..2 * n - 2], &upsample_reference(&c, 8, 7, 4)[..], "h2v2, {n} samples");
        }
    }

    #[test]
    fn the_fast_conversion_is_the_scalar_one_exactly() {
        // Every Cb and Cr for a spread of Y, sixteen pixels at a time.
        let mut y = Vec::new();
        let (mut cb, mut cr) = (Vec::new(), Vec::new());
        for luma in [0_u8, 1, 16, 77, 128, 200, 235, 254, 255] {
            for b in 0..=255_u8 {
                for r in 0..=255_u8 {
                    y.push(luma);
                    cb.push(b);
                    cr.push(r);
                }
            }
        }
        let (mut fast, mut slow) = (vec![0_u8; y.len() * 3], vec![0_u8; y.len() * 3]);
        ycc_to_rgb(&y, &cb, &cr, &mut fast);
        ycc_to_rgb_scalar(&y, &cb, &cr, &mut slow);
        assert_eq!(fast, slow);
        // A row that is not a whole number of groups finishes in the scalar loop.
        ycc_to_rgb(&y[..37], &cb[..37], &cr[..37], &mut fast[..111]);
        assert_eq!(&fast[..111], &slow[..111]);
    }

    #[test]
    fn grey_and_primaries_convert() {
        let mut out = [0_u8; 9];
        ycc_to_rgb(&[128, 76, 29], &[128, 85, 255], &[128, 255, 107], &mut out);
        assert_eq!(&out[..3], &[128, 128, 128]);
        // JFIF red and blue, to within the rounding of the inputs.
        assert!(out[3] >= 253 && out[4] <= 2 && out[5] <= 2, "{:?}", &out[3..6]);
        assert!(out[6] <= 2 && out[7] <= 2 && out[8] >= 253, "{:?}", &out[6..9]);
    }
}
