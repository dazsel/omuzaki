//! Alpha handling for the outputs and record formats (`docs/design/record.md` D9).
//!
//! The renderer's target is premultiplied, sRGB-encoded RGBA8. Each output says which form it wants
//! ([`AlphaNeed`]); the stream converts once per distinct need, off the render thread, with the exact integer formulas
//! below. These CPU functions are the reference a later GPU pass must match byte for byte.
//!
//! | Need | Function | Formula on the stored 8-bit values |
//! |---|---|---|
//! | Straight (PNG, VP9, NDI) | [`unpremultiply`] | `a == 0 → 0`, else `min(255, (c·255 + a/2) / a)` |
//! | Opaque (virtual camera, MP4) | [`flatten_over`] | `c + (key·(255 − a) + 127) / 255`, alpha 255 |
//! | Premultiplied (Spout, recorders' input) | none | — |

use omuzaki_core::colour::Colour;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream_api::output::AlphaNeed;

/// One premultiplied channel value made straight: `0` where alpha is `0`, else `min(255, (c·255 + a/2) / a)`.
#[must_use]
pub fn unpremultiply_u8(c: u8, a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    let (c, a) = (u32::from(c), u32::from(a));
    // At most 255 by the `min`, so the narrowing below is exact.
    u8::try_from(((c * 255 + a / 2) / a).min(255)).unwrap_or(u8::MAX)
}

/// One premultiplied channel value composited over `key`: `c + (key·(255 − a) + 127) / 255`, at most 255.
#[must_use]
pub fn flatten_u8(c: u8, a: u8, key: u8) -> u8 {
    let under = (u32::from(key) * (255 - u32::from(a)) + 127) / 255;
    u8::try_from((u32::from(c) + under).min(255)).unwrap_or(u8::MAX)
}

/// Writes the straight-alpha form of the premultiplied pixels `src` into `dst` (same length, RGBA8).
/// A length mismatch converts the shorter prefix only; the callers size both from one image.
pub fn unpremultiply(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len(), dst.len(), "source and destination differ in length");
    for (s, d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
        let a = s[3];
        d[0] = unpremultiply_u8(s[0], a);
        d[1] = unpremultiply_u8(s[1], a);
        d[2] = unpremultiply_u8(s[2], a);
        d[3] = a;
    }
}

/// Writes the premultiplied pixels `src` composited over the opaque colour `key` into `dst` (same length, RGBA8, alpha
/// 255).
pub fn flatten_over(src: &[u8], dst: &mut [u8], key: Colour) {
    debug_assert_eq!(src.len(), dst.len(), "source and destination differ in length");
    let (kr, kg, kb) = (key.r, key.g, key.b);
    for (s, d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
        let a = s[3];
        d[0] = flatten_u8(s[0], a, kr);
        d[1] = flatten_u8(s[1], a, kg);
        d[2] = flatten_u8(s[2], a, kb);
        d[3] = 255;
    }
}

/// Converts a premultiplied `src` into `dst` in the form `need` asks for, reusing `dst`'s allocation when the size is
/// unchanged. A `src` that is already straight is copied for a straight need and premultiplied-flattened otherwise
/// through the same formulas after premultiplying, so every path ends in the documented form.
pub fn convert_into(src: &Image, need: AlphaNeed, key: Colour, dst: &mut Image) {
    dst.size = src.size;
    dst.rgba8.resize(src.rgba8.len(), 0);
    match (src.alpha, need) {
        (AlphaEncoding::Premultiplied, AlphaNeed::Premultiplied) | (AlphaEncoding::Straight, AlphaNeed::Straight) => {
            dst.rgba8.copy_from_slice(&src.rgba8);
        }
        (AlphaEncoding::Premultiplied, AlphaNeed::Straight) => unpremultiply(&src.rgba8, &mut dst.rgba8),
        (AlphaEncoding::Premultiplied, AlphaNeed::Opaque) => flatten_over(&src.rgba8, &mut dst.rgba8, key),
        (AlphaEncoding::Straight, AlphaNeed::Premultiplied) => premultiply(&src.rgba8, &mut dst.rgba8),
        (AlphaEncoding::Straight, AlphaNeed::Opaque) => {
            premultiply(&src.rgba8, &mut dst.rgba8);
            flatten_in_place(&mut dst.rgba8, key);
        }
    }
    dst.alpha = match need {
        AlphaNeed::Straight | AlphaNeed::Opaque => AlphaEncoding::Straight,
        AlphaNeed::Premultiplied => AlphaEncoding::Premultiplied,
    };
}

