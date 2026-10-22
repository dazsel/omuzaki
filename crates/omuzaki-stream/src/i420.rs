//! Straight-alpha RGBA to the two 4:2:0 pictures a VP9 take with alpha codes: the colour (BT.709, studio range, the
//! matrix the `WebM` track declares) and the alpha as the luma of a second picture (full range, chroma at 128).
//!
//! Chroma is averaged over each 2×2 block weighted by alpha, so the colour of a transparent pixel (black, after
//! unpremultiplying) does not darken the edge pixels it shares a block with. A fully transparent pixel takes its
//! block's weighted luma for the same reason: the codec blurs across the edge, and what it blurs in is then the
//! character's colour, not black. Fixed-point Q16 coefficients from Kr = 0.2126, Kb = 0.0722, scaled to 219 and 224
//! levels; each chroma row sums to zero so greys stay neutral.

use crate::vp9::I420;

/// Luma coefficients (R, G, B), Q16.
const Y_COEF: [i64; 3] = [11_966, 40_254, 4_064];
/// Blue-difference coefficients, Q16.
const U_COEF: [i64; 3] = [-6_596, -22_188, 28_784];
/// Red-difference coefficients, Q16.
const V_COEF: [i64; 3] = [28_784, -26_145, -2_639];
const ONE: i64 = 1 << 16;

fn div_round(n: i64, d: i64) -> i64 {
    if n >= 0 { (n + d / 2) / d } else { -((-n + d / 2) / d) }
}

fn clamp_u8(v: i64) -> u8 {
    u8::try_from(v.clamp(0, 255)).unwrap_or(u8::MAX)
}

/// The studio-range luma of one colour.
#[must_use]
pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    let sum = Y_COEF[0] * i64::from(r) + Y_COEF[1] * i64::from(g) + Y_COEF[2] * i64::from(b);
    clamp_u8((sum + 16 * ONE + ONE / 2) >> 16)
}

/// Converts straight RGBA8 `rgba` of `width × height` into `colour` and `alpha`, resizing both, splitting the rows
/// across up to `threads` threads. Returns `false`, and leaves both untouched, when `rgba` is not `width × height × 4`
/// bytes.
pub fn rgba_to_i420(rgba: &[u8], width: u32, height: u32, colour: &mut I420, alpha: &mut I420) -> bool {
    rgba_to_i420_threads(rgba, width, height, colour, alpha, 1)
}

/// [`rgba_to_i420`] on up to `threads` threads, each converting a band of whole 2×2 block rows.
pub fn rgba_to_i420_threads(
    rgba: &[u8],
    width: u32,
    height: u32,
    colour: &mut I420,
    alpha: &mut I420,
    threads: usize,
) -> bool {
    let (w, h) = (width as usize, height as usize);
    if rgba.len() != w * h * 4 {
        return false;
    }
    colour.resize(width, height);
    alpha.resize(width, height);
    alpha.u.fill(128);
    alpha.v.fill(128);
    if w == 0 || h == 0 {
        return true;
    }
    let cw = w.div_ceil(2);
    let block_rows = h.div_ceil(2);
    let per_band = block_rows.div_ceil(threads.clamp(1, block_rows));
    let bands = rgba
        .chunks(per_band * 2 * w * 4)
        .zip(colour.y.chunks_mut(per_band * 2 * w))
        .zip(alpha.y.chunks_mut(per_band * 2 * w))
        .zip(colour.u.chunks_mut(per_band * cw))
        .zip(colour.v.chunks_mut(per_band * cw))
        .map(|((((rgba, y), ay), u), v)| Band { rgba, w, y, ay, u, v });
    std::thread::scope(|scope| {
        let mut bands = bands;
        let first = bands.next();
        for band in bands {
            scope.spawn(move || band.convert());
        }
        if let Some(band) = first {
            band.convert();
        }
    });
    true
}

/// A run of whole block rows (the last may be one pixel row high), with the plane rows it writes.
struct Band<'a> {
    rgba: &'a [u8],
    w: usize,
    y: &'a mut [u8],
    ay: &'a mut [u8],
    u: &'a mut [u8],
    v: &'a mut [u8],
}

