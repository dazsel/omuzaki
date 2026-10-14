//! Default calibration for sources nobody has calibrated yet.
//!
//! Webcam landmark models read subtle on a human face (a wide-open eye rarely tops 0.2), so the prototype multiplied
//! each channel by a hand-tuned gain to bring it to a full `0..1`. The contract folds gains into ranges (a gain `g` is
//! a span of `1 / g`), and this table is the tuning data that stays with the retarget implementation.

use omuzaki_anim_api::{Calibration, Channel, DeviceProfile};

/// Gains by channel family (the prototype's `GAIN`, `arkit.js`). A family name matches a channel's name with its
/// `Left`/`Right` suffix removed; a full channel name matches itself first (`jawLeft` is a jaw channel, not a side).
/// `1` means the source is already `ARKit`-scaled.
const GAINS: &[(&str, f32)] = &[
    ("eyeBlink", 1.45),
    ("eyeWide", 4.0),
    ("eyeSquint", 1.6),
    ("cheekSquint", 2.5),
    ("eyeLook", 1.0),
    ("browInnerUp", 2.0),
    ("browOuterUp", 2.2),
    ("browDown", 2.2),
    ("noseSneer", 2.0),
    ("cheekPuff", 2.0),
    ("jawOpen", 1.7),
    ("jawLeft", 2.0),
    ("jawRight", 2.0),
    ("jawForward", 1.5),
    ("mouthClose", 1.5),
    ("mouthSmile", 1.35),
    ("mouthFrown", 2.2),
    ("mouthDimple", 2.0),
    ("mouthStretch", 2.2),
    ("mouthFunnel", 1.6),
    ("mouthPucker", 1.3),
    ("mouthLeft", 2.0),
    ("mouthRight", 2.0),
    ("mouthPress", 2.0),
    ("mouthRollLower", 2.0),
    ("mouthRollUpper", 2.0),
    ("mouthShrugLower", 1.5),
    ("mouthShrugUpper", 1.5),
    ("mouthUpperUp", 2.0),
    ("mouthLowerDown", 2.0),
    ("tongueOut", 1.0),
];

/// How much a webcam-class source under-reports `channel` (`1` for a channel not in the table).
pub fn gain(channel: Channel) -> f32 {
    let name = channel.name();
    let family = name.strip_suffix("Left").or_else(|| name.strip_suffix("Right")).unwrap_or(name);
    // The full name wins (`mouthLeft`), then the family (`mouthSmileLeft` is a `mouthSmile`).
    [name, family].into_iter().find_map(|key| GAINS.iter().find(|(n, _)| *n == key)).map_or(1.0, |&(_, g)| g)
}

/// The calibration to use before the performer has done the guided routine: ranges from the gain table for a
/// [`Webcam`](DeviceProfile::Webcam) source, the identity for an already-scaled one (`ARKit`, or any profile added
/// later, which is assumed scaled until its own table exists).
pub fn default_calibration(profile: DeviceProfile) -> Calibration {
    match profile {
        DeviceProfile::Webcam => Calibration::from_gains(profile, gain),
        _ => Calibration::identity(profile),
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // gains are table constants and ranges derive from them by one division
mod tests {
    use omuzaki_anim_api::ChannelRange;

    use super::*;

    fn gain_of(name: &str) -> f32 {
        gain(Channel::from_name(name).expect("a real channel name"))
    }

    #[test]
    fn sided_channels_use_their_family_and_exact_names_win() {
        assert_eq!(gain_of("eyeWideLeft"), 4.0);
        assert_eq!(gain_of("eyeWideRight"), 4.0);
        assert_eq!(gain_of("mouthSmileRight"), 1.35);
        assert_eq!(gain_of("browOuterUpLeft"), 2.2);
        assert_eq!(gain_of("mouthLeft"), 2.0, "an exact entry, not the family `mouth`");
        assert_eq!(gain_of("jawRight"), 2.0);
        assert_eq!(gain_of("mouthRollLower"), 2.0);
        assert_eq!(gain_of("tongueOut"), 1.0);
    }

    #[test]
    fn a_channel_the_table_leaves_out_reads_at_face_value() {
        assert_eq!(gain_of("eyeLookInLeft"), 1.0, "falls through to 1, the same as the `eyeLook` entry");
        assert_eq!(gain_of("headYaw"), 1.0);
    }

    #[test]
    fn the_webcam_default_turns_gains_into_spans_and_arkit_is_untouched() {
        let webcam = default_calibration(DeviceProfile::Webcam);
        let wide = webcam.range(Channel::EYE_WIDE_LEFT);
        assert_eq!((wide.lo, wide.hi), (0.0, 0.25), "gain 4 = a quarter-span");
        assert_eq!(webcam.profile, DeviceProfile::Webcam);
        let arkit = default_calibration(DeviceProfile::Arkit);
        assert_eq!(arkit, Calibration::identity(DeviceProfile::Arkit));
        assert_eq!(arkit.range(Channel::EYE_WIDE_LEFT), ChannelRange::IDENTITY);
    }

    #[test]
    fn no_default_span_is_narrower_than_the_floor() {
        // Below `MIN_SPAN` the range would be widened silently and the gain would not be the one in the table.
        for info in Channel::arkit() {
            let r = default_calibration(DeviceProfile::Webcam).range(info.channel);
            assert!(r.hi - r.lo >= omuzaki_anim_api::MIN_SPAN, "{}", info.name);
        }
    }
}