/// Premultiplies straight pixels: `(c·a + 127) / 255`.
fn premultiply(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
        let a = u32::from(s[3]);
        for i in 0..3 {
            d[i] = u8::try_from((u32::from(s[i]) * a + 127) / 255).unwrap_or(u8::MAX);
        }
        d[3] = s[3];
    }
}

fn flatten_in_place(px: &mut [u8], key: Colour) {
    let (kr, kg, kb) = (key.r, key.g, key.b);
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3];
        p[0] = flatten_u8(p[0], a, kr);
        p[1] = flatten_u8(p[1], a, kg);
        p[2] = flatten_u8(p[2], a, kb);
        p[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_core::glam::UVec2;

    #[test]
    fn unpremultiply_matches_the_formula_and_its_edges() {
        assert_eq!(unpremultiply_u8(0, 0), 0);
        assert_eq!(unpremultiply_u8(200, 0), 0, "colour under zero alpha is meaningless and reads as black");
        assert_eq!(unpremultiply_u8(128, 255), 128);
        assert_eq!(unpremultiply_u8(64, 128), 128);
        // A premultiplied value above its alpha is invalid input; it clamps rather than wrapping.
        assert_eq!(unpremultiply_u8(200, 100), 255);
    }

    #[test]
    fn unpremultiply_inverts_premultiply_within_rounding() {
        for a in 1..=255u32 {
            for c in (0..=255u32).step_by(5) {
                let pre = u8::try_from((c * a + 127) / 255).unwrap_or(u8::MAX);
                let back = u32::from(unpremultiply_u8(pre, u8::try_from(a).unwrap_or(u8::MAX)));
                // One premultiplied level spans 255/a straight levels, so that is the most the round trip can lose.
                let slack = 255 / a + 1;
                assert!(back.abs_diff(c) <= slack, "c {c} a {a}: back {back}");
            }
        }
    }

    #[test]
    fn flatten_composites_over_the_key() {
        assert_eq!(flatten_u8(0, 0, 177), 177, "fully transparent shows the key");
        assert_eq!(flatten_u8(90, 255, 177), 90, "fully opaque keeps the colour");
        assert_eq!(u32::from(flatten_u8(64, 128, 200)), 64 + (200 * 127 + 127) / 255);
        assert_eq!(flatten_u8(255, 0, 255), 255, "invalid input saturates");
    }

    #[test]
    fn convert_into_picks_the_form_each_need_wants() {
        let src = Image {
            size: UVec2::new(2, 1),
            alpha: AlphaEncoding::Premultiplied,
            rgba8: vec![64, 0, 32, 128, 0, 0, 0, 0],
        };
        let mut dst = Image { size: UVec2::ZERO, alpha: AlphaEncoding::Straight, rgba8: Vec::new() };
        let key = Colour::hex(0x0000_b140);

        convert_into(&src, AlphaNeed::Straight, key, &mut dst);
        assert_eq!(dst.rgba8, [128, 0, 64, 128, 0, 0, 0, 0]);
        assert_eq!(dst.alpha, AlphaEncoding::Straight);

        convert_into(&src, AlphaNeed::Opaque, key, &mut dst);
        let (kr, kg, kb) = (key.r, key.g, key.b);
        assert_eq!(dst.rgba8[3], 255);
        assert_eq!(&dst.rgba8[4..], [kr, kg, kb, 255]);

        convert_into(&src, AlphaNeed::Premultiplied, key, &mut dst);
        assert_eq!(dst.rgba8, src.rgba8);
        assert_eq!(dst.alpha, AlphaEncoding::Premultiplied);
    }
}
