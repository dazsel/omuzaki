//! Blendshape names as the senders spell them, onto anim-api [`Channel`]s, without allocating.
//!
//! Every sender here speaks Apple's 52 `ARKit` blendshapes, but each spells them its own way:
//!
//! | Sender | Spelling |
//! |---|---|
//! | iFacialMocap | `eyeBlink_L`, `mouthSmile_R`, `jawOpen` |
//! | VTube Studio (iPhone) | `EyeBlinkLeft`, `JawOpen` |
//! | Live Link Face | none: a fixed order ([`LIVE_LINK_ORDER`]) |
//! | VMC (perfect sync) | `EyeBlinkLeft` or `eyeBlinkLeft`, plus the VRM presets `Blink`, `Blink_L`, `Blink_R` |

use omuzaki_anim_api::Channel;

/// The longest name looked up; anything longer is no `ARKit` name. `mouthLowerDownRight` is the longest, at 19.
const MAX_NAME: usize = 32;

/// The `ARKit` channel spelled `name` with any case on its first letter (`EyeBlinkLeft` or `eyeBlinkLeft`).
pub fn arkit(name: &str) -> Option<Channel> {
    let bytes = name.as_bytes();
    let first = *bytes.first()?;
    if first.is_ascii_lowercase() {
        return Channel::from_name(name).filter(|c| c.is_arkit());
    }
    if bytes.len() > MAX_NAME || !name.is_ascii() {
        return None;
    }
    let mut buf = [0u8; MAX_NAME];
    buf[..bytes.len()].copy_from_slice(bytes);
    buf[0] = first.to_ascii_lowercase();
    let lowered = std::str::from_utf8(&buf[..bytes.len()]).ok()?;
    Channel::from_name(lowered).filter(|c| c.is_arkit())
}

/// The `ARKit` channel for an iFacialMocap name: `_L` and `_R` stand for `Left` and `Right` (`eyeBlink_L`).
pub fn ifacialmocap(name: &str) -> Option<Channel> {
    let (stem, side) = match name.as_bytes() {
        [.., b'_', b'L'] => (&name[..name.len() - 2], "Left"),
        [.., b'_', b'R'] => (&name[..name.len() - 2], "Right"),
        _ => return arkit(name),
    };
    if stem.len() + side.len() > MAX_NAME || !stem.is_ascii() {
        return None;
    }
    let mut buf = [0u8; MAX_NAME];
    buf[..stem.len()].copy_from_slice(stem.as_bytes());
    buf[stem.len()..stem.len() + side.len()].copy_from_slice(side.as_bytes());
    arkit(std::str::from_utf8(&buf[..stem.len() + side.len()]).ok()?)
}

/// What a VMC blendshape name drives: `ARKit` channels by name, and the VRM blink presets (0.x `Blink`, `Blink_L`,
/// `Blink_R`; 1.0 `blink`, `blinkLeft`, `blinkRight`) as the `eyeBlink` channels. Up to two channels; `[None, None]`
/// for a name that drives nothing here (the emotion and vowel presets).
pub fn vmc(name: &str) -> [Option<Channel>; 2] {
    match name {
        "Blink" | "blink" => [Some(Channel::EYE_BLINK_LEFT), Some(Channel::EYE_BLINK_RIGHT)],
        "Blink_L" | "blinkLeft" => [Some(Channel::EYE_BLINK_LEFT), None],
        "Blink_R" | "blinkRight" => [Some(Channel::EYE_BLINK_RIGHT), None],
        _ => [arkit(name), None],
    }
}

/// Live Link Face's blendshape order: the 52 `ARKit` shapes in Apple's `ARFaceAnchor.BlendShapeLocation` declaration
/// order (eyes, jaw, mouth, brows, cheeks, nose, tongue), not the alphabetical order of the channel table. The nine
/// head and eye angles that follow them in the packet are read by position in `protocol::live_link`.
pub const LIVE_LINK_ORDER: [&str; 52] = [
    "eyeBlinkLeft",
    "eyeLookDownLeft",
    "eyeLookInLeft",
    "eyeLookOutLeft",
    "eyeLookUpLeft",
    "eyeSquintLeft",
    "eyeWideLeft",
    "eyeBlinkRight",
    "eyeLookDownRight",
    "eyeLookInRight",
    "eyeLookOutRight",
    "eyeLookUpRight",
    "eyeSquintRight",
    "eyeWideRight",
    "jawForward",
    "jawLeft",
    "jawRight",
    "jawOpen",
    "mouthClose",
    "mouthFunnel",
    "mouthPucker",
    "mouthLeft",
    "mouthRight",
    "mouthSmileLeft",
    "mouthSmileRight",
    "mouthFrownLeft",
    "mouthFrownRight",
    "mouthDimpleLeft",
    "mouthDimpleRight",
    "mouthStretchLeft",
    "mouthStretchRight",
    "mouthRollLower",
    "mouthRollUpper",
    "mouthShrugLower",
    "mouthShrugUpper",
    "mouthPressLeft",
    "mouthPressRight",
    "mouthLowerDownLeft",
    "mouthLowerDownRight",
    "mouthUpperUpLeft",
    "mouthUpperUpRight",
    "browDownLeft",
    "browDownRight",
    "browInnerUp",
    "browOuterUpLeft",
    "browOuterUpRight",
    "cheekPuff",
    "cheekSquintLeft",
    "cheekSquintRight",
    "noseSneerLeft",
    "noseSneerRight",
    "tongueOut",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_live_link_name_is_a_distinct_arkit_channel() {
        let mut seen = omuzaki_anim_api::ChannelMask::EMPTY;
        for name in LIVE_LINK_ORDER {
            let channel = arkit(name).unwrap_or_else(|| panic!("{name} is not an ARKit channel"));
            assert!(!seen.contains(channel), "{name} twice");
            seen.insert(channel);
        }
    }

    #[test]
    fn spellings_resolve() {
        assert_eq!(arkit("EyeBlinkLeft"), Some(Channel::EYE_BLINK_LEFT));
        assert_eq!(arkit("eyeBlinkLeft"), Some(Channel::EYE_BLINK_LEFT));
        assert_eq!(arkit("headYaw"), None, "not an ARKit channel");
        assert_eq!(arkit(""), None);
        assert_eq!(ifacialmocap("eyeBlink_L"), Some(Channel::EYE_BLINK_LEFT));
        assert_eq!(ifacialmocap("mouthSmile_R"), Some(Channel::MOUTH_SMILE_RIGHT));
        assert_eq!(ifacialmocap("jawOpen"), Some(Channel::JAW_OPEN));
        assert_eq!(ifacialmocap("nonsense_L"), None);
        assert_eq!(vmc("Blink"), [Some(Channel::EYE_BLINK_LEFT), Some(Channel::EYE_BLINK_RIGHT)]);
        assert_eq!(vmc("Blink_R"), [Some(Channel::EYE_BLINK_RIGHT), None]);
        assert_eq!(vmc("JawOpen"), [Some(Channel::JAW_OPEN), None]);
        assert_eq!(vmc("Joy"), [None, None]);
    }
}