impl Band<'_> {
    fn convert(self) {
        let Band { rgba, w, y, ay, u, v } = self;
        let h = y.len() / w;
        let cw = w.div_ceil(2);
        let mut planes = Planes { y, ay, u, v };
        // Whole 2×2 blocks take the fast path; a last odd row or column goes through the general one.
        for by in 0..h / 2 {
            let (r0, r1) = (by * 2 * w, (by * 2 + 1) * w);
            for bx in 0..w / 2 {
                let x = bx * 2;
                fast_block([r0 + x, r0 + x + 1, r1 + x, r1 + x + 1], by * cw + bx, rgba, &mut planes);
            }
        }
        for by in 0..h.div_ceil(2) {
            let first_bx = if by < h / 2 { w / 2 } else { 0 };
            for bx in first_bx..cw {
                general_block(bx, by, w, h, rgba, &mut planes);
            }
        }
    }
}

/// The planes a band writes: colour luma, alpha, and the colour's chroma.
struct Planes<'a> {
    y: &'a mut [u8],
    ay: &'a mut [u8],
    u: &'a mut [u8],
    v: &'a mut [u8],
}

/// The luma of one pixel, in `i32` for the hot loop.
#[inline]
fn luma_i32(p: &[u8]) -> u8 {
    // At most 56_284 × 255 plus the offset: far inside `i32`.
    let sum = 11_966 * i32::from(p[0]) + 40_254 * i32::from(p[1]) + 4_064 * i32::from(p[2]);
    u8::try_from(((sum + (16 << 16) + (1 << 15)) >> 16).clamp(0, 255)).unwrap_or(u8::MAX)
}

/// One whole 2×2 block at pixel indices `px`, its chroma at `ci`.
#[inline]
fn fast_block(px: [usize; 4], ci: usize, rgba: &[u8], out: &mut Planes<'_>) {
    let p = px.map(|i| &rgba[i * 4..i * 4 + 4]);
    let a = p.map(|q| q[3]);
    for k in 0..4 {
        out.y[px[k]] = luma_i32(p[k]);
        out.ay[px[k]] = a[k];
    }
    if a[0] == a[1] && a[0] == a[2] && a[0] == a[3] {
        // Equal weights (every opaque and every empty block): the weighted mean is the plain mean.
        let sums = [0, 1, 2].map(|c| p.iter().map(|q| i64::from(q[c])).sum::<i64>());
        let dot = |coef: &[i64; 3]| coef[0] * sums[0] + coef[1] * sums[1] + coef[2] * sums[2];
        out.u[ci] = clamp_u8(128 + ((dot(&U_COEF) + 2 * ONE) >> 18));
        out.v[ci] = clamp_u8(128 + ((dot(&V_COEF) + 2 * ONE) >> 18));
        return;
    }
    let weight: i64 = a.iter().map(|v| i64::from(*v)).sum();
    let sums = [0, 1, 2].map(|c| p.iter().map(|q| i64::from(q[c]) * i64::from(q[3])).sum::<i64>());
    let dot = |coef: &[i64; 3]| coef[0] * sums[0] + coef[1] * sums[1] + coef[2] * sums[2];
    out.u[ci] = clamp_u8(128 + div_round(dot(&U_COEF), weight * ONE));
    out.v[ci] = clamp_u8(128 + div_round(dot(&V_COEF), weight * ONE));
    if a.contains(&0) {
        let fill = clamp_u8(16 + div_round(dot(&Y_COEF), weight * ONE));
        for k in 0..4 {
            if a[k] == 0 {
                out.y[px[k]] = fill;
            }
        }
    }
}

