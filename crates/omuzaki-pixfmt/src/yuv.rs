//! NV12 and YUY2 to RGB8 in Q14 fixed point.
//!
//! Chroma is nearest-neighbour: each 2x2 block of an NV12 frame, and each pixel pair of a YUY2 frame, shares one
//! U/V sample. No interpolation, which is plenty for a face tracker and keeps the loop vectorisable.

use crate::{ConvertError, Plane, RgbOut, Yuv, YuvMatrix, YuvRange, check_out, check_plane, rows};

/// Fractional bits of the fixed-point coefficients.
const Q: u32 = 14;
const ONE: f64 = (1u32 << Q) as f64;

/// The matrix and range folded into integer coefficients, so a pixel is four multiplies and a clamp.
///
/// Matrix constants (Kr, Kb) are from ITU-R BT.601-7 and BT.709-6; the coefficients derive from them:
/// `R = Y + 2(1-Kr)V`, `B = Y + 2(1-Kb)U`, `G = Y - (2Kb(1-Kb)/Kg)U - (2Kr(1-Kr)/Kg)V`, with `Kg = 1-Kr-Kb`.
/// Limited range scales luma by 255/219 and chroma by 255/224 (Y 16..235, C 16..240 in 8 bits).
struct Coeffs {
    y_off: i32,
    y_mul: i32,
    r_v: i32,
    g_u: i32,
    g_v: i32,
    b_u: i32,
}

impl Coeffs {
    fn new(yuv: Yuv) -> Self {
        let (kr, kb) = match yuv.matrix {
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let (y_off, y_scale, c_scale) = match yuv.range {
            YuvRange::Limited => (16, 255.0 / 219.0, 255.0 / 224.0),
            YuvRange::Full => (0, 1.0, 1.0),
        };
        #[allow(clippy::cast_possible_truncation)] // every coefficient is below 3 * 2^14
        let q = |v: f64| (v * ONE).round() as i32;
        Self {
            y_off,
            y_mul: q(y_scale),
            r_v: q(2.0 * (1.0 - kr) * c_scale),
            g_u: q(2.0 * kb * (1.0 - kb) / kg * c_scale),
            g_v: q(2.0 * kr * (1.0 - kr) / kg * c_scale),
            b_u: q(2.0 * (1.0 - kb) * c_scale),
        }
    }

    /// The chroma contribution to R, G and B (Q14) of one U/V sample, shared by every pixel that uses it.
    #[inline]
    fn chroma(&self, u: u8, v: u8) -> [i32; 3] {
        let (u, v) = (i32::from(u) - 128, i32::from(v) - 128);
        [self.r_v * v, -(self.g_u * u + self.g_v * v), self.b_u * u]
    }

    /// One pixel from its luma and the shared chroma: rounds half up and clamps to 0..=255.
    #[inline]
    fn pixel(&self, y: u8, chroma: [i32; 3]) -> [u8; 3] {
        const ROUND: i32 = 1 << (Q - 1);
        let luma = (i32::from(y) - self.y_off) * self.y_mul + ROUND;
        chroma.map(|c| clamp_u8((luma + c) >> Q))
    }

    /// Converts `LANES` pixels (`LANES / 2` U/V samples) at once. The fixed-size arrays and the planar R, G and B
    /// intermediates are what lets the compiler turn the maths into vector code; interleaving is the last step.
    #[inline]
    fn block(&self, y: &[u8; LANES], u: &[u8; LANES / 2], v: &[u8; LANES / 2], out: &mut [u8; LANES * 3]) {
        const ROUND: i32 = 1 << (Q - 1);
        let mut chroma = [[0i32; LANES]; 3];
        for i in 0..LANES / 2 {
            let c = self.chroma(u[i], v[i]);
            for (plane, c) in chroma.iter_mut().zip(c) {
                plane[2 * i] = c;
                plane[2 * i + 1] = c;
            }
        }
        let mut rgb = [[0u8; LANES]; 3];
        for (plane, chroma) in rgb.iter_mut().zip(&chroma) {
            for i in 0..LANES {
                let luma = (i32::from(y[i]) - self.y_off) * self.y_mul + ROUND;
                plane[i] = clamp_u8((luma + chroma[i]) >> Q);
            }
        }
        for (i, px) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            px.copy_from_slice(&[rgb[0][i], rgb[1][i], rgb[2][i]]);
        }
    }

    /// Two horizontally adjacent pixels that share one U/V sample, as six RGB bytes (the tail after whole blocks).
    fn pair(&self, y0: u8, y1: u8, u: u8, v: u8, out: &mut [u8]) {
        let chroma = self.chroma(u, v);
        out[..3].copy_from_slice(&self.pixel(y0, chroma));
        out[3..].copy_from_slice(&self.pixel(y1, chroma));
    }
}

