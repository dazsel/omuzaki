//! Whether an embedded ICC profile is the sRGB profile, from its data: the colour space and connection space, the three
//! primaries, the white point and the three tone curves. The profile's description is metadata, and is not read for this.
//! Every count, offset and size is checked against the bytes actually present before it is used, so a forged profile
//! yields `false` quickly: the tag table is capped, and each walk over it is bounded by that cap.

use libm::pow;

/// The sRGB primaries as CIE XYZ under the D50 white the ICC uses: the red, green and blue columns, and the white.
const RED_XYZ: [f64; 3] = [0.4361, 0.2225, 0.0139];
const GREEN_XYZ: [f64; 3] = [0.3851, 0.7169, 0.0971];
const BLUE_XYZ: [f64; 3] = [0.1431, 0.0606, 0.7141];
const D50_WHITE: [f64; 3] = [0.9642, 1.0, 0.8249];
const XYZ_TOLERANCE: f64 = 0.002;

/// The sRGB curve as a parametric curve (type 3): gamma, a, b, c, d. Stored as `s15Fixed16`, so the tolerance is the
/// fixed-point step, not a judgement.
const SRGB_PARAMETERS: [f64; 5] = [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045];
const PARAMETER_TOLERANCE: f64 = 1e-4;

/// The most tags a profile may declare. Real profiles declare a few dozen; the cap bounds every walk over the table.
pub(crate) const MAX_TAGS: usize = 256;

/// The fewest and most points a sampled tone curve may have. A table with fewer points is not a faithful sRGB curve (an
/// identity or a two-point stand-in is refused by its count alone); the most bounds the work per channel.
const MIN_CURVE_SAMPLES: usize = 256;
const MAX_CURVE_SAMPLES: usize = 4096;

/// How far a sampled curve's value may stray from sRGB at any point: one step of an 8-bit channel.
const CURVE_TOLERANCE: f64 = 1.0 / 255.0;

/// Whether the profile describes sRGB: an RGB profile with an XYZ connection space, sRGB's primaries and D50 white, and
/// the sRGB curve on each channel (a parametric curve, or a sampled curve within one 8-bit step of it at every point).
pub(crate) fn is_srgb(profile: &[u8]) -> bool {
    if profile.get(16..20) != Some(b"RGB ".as_slice()) || profile.get(20..24) != Some(b"XYZ ".as_slice()) {
        return false;
    }
    let Some(count) = tag_count(profile) else { return false };
    let tag = |signature: [u8; 4]| find_tag(profile, count, signature);
    let xyz_matches = |signature: [u8; 4], want: [f64; 3]| {
        tag(signature)
            .and_then(xyz)
            .is_some_and(|got| got.iter().zip(want).all(|(got, want)| (got - want).abs() <= XYZ_TOLERANCE))
    };
    let curve_matches = |signature: [u8; 4]| tag(signature).is_some_and(is_srgb_curve);
    // A lookup-table transform (`A2B0`, `A2B1`, `A2B2`) takes priority over the matrix and curves when a colour is
    // converted, and defines device colours this check does not read: a profile with one is not shown to be sRGB.
    let has_lookup_table =
        [*b"A2B0", *b"A2B1", *b"A2B2"].into_iter().any(|signature| declares_tag(profile, count, signature));
    !has_lookup_table
        && xyz_matches(*b"rXYZ", RED_XYZ)
        && xyz_matches(*b"gXYZ", GREEN_XYZ)
        && xyz_matches(*b"bXYZ", BLUE_XYZ)
        && xyz_matches(*b"wtpt", D50_WHITE)
        && curve_matches(*b"rTRC")
        && curve_matches(*b"gTRC")
        && curve_matches(*b"bTRC")
}

/// The tag count at byte 128, refused when it is over [`MAX_TAGS`] or over the complete 12-byte entries present. Checked
/// before any entry is read, so a forged count costs nothing.
fn tag_count(profile: &[u8]) -> Option<usize> {
    let count = usize::try_from(be_u32(profile, 128)?).ok()?;
    let present = profile.len().saturating_sub(132) / 12;
    (count <= MAX_TAGS && count <= present).then_some(count)
}

/// Whether the table declares a tag with `signature`, whatever its offset and size say.
fn declares_tag(profile: &[u8], count: usize, signature: [u8; 4]) -> bool {
    (0..count).any(|entry| {
        let at = 132 + 12 * entry;
        profile.get(at..at + 4) == Some(signature.as_slice())
    })
}

