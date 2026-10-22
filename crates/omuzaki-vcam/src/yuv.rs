//! Opaque RGBA8 to packed YUYV (4:2:2), the format webcams send and every camera consumer reads.
//!
//! BT.601 matrix, limited range (Y 16 to 235, Cb and Cr 16 to 240), as USB webcams use, in Q8 fixed point. Each
//! pair of pixels shares one chroma sample, the average of the pair's chroma. The picture arrives already flattened
//! over the backdrop (the stream converts once for every opaque output), so alpha is ignored.

/// `round(x / 256)` for the Q8 sums below, then the range offset.
fn q8(sum: i32, offset: i32) -> u8 {
    u8::try_from(((sum + 128) >> 8) + offset).unwrap_or(if sum < 0 { 0 } else { u8::MAX })
}

/// Luma of one pixel: `16 + (66R + 129G + 25B) / 256`.
pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    q8(66 * r + 129 * g + 25 * b, 16)
}

/// Chroma of one colour: `(128 + (−38R − 74G + 112B) / 256, 128 + (112R − 94G − 18B) / 256)`.
pub fn chroma(r: i32, g: i32, b: i32) -> (u8, u8) {
    (q8(-38 * r - 74 * g + 112 * b, 128), q8(112 * r - 94 * g - 18 * b, 128))
}

/// Bytes of a YUYV frame of `width` × `height`.
pub fn yuyv_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 2
}

/// Converts `rgba` (`width` × `height`, `width` even) into `out` as YUYV. `out` must hold [`yuyv_len`] bytes.
///
/// Returns `false`, writing nothing, when the width is odd or a buffer has the wrong length.
pub fn rgba_to_yuyv(rgba: &[u8], width: u32, height: u32, out: &mut [u8]) -> bool {
    let pixels = width as usize * height as usize;
    if !width.is_multiple_of(2) || rgba.len() != pixels * 4 || out.len() != yuyv_len(width, height) {
        return false;
    }
    for (pair, dst) in rgba.as_chunks::<8>().0.iter().zip(out.as_chunks_mut::<4>().0.iter_mut()) {
        let (r0, g0, b0) = (pair[0], pair[1], pair[2]);
        let (r1, g1, b1) = (pair[4], pair[5], pair[6]);
        // The pair's average colour, kept at double scale so the halving rounds once, inside `q8`.
        let sums = [i32::from(r0) + i32::from(r1), i32::from(g0) + i32::from(g1), i32::from(b0) + i32::from(b1)];
        let (cb, cr) = chroma_doubled(sums);
        *dst = [luma(r0, g0, b0), cb, luma(r1, g1, b1), cr];
    }
    true
}

/// [`chroma`] of the average of two pixels, given their summed red, green and blue.
fn chroma_doubled([red, green, blue]: [i32; 3]) -> (u8, u8) {
    let half = |sum: i32| (sum + 256) >> 9;
    let cb = half(-38 * red - 74 * green + 112 * blue) + 128;
    let cr = half(112 * red - 94 * green - 18 * blue) + 128;
    (u8::try_from(cb.clamp(0, 255)).unwrap_or(0), u8::try_from(cr.clamp(0, 255)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inverse, BT.601 limited range, in floats: what a camera consumer computes.
    fn to_rgb(y: u8, u: u8, v: u8) -> [f32; 3] {
        let (y, u, v) = (f32::from(y) - 16.0, f32::from(u) - 128.0, f32::from(v) - 128.0);
        [1.164 * y + 1.596 * v, 1.164 * y - 0.392 * u - 0.813 * v, 1.164 * y + 2.017 * u]
    }

    #[test]
    fn black_white_and_grey_hit_the_limited_range() {
        assert_eq!((luma(0, 0, 0), chroma(0, 0, 0)), (16, (128, 128)));
        assert_eq!((luma(255, 255, 255), chroma(255, 255, 255)), (235, (128, 128)));
        assert_eq!(chroma(128, 128, 128), (128, 128), "grey has no chroma");
    }

    #[test]
    fn the_primaries_and_the_key_green_round_trip_within_two_levels() {
        for rgb in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [0, 177, 64], [250, 211, 197]] {
            let mut out = [0u8; 4];
            let px = [rgb[0], rgb[1], rgb[2], 255, rgb[0], rgb[1], rgb[2], 255];
            assert!(rgba_to_yuyv(&px, 2, 1, &mut out));
            let back = to_rgb(out[0], out[1], out[3]);
            for c in 0..3 {
                assert!((back[c] - f32::from(rgb[c])).abs() <= 2.5, "{rgb:?} came back as {back:?}");
            }
            assert_eq!(out[0], out[2], "both pixels of a flat pair have the same luma");
        }
    }

    #[test]
    fn a_pair_shares_the_average_chroma() {
        let px = [255, 0, 0, 255, 0, 0, 255, 255];
        let mut out = [0u8; 4];
        assert!(rgba_to_yuyv(&px, 2, 1, &mut out));
        let (u, v) = chroma(128, 0, 128);
        assert!(u.abs_diff(out[1]) <= 1 && v.abs_diff(out[3]) <= 1, "{out:?} against {u} {v}");
    }

    #[test]
    fn odd_widths_and_wrong_lengths_are_refused() {
        assert!(!rgba_to_yuyv(&[0; 12], 3, 1, &mut [0; 6]));
        assert!(!rgba_to_yuyv(&[0; 8], 2, 1, &mut [0; 3]));
        assert_eq!(yuyv_len(1920, 1080), 4_147_200);
    }
}