/// Clamps to 0..=255.
#[inline]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // the clamp makes the cast exact
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Pixels per vector block.
const LANES: usize = 32;

/// Converts one NV12 row: `y` has `w` luma bytes, `uv` has `w` interleaved U,V bytes shared with the row below or above.
fn nv12_row(c: &Coeffs, y: &[u8], uv: &[u8], out: &mut [u8]) {
    let (y_blocks, y_tail) = y.as_chunks::<LANES>();
    let (uv_blocks, uv_tail) = uv.as_chunks::<LANES>();
    let (out_blocks, out_tail) = out.as_chunks_mut::<{ LANES * 3 }>();
    for ((y, uv), out) in y_blocks.iter().zip(uv_blocks).zip(out_blocks) {
        let (mut u, mut v) = ([0; LANES / 2], [0; LANES / 2]);
        for ((u, v), pair) in u.iter_mut().zip(&mut v).zip(uv.as_chunks::<2>().0.iter()) {
            (*u, *v) = (pair[0], pair[1]);
        }
        c.block(y, &u, &v, out);
    }
    for ((y, uv), out) in y_tail
        .as_chunks::<2>()
        .0
        .iter()
        .zip(uv_tail.as_chunks::<2>().0.iter())
        .zip(out_tail.as_chunks_mut::<6>().0.iter_mut())
    {
        c.pair(y[0], y[1], uv[0], uv[1], out);
    }
}

/// Converts one YUY2 row of `Y0 U Y1 V` groups.
fn yuy2_row(c: &Coeffs, src: &[u8], out: &mut [u8]) {
    let (src_blocks, src_tail) = src.as_chunks::<{ LANES * 2 }>();
    let (out_blocks, out_tail) = out.as_chunks_mut::<{ LANES * 3 }>();
    for (src, out) in src_blocks.iter().zip(out_blocks) {
        let (mut y, mut u, mut v) = ([0; LANES], [0; LANES / 2], [0; LANES / 2]);
        for (i, group) in src.as_chunks::<4>().0.iter().enumerate() {
            (y[2 * i], u[i], y[2 * i + 1], v[i]) = (group[0], group[1], group[2], group[3]);
        }
        c.block(&y, &u, &v, out);
    }
    for (px, out) in src_tail.as_chunks::<4>().0.iter().zip(out_tail.as_chunks_mut::<6>().0.iter_mut()) {
        c.pair(px[0], px[2], px[1], px[3], out);
    }
}

/// Converts an NV12 frame (a luma plane, then one plane of interleaved U,V at half width and height) to RGB8.
///
/// `uv` rows are `width` bytes (`width / 2` U,V pairs). Chroma is nearest-neighbour per 2x2 block.
///
/// # Errors
/// [`ConvertError::BadSize`] for a zero or odd width or height, [`ConvertError::ShortPlane`] or
/// [`ConvertError::ShortOutput`] for a buffer or stride that cannot hold the frame.
pub fn nv12_to_rgb(y: Plane<'_>, uv: Plane<'_>, yuv: Yuv, out: &mut RgbOut<'_>) -> Result<(), ConvertError> {
    let (w, h, out_row) = check_out(out, true, true)?;
    check_plane("y", y, w, h)?;
    check_plane("uv", uv, w, h / 2)?;
    let c = Coeffs::new(yuv);
    let uv_rows = rows(uv, w, h / 2).flat_map(|row| [row, row]);
    for ((y, uv), out) in rows(y, w, h).zip(uv_rows).zip(out.data.chunks_exact_mut(out_row)) {
        nv12_row(&c, y, uv, out);
    }
    Ok(())
}

/// Converts a YUY2 (YUYV 4:2:2) frame, bytes `Y0 U Y1 V` per pixel pair, to RGB8.
///
/// Chroma is nearest-neighbour: both pixels of a pair share the pair's U and V.
///
/// # Errors
/// [`ConvertError::BadSize`] for a zero or odd width or a zero height, [`ConvertError::ShortPlane`] or
/// [`ConvertError::ShortOutput`] for a buffer or stride that cannot hold the frame.
pub fn yuy2_to_rgb(src: Plane<'_>, yuv: Yuv, out: &mut RgbOut<'_>) -> Result<(), ConvertError> {
    let (w, h, out_row) = check_out(out, true, false)?;
    check_plane("src", src, w * 2, h)?;
    let c = Coeffs::new(yuv);
    for (row, out) in rows(src, w * 2, h).zip(out.data.chunks_exact_mut(out_row)) {
        yuy2_row(&c, row, out);
    }
    Ok(())
}
