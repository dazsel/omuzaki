//! Whether an embedded ICC profile is the sRGB profile, from its data: the colour space and connection space, the three
//! primaries, the white point and the three tone curves. The profile's description is metadata, and is not read for this.
//! Every count, offset and size is checked against the bytes actually present before it is used, so a forged profile
//! yields `false` quickly, never a panic and never a long loop.

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

/// Whether the profile describes sRGB: an RGB profile with an XYZ connection space, sRGB's primaries and D50 white, and
/// the sRGB curve on each channel (as a parametric curve or a table within 1/255 of it at every entry).
pub(crate) fn is_srgb(profile: &[u8]) -> bool {
    if profile.get(16..20) != Some(b"RGB ".as_slice()) || profile.get(20..24) != Some(b"XYZ ".as_slice()) {
        return false;
    }
    let Some(tags) = Tags::read(profile) else { return false };
    let xyz_matches = |signature: [u8; 4], want: [f64; 3]| {
        tags.get(signature)
            .and_then(xyz)
            .is_some_and(|got| got.iter().zip(want).all(|(got, want)| (got - want).abs() <= XYZ_TOLERANCE))
    };
    let curve_matches = |signature: [u8; 4]| tags.get(signature).is_some_and(is_srgb_curve);
    xyz_matches(*b"rXYZ", RED_XYZ)
        && xyz_matches(*b"gXYZ", GREEN_XYZ)
        && xyz_matches(*b"bXYZ", BLUE_XYZ)
        && xyz_matches(*b"wtpt", D50_WHITE)
        && curve_matches(*b"rTRC")
        && curve_matches(*b"gTRC")
        && curve_matches(*b"bTRC")
}

/// The tag table of a profile: each entry's signature and its data, every one checked against the bytes present.
struct Tags<'a> {
    entries: Vec<([u8; 4], &'a [u8])>,
}

impl<'a> Tags<'a> {
    /// Reads the table at byte 128. A count larger than the complete 12-byte entries present is refused before any
    /// entry is read, so a forged count cannot make the reader walk a long, empty table.
    fn read(profile: &'a [u8]) -> Option<Self> {
        let count = usize::try_from(be_u32(profile, 128)?).ok()?;
        let present = profile.len().saturating_sub(132) / 12;
        if count > present {
            return None;
        }
        let mut entries = Vec::with_capacity(count);
        for entry in 0..count {
            let at = 132 + 12 * entry;
            let signature: [u8; 4] = profile.get(at..at + 4)?.try_into().ok()?;
            let offset = usize::try_from(be_u32(profile, at + 4)?).ok()?;
            let size = usize::try_from(be_u32(profile, at + 8)?).ok()?;
            let data = profile.get(offset..offset.checked_add(size)?)?;
            entries.push((signature, data));
        }
        Some(Self { entries })
    }

    fn get(&self, signature: [u8; 4]) -> Option<&'a [u8]> {
        self.entries.iter().find(|(kind, _)| *kind == signature).map(|(_, data)| *data)
    }
}

/// The XYZ values of an `XYZ ` tag: three `s15Fixed16` numbers after its type and reserved words.
fn xyz(data: &[u8]) -> Option<[f64; 3]> {
    if data.get(..4)? != b"XYZ ".as_slice() {
        return None;
    }
    Some(std::array::from_fn(|i| fixed(data, 8 + 4 * i)))
}

/// Whether a tone curve is the sRGB curve: a parametric type 3 with sRGB's parameters, or a table of at least two
/// entries that is within 1/255 of sRGB at every entry. A gamma, an identity or any other curve is not.
#[allow(clippy::cast_precision_loss, reason = "a table index is far below 2^52, so the conversion is exact")]
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
            if count < 2 || data.len() < 12 + 2 * count {
                return false;
            }
            (0..count).all(|i| {
                let value = f64::from(u16::from_be_bytes([data[12 + 2 * i], data[13 + 2 * i]])) / 65_535.0;
                let want = srgb_encode(i as f64 / (count - 1) as f64);
                (value - want).abs() <= 1.0 / 255.0
            })
        }
        _ => false,
    }
}

/// The sRGB transfer function: linear light to the encoded value, for `x` in [0, 1].
fn srgb_encode(x: f64) -> f64 {
    if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * pow(x, 1.0 / 2.4) - 0.055 }
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
    use super::{is_srgb, srgb_encode};

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

    /// A table curve of `entries` points, each the sRGB curve at its place, in 16 bits.
    fn table_tag(entries: usize) -> Vec<u8> {
        let mut tag = b"curv\0\0\0\0".to_vec();
        tag.extend(u32::try_from(entries).unwrap().to_be_bytes());
        for i in 0..entries {
            let value = (srgb_encode(i as f64 / (entries - 1) as f64) * 65_535.0).round() as u16;
            tag.extend(value.to_be_bytes());
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
    fn a_profile_with_srgb_primaries_white_and_curve_is_srgb() {
        assert!(is_srgb(&srgb_parametric()));
        assert!(is_srgb(&srgb_with(&table_tag(1024))));
    }

    #[test]
    fn a_display_p3_matrix_is_refused_even_when_its_description_says_srgb() {
        // Display P3 (D50-adapted): wider red and green, same curve and white. Its tags say P3, whatever it is called.
        let p3 = profile(&[
            (b"rXYZ", xyz_tag([0.5151, 0.2412, -0.0011])),
            (b"gXYZ", xyz_tag([0.2919, 0.6922, 0.0419])),
            (b"bXYZ", xyz_tag([0.1572, 0.0666, 0.7841])),
            (b"wtpt", xyz_tag(D50)),
            (b"rTRC", parametric_tag([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])),
            (b"gTRC", parametric_tag([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])),
            (b"bTRC", parametric_tag([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])),
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
    fn a_forged_tag_count_is_refused_without_walking_the_table() {
        // The count claims four billion entries; only one is present. Refused before any entry is read.
        let mut forged = srgb_parametric();
        forged[128..132].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(!is_srgb(&forged));
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
        // A table that claims more points than its bytes hold.
        let mut curve = b"curv\0\0\0\0".to_vec();
        curve.extend(u32::MAX.to_be_bytes());
        curve.extend([0_u8; 8]);
        let forged = srgb_with(&curve);
        assert!(!is_srgb(&forged));
    }

    #[test]
    fn a_truncated_parametric_curve_is_refused() {
        let truncated = srgb_with(&parametric_tag([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])[..16]);
        assert!(!is_srgb(&truncated));
    }
}
