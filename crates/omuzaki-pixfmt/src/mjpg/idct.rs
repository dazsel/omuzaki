//! The inverse DCT: libjpeg's accurate integer algorithm (`jidctint.c`, Loeffler, Ligtenberg and Moschytz with 13-bit
//! constants), so a frame decodes to the same samples libjpeg-turbo gives.
//!
//! On x86-64 it runs as libjpeg-turbo's SSE2 version does (`jidctint-sse2`): each row of eight 16-bit coefficients is
//! one register, and every rotation is one `pmaddwd` on interleaved pairs, which gives exactly the scalar result for
//! any valid JPEG. Elsewhere the scalar version runs. Both wrap or saturate on corrupt input: wrong pixels, never a
//! panic.

// Codec arithmetic: every cast is of a value bounded by construction (a code of at most 16 bits, a clamped or
// shifted sample, a table index under 512), and the tests hold each fast path to its scalar reference.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

const CONST_BITS: u32 = 13;
const PASS1_BITS: u32 = 2;

const FIX_0_298631336: i32 = 2446;
const FIX_0_390180644: i32 = 3196;
const FIX_0_541196100: i32 = 4433;
const FIX_0_765366865: i32 = 6270;
const FIX_0_899976223: i32 = 7373;
const FIX_1_175875602: i32 = 9633;
const FIX_1_501321110: i32 = 12299;
const FIX_1_847759065: i32 = 15137;
const FIX_1_961570560: i32 = 16069;
const FIX_2_053119869: i32 = 16819;
const FIX_2_562915447: i32 = 20995;
const FIX_3_072711026: i32 = 25172;

/// A block of dequantised coefficients in natural (row-major) order.
pub(super) type Block = [i16; 64];

/// Transforms `coef` and writes the 8x8 samples to `out`, rows `stride` apart. `out` holds at least `7 * stride + 8`
/// bytes.
pub(super) fn idct(coef: &Block, out: &mut [u8], stride: usize) {
    #[cfg(target_arch = "x86_64")]
    sse2::idct(coef, out, stride);
    #[cfg(not(target_arch = "x86_64"))]
    scalar(coef, out, stride);
}

/// The samples of a block whose only coefficient is the (dequantised) DC: libjpeg's result, without the transform.
pub(super) fn idct_dc(dc: i16, out: &mut [u8], stride: usize) {
    // Pass 1 gives `dc << PASS1_BITS` down the first column; pass 2 descales that by PASS1_BITS + 3.
    let v = sample(((i32::from(dc) << PASS1_BITS) + (1 << (PASS1_BITS + 2))) >> (PASS1_BITS + 3));
    for row in out.chunks_mut(stride).take(8) {
        if let Some(dst) = row.get_mut(..8) {
            dst.fill(v);
        }
    }
}

fn sample(v: i32) -> u8 {
    v.wrapping_add(128).clamp(0, 255) as u8
}

