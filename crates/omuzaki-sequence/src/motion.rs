//! Motion styles: how a character's keys play, separate from what the keys are, so one performance reads as smooth 3D,
//! hand-drawn anime on twos or threes, a snappy cartoon, limited TV animation or stop-motion by naming a preset.
//!
//! A style has five knobs, each a known animation idea:
//!
//! | Knob | What it does | Where it comes from |
//! |---|---|---|
//! | `on` | the body and face hold each drawing for this many frames | "on twos" and "on threes" of hand-drawn animation |
//! | `camera_on` | the camera holds the same way | limited animation's held camera moves |
//! | `snap` | a pose holds longer, then moves fast into the next | pose-to-pose timing with few in-betweens |
//! | `overshoot` | the move passes the next key and settles back | follow-through and overlapping action |
//! | `boil` | each drawing wobbles a little | stop-motion and hand-drawn line boil |
//!
//! A sequence's `motion` is the default; an actor's `motion` replaces it for that actor. A preset sets every knob;
//! knobs given beside it override the preset's.

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{EulerRot, Quat};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A named motion style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MotionPreset {
    /// Every frame moves and keys ease into each other: 3D animation and live performance.
    #[default]
    Smooth,
    /// On twos with a little snap: TV and film anime's character acting.
    AnimeTwos,
    /// On threes with a firmer snap: anime's limited, held acting and comedy beats.
    AnimeThrees,
    /// On threes, poses held long and snapped between, a held camera: limited TV animation.
    Limited,
    /// On ones with strong snap and overshoot: Western cartoon timing, poses that hit and settle.
    CartoonSnappy,
    /// On twos, soft and bouncy with a big overshoot: rubber-hose cartoons.
    RubberHose,
    /// On twos with a visible boil and no easing snap: stop-motion puppets.
    StopMotion,
}

impl MotionPreset {
    /// Every preset, for listings.
    pub const ALL: [Self; 7] = [
        Self::Smooth,
        Self::AnimeTwos,
        Self::AnimeThrees,
        Self::Limited,
        Self::CartoonSnappy,
        Self::RubberHose,
        Self::StopMotion,
    ];

    /// The preset's id as a sequence names it.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Smooth => "smooth",
            Self::AnimeTwos => "anime-twos",
            Self::AnimeThrees => "anime-threes",
            Self::Limited => "limited",
            Self::CartoonSnappy => "cartoon-snappy",
            Self::RubberHose => "rubber-hose",
            Self::StopMotion => "stop-motion",
        }
    }

    /// One line on what the preset looks like.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Smooth => "every frame moves, keys ease into each other: 3D animation and live performance",
            Self::AnimeTwos => "on twos with a little snap: anime character acting",
            Self::AnimeThrees => "on threes with a firmer snap: anime's held acting and comedy beats",
            Self::Limited => "on threes, long holds snapped between, a held camera: limited TV animation",
            Self::CartoonSnappy => {
                "on ones, strong snap and overshoot: poses that hit and settle, Western cartoon timing"
            }
            Self::RubberHose => "on twos, soft and bouncy with a big overshoot: rubber-hose cartoons",
            Self::StopMotion => "on twos with a visible boil and plain easing: stop-motion puppets",
        }
    }

    /// The preset's knobs.
    #[must_use]
    pub fn knobs(self) -> Knobs {
        let k = |on, camera_on, snap, overshoot, boil| Knobs { on, camera_on, snap, overshoot, boil };
        match self {
            Self::Smooth => k(1, 1, 0.0, 0.0, 0.0),
            Self::AnimeTwos => k(2, 1, 0.25, 0.1, 0.0),
            Self::AnimeThrees => k(3, 1, 0.45, 0.15, 0.0),
            Self::Limited => k(3, 3, 0.8, 0.0, 0.0),
            Self::CartoonSnappy => k(1, 1, 0.6, 0.55, 0.0),
            Self::RubberHose => k(2, 1, 0.1, 0.9, 0.1),
            Self::StopMotion => k(2, 2, 0.0, 0.05, 0.45),
        }
    }
}

/// A motion style as a sequence or an actor writes it: a preset, and any knob set beside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MotionStyle {
    /// The preset the knobs start from (`smooth` by default; `sequence.kinds` lists them).
    #[serde(default)]
    pub preset: MotionPreset,
    /// Frames each drawing of the body and face holds, 1 to 4 (2 is "on twos").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<u32>,
    /// Frames each camera position holds, 1 to 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_on: Option<u32>,
    /// How much a pose holds before it moves fast into the next, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snap: Option<f32>,
    /// How far a move passes its key before settling back, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overshoot: Option<f32>,
    /// How much each drawing wobbles, 0 to 1 (1 is about 3 degrees on the spine, head and arms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boil: Option<f32>,
}