/// The data of the tag with `signature`, found by scanning the `count` entries of the table in place: no entry is
/// collected, and an entry whose bytes are not present is skipped.
fn find_tag(profile: &[u8], count: usize, signature: [u8; 4]) -> Option<&[u8]> {
    (0..count).find_map(|entry| {
        let at = 132 + 12 * entry;
        if profile.get(at..at + 4)? != signature.as_slice() {
            return None;
        }
        let offset = usize::try_from(be_u32(profile, at + 4)?).ok()?;
        let size = usize::try_from(be_u32(profile, at + 8)?).ok()?;
        profile.get(offset..offset.checked_add(size)?)
    })
}

/// The XYZ values of an `XYZ ` tag: three `s15Fixed16` numbers after its type and reserved words.
fn xyz(data: &[u8]) -> Option<[f64; 3]> {
    if data.get(..4)? != b"XYZ ".as_slice() {
        return None;
    }
    Some(std::array::from_fn(|i| fixed(data, 8 + 4 * i)))
}

/// Whether a tone curve is the sRGB curve: a parametric type 3 with sRGB's parameters, or a sampled curve of
/// [`MIN_CURVE_SAMPLES`] to [`MAX_CURVE_SAMPLES`] points that is within [`CURVE_TOLERANCE`] of sRGB's decode at every
/// point. The curve maps an encoded value to linear light, as the parametric one does. A gamma, an identity or any other
/// curve is refused.
fn is_srgb_curve(data: &[u8]) -> bool {
    match data.get(..4) {
        Some(b"para") => {
            if data.get(8..10) != Some([0_u8, 3].as_slice()) || data.len() < 12 + 4 * SRGB_PARAMETERS.len() {
                return false;
            }
            SRGB_PARAMETERS
                .iter()
                .enumerate()
                .all(|(i, &want)| (fixed(data, 12 + 4 * i) - want).abs() <= PARAMETER_TOLERANCE)
        }
        Some(b"curv") => {
            let Some(count) = be_u32(data, 8).and_then(|count| usize::try_from(count).ok()) else { return false };
            if !(MIN_CURVE_SAMPLES..=MAX_CURVE_SAMPLES).contains(&count) || data.len() < 12 + 2 * count {
                return false;
            }
            #[allow(clippy::cast_precision_loss, reason = "the count is at most 4096, exact in f64")]
            (0..count).all(|i| {
                let value = f64::from(u16::from_be_bytes([data[12 + 2 * i], data[13 + 2 * i]])) / 65_535.0;
                // A table index of at most 4096 is exact in f64.
                let encoded = i as f64 / (count - 1) as f64;
                (value - srgb_decode(encoded)).abs() <= CURVE_TOLERANCE
            })
        }
        _ => false,
    }
}

/// The sRGB transfer function, encoded value to linear light, for `x` in [0, 1]: the inverse of the encoding.
fn srgb_decode(x: f64) -> f64 {
    if x <= 0.040_45 { x / 12.92 } else { pow((x + 0.055) / 1.055, 2.4) }
}

/// An `s15Fixed16` number at `at`, or NaN when it is past the end, which fails every comparison.
fn fixed(bytes: &[u8], at: usize) -> f64 {
    be_u32(bytes, at).map_or(f64::NAN, |raw| f64::from(raw.cast_signed()) / 65_536.0)
}

/// Reads a big-endian `u32` at `at`, or `None` when it is past the end.
fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(bytes.get(at..end)?.try_into().ok()?))
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test tables and fixed-point numbers are small and in range"
)]
mod tests {
    use super::{MAX_TAGS, is_srgb};

    /// The `s15Fixed16` bytes of a number.
    fn s15(value: f64) -> [u8; 4] {
        ((value * 65_536.0).round() as i32).to_be_bytes()
    }

    fn xyz_tag(value: [f64; 3]) -> Vec<u8> {
        let mut tag = b"XYZ \0\0\0\0".to_vec();
        for &v in &value {
            tag.extend(s15(v));
        }
        tag
    }

    fn parametric_tag(parameters: [f64; 5]) -> Vec<u8> {
        let mut tag = b"para\0\0\0\0\0\x03\0\0".to_vec();
        for &v in &parameters {
            tag.extend(s15(v));
        }
        tag
    }