/// A block at the right or bottom edge, which may hold fewer than four pixels.
fn general_block(bx: usize, by: usize, width: usize, height: usize, rgba: &[u8], out: &mut Planes<'_>) {
    let cw = width.div_ceil(2);
    let rows = (by * 2)..(by * 2 + 2).min(height);
    let cols = (bx * 2)..(bx * 2 + 2).min(width);
    // Weighted sums (by alpha) and plain sums, for blocks with no alpha at all.
    let (mut weighted, mut weight) = ([0i64; 3], 0i64);
    let (mut plain, mut count) = ([0i64; 3], 0i64);
    for row in rows.clone() {
        for col in cols.clone() {
            let idx = row * width + col;
            let px = &rgba[idx * 4..idx * 4 + 4];
            let a = i64::from(px[3]);
            for c in 0..3 {
                weighted[c] += i64::from(px[c]) * a;
                plain[c] += i64::from(px[c]);
            }
            weight += a;
            count += 1;
            out.y[idx] = luma(px[0], px[1], px[2]);
            out.ay[idx] = px[3];
        }
    }
    let (sums, total) = if weight > 0 { (weighted, weight) } else { (plain, count) };
    let dot = |coef: &[i64; 3]| coef[0] * sums[0] + coef[1] * sums[1] + coef[2] * sums[2];
    let ci = by * cw + bx;
    out.u[ci] = clamp_u8(128 + div_round(dot(&U_COEF), total * ONE));
    out.v[ci] = clamp_u8(128 + div_round(dot(&V_COEF), total * ONE));
    if weight > 0 && weight < 255 * count {
        let fill = clamp_u8(16 + div_round(dot(&Y_COEF), total * ONE));
        for row in rows.clone() {
            for col in cols.clone() {
                let idx = row * width + col;
                if rgba[idx * 4 + 3] == 0 {
                    out.y[idx] = fill;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::many_single_char_names, clippy::cast_possible_truncation, clippy::assert_is_empty)] // colour maths in its own letters; test values wrap on purpose
mod tests {
    use super::*;

    fn reference(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
        let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let u = (b - y) / 1.8556;
        let v = (r - y) / 1.5748;
        (16.0 + y * 219.0 / 255.0, 128.0 + u * 224.0 / 255.0, 128.0 + v * 224.0 / 255.0)
    }

    fn convert(rgba: &[u8], w: u32, h: u32) -> (I420, I420) {
        let (mut colour, mut alpha) = (I420::default(), I420::default());
        assert!(rgba_to_i420(rgba, w, h, &mut colour, &mut alpha));
        (colour, alpha)
    }

    #[test]
    fn opaque_colours_match_the_bt709_formula_within_one_level() {
        let colours =
            [[0, 0, 0], [255, 255, 255], [128, 128, 128], [255, 0, 0], [0, 255, 0], [0, 0, 255], [12, 200, 77]];
        for [r, g, b] in colours {
            let (colour, alpha) = convert(&[r, g, b, 255].repeat(4), 2, 2);
            let (ry, ru, rv) = reference(r, g, b);
            for (got, want) in [(colour.y[0], ry), (colour.u[0], ru), (colour.v[0], rv)] {
                assert!((f64::from(got) - want).abs() <= 1.0, "{r},{g},{b}: got {got}, want {want:.2}");
            }
            assert_eq!(alpha.y, [255; 4]);
        }
        let (grey, _) = convert(&[90, 90, 90, 255].repeat(4), 2, 2);
        assert_eq!((grey.u[0], grey.v[0]), (128, 128), "greys stay neutral");
    }

    #[test]
    fn transparent_pixels_take_the_blocks_colour() {
        // One opaque red pixel and three transparent ones (black after unpremultiplying).
        let mut rgba = vec![0u8; 16];
        rgba[..4].copy_from_slice(&[255, 0, 0, 255]);
        let (colour, alpha) = convert(&rgba, 2, 2);
        let (_, ru, rv) = reference(255, 0, 0);
        assert!((f64::from(colour.u[0]) - ru).abs() <= 1.0, "chroma is the red pixel's, not darkened");
        assert!((f64::from(colour.v[0]) - rv).abs() <= 1.0);
        assert_eq!(colour.y, [luma(255, 0, 0); 4], "the transparent pixels carry the red luma");
        assert_eq!(alpha.y, [255, 0, 0, 0]);
        assert_eq!((alpha.u[0], alpha.v[0]), (128, 128));
    }

    #[test]
    fn odd_sizes_cover_every_pixel() {
        let rgba: Vec<u8> = (0..3 * 3).flat_map(|i| [i * 20, 100, 200, 255]).collect();
        let (colour, alpha) = convert(&rgba, 3, 3);
        assert_eq!((colour.y.len(), colour.u.len()), (9, 4));
        assert_eq!(alpha.y.len(), 9);
        let (_, ru, _) = reference(160, 100, 200);
        assert!((f64::from(colour.u[3]) - ru).abs() <= 1.0, "the corner block holds just the last pixel");
    }

    #[test]
    fn threads_change_nothing() {
        for (w, h) in [(7u32, 9u32), (64, 36), (5, 1), (1, 6)] {
            let rgba: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7) as u8, (i * 13) as u8, 90, (i * 31) as u8]).collect();
            let one = convert(&rgba, w, h);
            for threads in [2, 3, 8] {
                let (mut colour, mut alpha) = (I420::default(), I420::default());
                assert!(rgba_to_i420_threads(&rgba, w, h, &mut colour, &mut alpha, threads));
                assert_eq!((&colour, &alpha), (&one.0, &one.1), "{w}x{h} on {threads} threads");
            }
        }
    }

    #[test]
    fn a_wrong_length_is_refused() {
        let (mut colour, mut alpha) = (I420::default(), I420::default());
        assert!(!rgba_to_i420(&[0; 12], 2, 2, &mut colour, &mut alpha));
        assert!(colour.y.is_empty());
    }
}