/// A style's knobs, resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Knobs {
    /// Frames per drawing of the body and face.
    pub on: u32,
    /// Frames per camera position.
    pub camera_on: u32,
    /// Hold-then-move, 0 to 1.
    pub snap: f32,
    /// Follow-through past the key, 0 to 1.
    pub overshoot: f32,
    /// Per-drawing wobble, 0 to 1.
    pub boil: f32,
}

impl Default for Knobs {
    fn default() -> Self {
        MotionPreset::Smooth.knobs()
    }
}

/// The bones a boil wobbles, and how much each takes of it.
const BOIL_BONES: [(HumanoidBone, f32); 10] = [
    (HumanoidBone::Spine, 0.6),
    (HumanoidBone::Chest, 0.6),
    (HumanoidBone::Neck, 0.8),
    (HumanoidBone::Head, 1.0),
    (HumanoidBone::LeftUpperArm, 1.0),
    (HumanoidBone::RightUpperArm, 1.0),
    (HumanoidBone::LeftLowerArm, 1.0),
    (HumanoidBone::RightLowerArm, 1.0),
    (HumanoidBone::LeftHand, 1.2),
    (HumanoidBone::RightHand, 1.2),
];
/// Degrees of wobble per axis at a boil of 1.
const BOIL_DEGREES: f32 = 3.0;

impl MotionStyle {
    /// The knobs: the preset's, with any set here on top.
    #[must_use]
    pub fn knobs(&self) -> Knobs {
        let base = self.preset.knobs();
        Knobs {
            on: self.on.unwrap_or(base.on),
            camera_on: self.camera_on.unwrap_or(base.camera_on),
            snap: self.snap.unwrap_or(base.snap),
            overshoot: self.overshoot.unwrap_or(base.overshoot),
            boil: self.boil.unwrap_or(base.boil),
        }
    }

    /// Checks every knob's range.
    ///
    /// # Errors
    /// A message naming the knob that is out of range.
    pub fn check(&self, at: &str) -> Result<(), String> {
        let k = self.knobs();
        if !(1..=4).contains(&k.on) || !(1..=4).contains(&k.camera_on) {
            return Err(format!("{at}: `on` and `camera_on` are 1 to 4 frames"));
        }
        for (name, v) in [("snap", k.snap), ("overshoot", k.overshoot), ("boil", k.boil)] {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{at}: `{name}` is 0 to 1"));
            }
        }
        Ok(())
    }

    /// Whether this is the plain default (smooth, nothing set), so files can leave it out.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Knobs {
    /// The time a drawing that holds `on` frames at `fps` shows at `t`: the start of its hold.
    #[must_use]
    pub fn held(on: u32, fps: u32, t: f32) -> f32 {
        if on <= 1 || fps == 0 {
            return t;
        }
        #[allow(clippy::cast_precision_loss, reason = "frame rates of at most 60")]
        let step = on as f32 / fps as f32;
        (t / step + 1e-4).floor() * step
    }

    /// The progress `u` (0 to 1) between two keys reshaped by snap: held at the start, then a faster move.
    #[must_use]
    pub fn snapped(&self, u: f32) -> f32 {
        let hold = self.snap * 0.8;
        if hold <= 0.0 { u } else { ((u - hold) / (1.0 - hold)).clamp(0.0, 1.0) }
    }

    /// The eased progress `eased` (from the key's own ease of `u`) with the overshoot's bulge added: past 1 near the
    /// end, back to exactly 1 at the key.
    #[must_use]
    pub fn overshot(&self, u: f32, eased: f32) -> f32 {
        if self.overshoot <= 0.0 {
            return eased;
        }
        // Back-out's bulge over a straight line: 0 at both ends, its peak about 10% past 1 at a strength of 1.7.
        let c = self.overshoot * 1.7;
        let x = u - 1.0;
        let back = 1.0 + (c + 1.0) * x * x * x + c * x * x;
        eased + (back - u)
    }

    /// Wobbles `pose` for the drawing at `held_t` (a hold's start, so a held drawing keeps its wobble).
    pub fn boil(&self, pose: &mut HumanoidPose, held_t: f32, seed: u32) {
        if self.boil <= 0.0 {
            return;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a drawing's number")]
        let drawing = (held_t.max(0.0) * 1000.0).round() as u32;
        for (i, (bone, share)) in BOIL_BONES.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation, reason = "ten bones")]
            let n = drawing.wrapping_mul(31).wrapping_add(i as u32 * 3);
            let wobble = |axis: u32| (unit(seed ^ 0xb011, n + axis) * 2.0 - 1.0) * self.boil * BOIL_DEGREES * share;
            let turn =
                Quat::from_euler(EulerRot::XYZ, wobble(0).to_radians(), wobble(1).to_radians(), wobble(2).to_radians());
            pose.set(*bone, pose.get(*bone) * turn);
        }
    }
}