    /// The sRGB transfer function as the reference, written independently of the code under test: the encoding by its
    /// definition, and the decode as its inverse.
    fn reference_encode(linear: f64) -> f64 {
        if linear <= 0.003_130_8 { 12.92 * linear } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 }
    }

    fn reference_decode(encoded: f64) -> f64 {
        if encoded <= 0.040_45 { encoded / 12.92 } else { ((encoded + 0.055) / 1.055).powf(2.4) }
    }

    /// A sampled curve of `entries` points for `value(position)`, where the position runs over [0, 1]: 16 bits each.
    fn sampled_tag(entries: usize, value: impl Fn(f64) -> f64) -> Vec<u8> {
        let mut tag = b"curv\0\0\0\0".to_vec();
        tag.extend(u32::try_from(entries).unwrap().to_be_bytes());
        for i in 0..entries {
            let sample = (value(i as f64 / (entries - 1) as f64) * 65_535.0).round() as u16;
            tag.extend(sample.to_be_bytes());
        }
        tag
    }

    /// A profile of the given tags: a 128-byte header (RGB, XYZ), the table, then each tag's data in turn.
    fn profile(tags: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0; 128];
        out[16..20].copy_from_slice(b"RGB ");
        out[20..24].copy_from_slice(b"XYZ ");
        out.extend(u32::try_from(tags.len()).unwrap().to_be_bytes());
        let mut offset = 132 + 12 * tags.len();
        for (signature, data) in tags {
            out.extend(*signature);
            out.extend(u32::try_from(offset).unwrap().to_be_bytes());
            out.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
            offset += data.len();
        }
        for (_, data) in tags {
            out.extend(data);
        }
        out
    }

    const SRGB_RED: [f64; 3] = [0.4361, 0.2225, 0.0139];
    const SRGB_GREEN: [f64; 3] = [0.3851, 0.7169, 0.0971];
    const SRGB_BLUE: [f64; 3] = [0.1431, 0.0606, 0.7141];
    const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

    /// The profile of sRGB, with the given curve on each channel.
    fn srgb_with(curve: &[u8]) -> Vec<u8> {
        profile(&[
            (b"rXYZ", xyz_tag(SRGB_RED)),
            (b"gXYZ", xyz_tag(SRGB_GREEN)),
            (b"bXYZ", xyz_tag(SRGB_BLUE)),
            (b"wtpt", xyz_tag(D50)),
            (b"rTRC", curve.to_vec()),
            (b"gTRC", curve.to_vec()),
            (b"bTRC", curve.to_vec()),
        ])
    }

    fn srgb_parametric() -> Vec<u8> {
        srgb_with(&parametric_tag([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045]))
    }

    #[test]
    fn a_lookup_table_transform_beside_the_srgb_matrix_and_curves_is_not_srgb() {
        let curve = parametric_tag([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045]);
        for lut in [b"A2B0", b"A2B1", b"A2B2"] {
            let profile = profile(&[
                (b"rXYZ", xyz_tag(SRGB_RED)),
                (b"gXYZ", xyz_tag(SRGB_GREEN)),
                (b"bXYZ", xyz_tag(SRGB_BLUE)),
                (b"wtpt", xyz_tag(D50)),
                (b"rTRC", curve.clone()),
                (b"gTRC", curve.clone()),
                (b"bTRC", curve.clone()),
                (lut, b"mft2\0\0\0\0".to_vec()),
            ]);
            assert!(!is_srgb(&profile), "{}", String::from_utf8_lossy(lut));
        }
    }

    #[test]
    fn a_tag_counts_as_declared_whatever_its_offset_and_size_say() {
        // One table entry: `A2B0` at an offset far past the end of the profile.
        let mut table = vec![0_u8; 128];
        table.extend(1_u32.to_be_bytes());
        table.extend(b"A2B0");
        table.extend(u32::MAX.to_be_bytes());
        table.extend(u32::MAX.to_be_bytes());
        assert!(super::declares_tag(&table, 1, *b"A2B0"));
        assert!(!super::declares_tag(&table, 1, *b"A2B1"));
        assert!(super::find_tag(&table, 1, *b"A2B0").is_none(), "its data is not there to be read");
    }

    #[test]
    fn a_profile_with_srgb_primaries_white_and_curve_is_srgb() {
        assert!(is_srgb(&srgb_parametric()));
        // A sampled sRGB curve maps encoded values to linear light: the decode, not the encode.
        assert!(is_srgb(&srgb_with(&sampled_tag(1024, reference_decode))));
    }

    #[test]
    fn a_sampled_curve_in_the_encode_direction_is_refused() {
        // The same samples as the encode direction map linear light back to encoded values: a reversed table.
        assert!(!is_srgb(&srgb_with(&sampled_tag(1024, reference_encode))));
    }

    #[test]
    fn a_sparse_or_identity_table_is_refused_by_its_count() {
        // Two points (an identity) match the endpoints of the sRGB decode, and nothing else: refused by count.
        assert!(!is_srgb(&srgb_with(&sampled_tag(2, |x| x))));
        assert!(!is_srgb(&srgb_with(&sampled_tag(255, reference_decode))));
    }

    #[test]
    fn a_sampled_curve_that_strays_inside_the_table_is_refused() {
        // Right at both ends and wrong in the middle: a gamma of 1.8 stands in for sRGB between the endpoints.
        assert!(!is_srgb(&srgb_with(&sampled_tag(1024, |x| x.powf(1.8)))));
    }

    #[test]
    fn a_display_p3_matrix_is_refused_even_when_its_description_says_srgb() {
        // Display P3 (D50-adapted): wider red and green, same curve and white. Its tags say P3, whatever it is called.
        let curve = parametric_tag([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045]);
        let p3 = profile(&[
            (b"rXYZ", xyz_tag([0.5151, 0.2412, -0.0011])),
            (b"gXYZ", xyz_tag([0.2919, 0.6922, 0.0419])),
            (b"bXYZ", xyz_tag([0.1572, 0.0666, 0.7841])),
            (b"wtpt", xyz_tag(D50)),
            (b"rTRC", curve.clone()),
            (b"gTRC", curve.clone()),
            (b"bTRC", curve),
            (b"desc", b"desc\0\0\0\0\0\0\0\x11sRGB IEC61966-2.1\0".to_vec()),
        ]);
        assert!(!is_srgb(&p3));
    }

    #[test]
    fn a_gamma_curve_or_an_identity_curve_is_refused() {
        let gamma_22 = b"curv\0\0\0\0\0\0\0\x01\x02\x33".to_vec();
        assert!(!is_srgb(&srgb_with(&gamma_22)));
        let identity = b"curv\0\0\0\0\0\0\0\0".to_vec();
        assert!(!is_srgb(&srgb_with(&identity)));
    }

    #[test]
    fn a_profile_that_is_not_rgb_or_has_no_tags_is_refused() {
        let mut gray = srgb_parametric();
        gray[16..20].copy_from_slice(b"GRAY");
        assert!(!is_srgb(&gray));
        assert!(!is_srgb(&[0; 100]));
    }

    #[test]
    fn a_header_claiming_millions_of_tags_is_refused_before_the_table_is_walked() {
        // The count claims five million tags; the profile holds one. Refused by the cap before any entry is read.
        let mut forged = srgb_parametric();
        forged[128..132].copy_from_slice(&5_000_000_u32.to_be_bytes());
        assert!(!is_srgb(&forged));
        // Just over the cap is refused too, even with the entries present.
        let mut over = srgb_parametric();
        over[128..132].copy_from_slice(&((MAX_TAGS + 1) as u32).to_be_bytes());
        assert!(!is_srgb(&over));
    }

    #[test]
    fn a_forged_tag_offset_or_size_is_refused() {
        // The first entry's data runs past the end of the profile.
        let mut forged = srgb_parametric();
        forged[136..140].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(!is_srgb(&forged));
        let mut forged = srgb_parametric();
        forged[140..144].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(!is_srgb(&forged));
    }

    #[test]
    fn a_forged_curve_count_is_refused() {
        // A table that claims four billion points with none present.
        let mut curve = b"curv\0\0\0\0".to_vec();
        curve.extend(u32::MAX.to_be_bytes());
        curve.extend([0_u8; 8]);
        assert!(!is_srgb(&srgb_with(&curve)));
    }

    #[test]
    fn a_truncated_parametric_curve_is_refused() {
        let truncated = srgb_with(&parametric_tag([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])[..16]);
        assert!(!is_srgb(&truncated));
    }
}