/// The 1-D transform of eight frequencies into eight samples, descaled by `shift` bits (`jidctint.c`).
#[allow(clippy::similar_names)]
fn pass_1d(x: [i32; 8], shift: u32) -> [i32; 8] {
    let z2 = x[2];
    let z3 = x[6];
    let z1 = z2.wrapping_add(z3).wrapping_mul(FIX_0_541196100);
    let tmp2 = z1.wrapping_add(z3.wrapping_mul(-FIX_1_847759065));
    let tmp3 = z1.wrapping_add(z2.wrapping_mul(FIX_0_765366865));
    let tmp0 = x[0].wrapping_add(x[4]) << CONST_BITS;
    let tmp1 = x[0].wrapping_sub(x[4]) << CONST_BITS;
    let tmp10 = tmp0.wrapping_add(tmp3);
    let tmp13 = tmp0.wrapping_sub(tmp3);
    let tmp11 = tmp1.wrapping_add(tmp2);
    let tmp12 = tmp1.wrapping_sub(tmp2);

    let (t0, t1, t2, t3) = (x[7], x[5], x[3], x[1]);
    let z1 = t0.wrapping_add(t3);
    let z2 = t1.wrapping_add(t2);
    let z3 = t0.wrapping_add(t2);
    let z4 = t1.wrapping_add(t3);
    let z5 = z3.wrapping_add(z4).wrapping_mul(FIX_1_175875602);
    let z1 = z1.wrapping_mul(-FIX_0_899976223);
    let z2 = z2.wrapping_mul(-FIX_2_562915447);
    let z3 = z3.wrapping_mul(-FIX_1_961570560).wrapping_add(z5);
    let z4 = z4.wrapping_mul(-FIX_0_390180644).wrapping_add(z5);
    let t0 = t0.wrapping_mul(FIX_0_298631336).wrapping_add(z1).wrapping_add(z3);
    let t1 = t1.wrapping_mul(FIX_2_053119869).wrapping_add(z2).wrapping_add(z4);
    let t2 = t2.wrapping_mul(FIX_3_072711026).wrapping_add(z2).wrapping_add(z3);
    let t3 = t3.wrapping_mul(FIX_1_501321110).wrapping_add(z1).wrapping_add(z4);

    let d = |v: i32| v.wrapping_add(1 << (shift - 1)) >> shift;
    [
        d(tmp10.wrapping_add(t3)),
        d(tmp11.wrapping_add(t2)),
        d(tmp12.wrapping_add(t1)),
        d(tmp13.wrapping_add(t0)),
        d(tmp13.wrapping_sub(t0)),
        d(tmp12.wrapping_sub(t1)),
        d(tmp11.wrapping_sub(t2)),
        d(tmp10.wrapping_sub(t3)),
    ]
}