/// Carries `to` past itself by `beyond` of the move from `from` (0 is `to`), bone by bone: what an overshoot shows.
#[must_use]
pub fn past(from: &HumanoidPose, to: &HumanoidPose, beyond: f32) -> HumanoidPose {
    let mut out = *to;
    for bone in HumanoidBone::ALL {
        let (a, b) = (from.get(bone), to.get(bone));
        let step = b * a.inverse();
        out.set(bone, (Quat::IDENTITY.slerp(step, beyond) * b).normalize());
    }
    out
}

fn unit(seed: u32, n: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9e37_79b9) ^ n.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    #[allow(clippy::cast_precision_loss, reason = "24 bits are exact")]
    let u = (h >> 8) as f32 / 16_777_216.0;
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse_by_id_and_check() {
        for preset in MotionPreset::ALL {
            let style: MotionStyle = toml::from_str(&format!("preset = \"{}\"", preset.id())).unwrap();
            assert_eq!(style.preset, preset);
            style.check("motion").unwrap();
        }
    }

    #[test]
    fn a_knob_beside_a_preset_overrides_it() {
        let style: MotionStyle = toml::from_str("preset = \"anime-twos\"\non = 3\nsnap = 0.9").unwrap();
        let k = style.knobs();
        assert_eq!((k.on, k.snap), (3, 0.9));
        assert_eq!(k.camera_on, MotionPreset::AnimeTwos.knobs().camera_on);
        assert!(MotionStyle { on: Some(5), ..MotionStyle::default() }.check("m").is_err());
    }

    #[test]
    fn on_twos_holds_each_drawing_for_two_frames() {
        let held: Vec<f32> = (0..6).map(|f| Knobs::held(2, 24, f as f32 / 24.0)).collect();
        assert_eq!(held[0], held[1]);
        assert_eq!(held[2], held[3]);
        assert!(held[2] > held[1]);
        assert_eq!(Knobs::held(1, 24, 0.3), 0.3);
    }

    #[test]
    fn snap_holds_then_moves_and_overshoot_passes_and_returns() {
        let k = Knobs { snap: 0.5, overshoot: 0.6, ..Knobs::default() };
        assert_eq!(k.snapped(0.2), 0.0);
        assert!(k.snapped(0.7) > 0.4);
        assert_eq!(k.snapped(1.0), 1.0);
        let peak = (0..=100).map(|i| k.overshot(i as f32 / 100.0, i as f32 / 100.0)).fold(0.0f32, f32::max);
        assert!(peak > 1.02, "{peak}");
        assert!((k.overshot(1.0, 1.0) - 1.0).abs() < 1e-6);
        assert!(k.overshot(0.0, 0.0).abs() < 1e-6);
    }

    #[test]
    fn boil_wobbles_each_drawing_the_same_way_every_time() {
        let k = Knobs { boil: 1.0, ..Knobs::default() };
        let (mut a, mut b, mut c) = (HumanoidPose::REST, HumanoidPose::REST, HumanoidPose::REST);
        k.boil(&mut a, 0.25, 7);
        k.boil(&mut b, 0.25, 7);
        k.boil(&mut c, 1.0 / 3.0, 7);
        assert_eq!(a.get(HumanoidBone::Head), b.get(HumanoidBone::Head));
        assert_ne!(a.get(HumanoidBone::Head), c.get(HumanoidBone::Head));
        let angle = a.get(HumanoidBone::Head).angle_between(Quat::IDENTITY).to_degrees();
        assert!(angle > 0.01 && angle < 6.0, "{angle}");
    }

    #[test]
    fn past_carries_a_turn_beyond_its_key() {
        let mut from = HumanoidPose::REST;
        let mut to = HumanoidPose::REST;
        from.set(HumanoidBone::Head, Quat::IDENTITY);
        to.set(HumanoidBone::Head, Quat::from_rotation_x(0.2));
        let beyond = past(&from, &to, 0.5).get(HumanoidBone::Head);
        assert!((beyond.angle_between(Quat::IDENTITY) - 0.3).abs() < 1e-4);
    }
}