/// The scalar transform: columns, then rows, as `jidctint.c` does.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
fn scalar(coef: &Block, out: &mut [u8], stride: usize) {
    let mut ws = [0_i32; 64];
    for c in 0..8 {
        let col = std::array::from_fn(|r| i32::from(coef[r * 8 + c]));
        for (r, v) in pass_1d(col, CONST_BITS - PASS1_BITS).into_iter().enumerate() {
            ws[r * 8 + c] = v;
        }
    }
    for (row, w) in out.chunks_mut(stride).zip(ws.as_chunks::<8>().0) {
        let samples = pass_1d(*w, CONST_BITS + PASS1_BITS + 3);
        if let Some(dst) = row.get_mut(..8) {
            for (o, v) in dst.iter_mut().zip(samples) {
                *o = sample(v);
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod sse2 {
    //! SSE2 is part of the x86-64 baseline, so this always runs there. Every function is `#[target_feature(enable =
    //! "sse2")]`, which makes the intrinsics safe to call inside them; only the entry point needs one `unsafe` call.

    use std::arch::x86_64::{
        __m128i, _mm_add_epi8, _mm_add_epi16, _mm_add_epi32, _mm_cvtsi128_si64, _mm_madd_epi16, _mm_packs_epi16,
        _mm_packs_epi32, _mm_set_epi64x, _mm_set1_epi8, _mm_set1_epi32, _mm_srai_epi32, _mm_sub_epi32,
        _mm_unpackhi_epi16, _mm_unpackhi_epi32, _mm_unpackhi_epi64, _mm_unpacklo_epi16, _mm_unpacklo_epi32,
        _mm_unpacklo_epi64,
    };

    use super::{
        Block, CONST_BITS, FIX_0_298631336, FIX_0_390180644, FIX_0_541196100, FIX_0_765366865, FIX_0_899976223,
        FIX_1_175875602, FIX_1_501321110, FIX_1_847759065, FIX_1_961570560, FIX_2_053119869, FIX_2_562915447,
        FIX_3_072711026, PASS1_BITS,
    };

    /// Runs the SSE2 transform.
    // The crate's one unsafe call: `transform` needs SSE2, which every x86-64 CPU has. It is in the target's baseline,
    // and the assertion below stops the build if a target ever left it out.
    #[allow(unsafe_code)]
    pub(super) fn idct(coef: &Block, out: &mut [u8], stride: usize) {
        const { assert!(cfg!(target_feature = "sse2")) };
        // SAFETY: SSE2 is enabled for this target (checked at compile time above).
        let rows = unsafe { transform(coef) };
        for (row, bytes) in out.chunks_mut(stride).zip(rows) {
            if let Some(dst) = row.get_mut(..8) {
                dst.copy_from_slice(&bytes.to_le_bytes());
            }
        }
    }

    /// Two 16-bit constants as the pair `pmaddwd` multiplies an interleaved `(a, b)` by: `a * lo + b * hi`.
    #[target_feature(enable = "sse2")]
    fn pair(lo: i32, hi: i32) -> __m128i {
        _mm_set1_epi32((hi << 16) | (lo & 0xFFFF))
    }

    /// `a * lo + b * hi` for each lane of `a` and `b`, as two halves of four 32-bit results.
    #[target_feature(enable = "sse2")]
    fn rotate(a: __m128i, b: __m128i, lo: i32, hi: i32) -> [__m128i; 2] {
        let k = pair(lo, hi);
        [_mm_madd_epi16(_mm_unpacklo_epi16(a, b), k), _mm_madd_epi16(_mm_unpackhi_epi16(a, b), k)]
    }

    #[target_feature(enable = "sse2")]
    fn add(a: [__m128i; 2], b: [__m128i; 2]) -> [__m128i; 2] {
        [_mm_add_epi32(a[0], b[0]), _mm_add_epi32(a[1], b[1])]
    }

    #[target_feature(enable = "sse2")]
    fn sub(a: [__m128i; 2], b: [__m128i; 2]) -> [__m128i; 2] {
        [_mm_sub_epi32(a[0], b[0]), _mm_sub_epi32(a[1], b[1])]
    }

    /// Rounds, shifts right by `SHIFT` and packs back to eight 16-bit lanes (saturating).
    #[target_feature(enable = "sse2")]
    fn descale<const SHIFT: i32>(a: [__m128i; 2]) -> __m128i {
        let round = _mm_set1_epi32(1 << (SHIFT - 1));
        _mm_packs_epi32(
            _mm_srai_epi32::<SHIFT>(_mm_add_epi32(a[0], round)),
            _mm_srai_epi32::<SHIFT>(_mm_add_epi32(a[1], round)),
        )
    }

    /// The 1-D transform on eight lanes: `x[k]` holds frequency `k` of each lane; the result's `[n]` is sample `n`.
    /// The rotations are `jidctint.c`'s, regrouped into pairs; integer arithmetic makes the regrouping exact.
    #[allow(clippy::similar_names)]
    #[target_feature(enable = "sse2")]
    fn pass<const SHIFT: i32>(x: [__m128i; 8]) -> [__m128i; 8] {
        let one = 1 << CONST_BITS;
        // Even part.
        let tmp3 = rotate(x[2], x[6], FIX_0_541196100 + FIX_0_765366865, FIX_0_541196100);
        let tmp2 = rotate(x[2], x[6], FIX_0_541196100, FIX_0_541196100 - FIX_1_847759065);
        let tmp0 = rotate(x[0], x[4], one, one);
        let tmp1 = rotate(x[0], x[4], one, -one);
        let tmp10 = add(tmp0, tmp3);
        let tmp13 = sub(tmp0, tmp3);
        let tmp11 = add(tmp1, tmp2);
        let tmp12 = sub(tmp1, tmp2);

        // Odd part.
        let z3 = _mm_add_epi16(x[7], x[3]);
        let z4 = _mm_add_epi16(x[5], x[1]);
        let z3r = rotate(z3, z4, FIX_1_175875602 - FIX_1_961570560, FIX_1_175875602);
        let z4r = rotate(z3, z4, FIX_1_175875602, FIX_1_175875602 - FIX_0_390180644);
        let t0 = add(rotate(x[7], x[1], FIX_0_298631336 - FIX_0_899976223, -FIX_0_899976223), z3r);
        let t3 = add(rotate(x[7], x[1], -FIX_0_899976223, FIX_1_501321110 - FIX_0_899976223), z4r);
        let t1 = add(rotate(x[5], x[3], FIX_2_053119869 - FIX_2_562915447, -FIX_2_562915447), z4r);
        let t2 = add(rotate(x[5], x[3], -FIX_2_562915447, FIX_3_072711026 - FIX_2_562915447), z3r);

        [
            descale::<SHIFT>(add(tmp10, t3)),
            descale::<SHIFT>(add(tmp11, t2)),
            descale::<SHIFT>(add(tmp12, t1)),
            descale::<SHIFT>(add(tmp13, t0)),
            descale::<SHIFT>(sub(tmp13, t0)),
            descale::<SHIFT>(sub(tmp12, t1)),
            descale::<SHIFT>(sub(tmp11, t2)),
            descale::<SHIFT>(sub(tmp10, t3)),
        ]
    }

    /// Transposes eight rows of eight 16-bit lanes.
    #[target_feature(enable = "sse2")]
    fn transpose(r: [__m128i; 8]) -> [__m128i; 8] {
        let a = [
            _mm_unpacklo_epi16(r[0], r[1]),
            _mm_unpackhi_epi16(r[0], r[1]),
            _mm_unpacklo_epi16(r[2], r[3]),
            _mm_unpackhi_epi16(r[2], r[3]),
            _mm_unpacklo_epi16(r[4], r[5]),
            _mm_unpackhi_epi16(r[4], r[5]),
            _mm_unpacklo_epi16(r[6], r[7]),
            _mm_unpackhi_epi16(r[6], r[7]),
        ];
        let b = [
            _mm_unpacklo_epi32(a[0], a[2]),
            _mm_unpackhi_epi32(a[0], a[2]),
            _mm_unpacklo_epi32(a[1], a[3]),
            _mm_unpackhi_epi32(a[1], a[3]),
            _mm_unpacklo_epi32(a[4], a[6]),
            _mm_unpackhi_epi32(a[4], a[6]),
            _mm_unpacklo_epi32(a[5], a[7]),
            _mm_unpackhi_epi32(a[5], a[7]),
        ];
        [
            _mm_unpacklo_epi64(b[0], b[4]),
            _mm_unpackhi_epi64(b[0], b[4]),
            _mm_unpacklo_epi64(b[1], b[5]),
            _mm_unpackhi_epi64(b[1], b[5]),
            _mm_unpacklo_epi64(b[2], b[6]),
            _mm_unpackhi_epi64(b[2], b[6]),
            _mm_unpacklo_epi64(b[3], b[7]),
            _mm_unpackhi_epi64(b[3], b[7]),
        ]
    }

    /// The transform: eight rows of eight samples, each as the little-endian bytes of a `u64`.
    #[target_feature(enable = "sse2")]
    fn transform(c: &Block) -> [u64; 8] {
        let row = |r: usize| {
            let word = |at: usize| {
                c[at..at + 4].iter().rev().fold(0_i64, |acc, &v| (acc << 16) | i64::from(v.cast_unsigned()))
            };
            _mm_set_epi64x(word(r * 8 + 4), word(r * 8))
        };
        let x = [row(0), row(1), row(2), row(3), row(4), row(5), row(6), row(7)];
        // Pass 1 down the columns (each register is one vertical frequency across the eight columns), pass 2 along
        // the rows, then back to rows of samples.
        let ws = pass::<{ (CONST_BITS - PASS1_BITS) as i32 }>(x);
        let px = transpose(pass::<{ (CONST_BITS + PASS1_BITS + 3) as i32 }>(transpose(ws)));
        // Saturate to -128..=127, then add 128: the same as clamping the sample plus 128 to 0..=255.
        let bias = _mm_set1_epi8(-128);
        let mut out = [0_u64; 8];
        for (pair, o) in px.as_chunks::<2>().0.iter().zip(out.as_chunks_mut::<2>().0) {
            let bytes = _mm_add_epi8(_mm_packs_epi16(pair[0], pair[1]), bias);
            o[0] = _mm_cvtsi128_si64(bytes).cast_unsigned();
            o[1] = _mm_cvtsi128_si64(_mm_unpackhi_epi64(bytes, bytes)).cast_unsigned();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xorshift(seed: &mut u32) -> u32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        *seed
    }

    /// The float IDCT the integer one approximates (T.81 A.3.3).
    #[allow(clippy::cast_precision_loss, clippy::many_single_char_names)]
    fn reference(coef: &Block) -> [f64; 64] {
        let c = |u: usize| if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
        let mut out = [0.0; 64];
        for (i, o) in out.iter_mut().enumerate() {
            let (x, y) = (i % 8, i / 8);
            let mut s = 0.0;
            for (k, &v) in coef.iter().enumerate() {
                let (u, w) = (k % 8, k / 8);
                s += c(u)
                    * c(w)
                    * f64::from(v)
                    * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()
                    * (((2 * y + 1) * w) as f64 * std::f64::consts::PI / 16.0).cos();
            }
            *o = (s / 4.0 + 128.0).clamp(0.0, 255.0);
        }
        out
    }

    /// Blocks with the spread of real dequantised coefficients: large low frequencies, small high ones.
    fn blocks(n: usize) -> Vec<Block> {
        let mut seed = 0x2545_F491_u32;
        (0..n)
            .map(|_| {
                std::array::from_fn(|k| {
                    let (u, v) = ((k % 8) as u32, (k / 8) as u32);
                    let range = if k == 0 { 1024 } else { 512 / (1 + u + v) };
                    (xorshift(&mut seed) % (2 * range + 1)) as i16 - range as i16
                })
            })
            .collect()
    }

    #[test]
    fn it_matches_the_float_transform_within_one_level() {
        for coef in blocks(500) {
            let mut out = [0_u8; 64];
            idct(&coef, &mut out, 8);
            let want = reference(&coef);
            for (i, (&got, &want)) in out.iter().zip(&want).enumerate() {
                assert!((f64::from(got) - want).abs() <= 1.0, "sample {i}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn the_fast_transform_is_the_scalar_one_exactly() {
        for coef in blocks(20_000) {
            let (mut fast, mut slow) = ([0_u8; 64], [0_u8; 64]);
            idct(&coef, &mut fast, 8);
            scalar(&coef, &mut slow, 8);
            assert_eq!(fast, slow, "{coef:?}");
        }
    }

    #[test]
    fn the_dc_shortcut_equals_the_transform() {
        for dc in -1100..1100 {
            let mut coef = [0_i16; 64];
            coef[0] = dc;
            let (mut full, mut short) = ([0_u8; 64], [0_u8; 64]);
            idct(&coef, &mut full, 8);
            idct_dc(dc, &mut short, 8);
            assert_eq!(full, short, "dc {dc}");
        }
    }

    #[test]
    fn extreme_coefficients_do_not_panic() {
        let mut out = [0_u8; 64];
        for v in [i16::MAX, i16::MIN] {
            idct(&[v; 64], &mut out, 8);
            scalar(&[v; 64], &mut out, 8);
            idct_dc(v, &mut out, 8);
        }
    }

    #[test]
    #[ignore = "a timing, for development: cargo test --release -p omuzaki-pixfmt --lib speed -- --ignored"]
    fn speed() {
        let coef = blocks(1000);
        let mut out = vec![0_u8; 8 * 1024];
        let t = std::time::Instant::now();
        for i in 0..2_000_000 {
            idct(std::hint::black_box(&coef[i % 1000]), &mut out[(i % 128) * 8..], 1024);
        }
        eprintln!("idct: {:.1} ns/block", t.elapsed().as_secs_f64() * 1e9 / 2e6);
    }
}
