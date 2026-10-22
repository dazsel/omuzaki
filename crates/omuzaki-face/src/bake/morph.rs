//! Morph targets for the channels that move the face's shape (#172): the jaw, the lips' pinch, shift, stretch, press,
//! roll and shrug, the cheeks and the nose. The painted features are textures, so these are small analytic displacement fields of the head surface, not
//! sculpted shapes: rotate the lower face about the jaw, pull the lips toward the mouth's centre, push the cheeks out.
//! A painted feature's patch lies on the head and carries the same targets, so a field that moves the lips also bends
//! the painted mouth with them: a stretch widens the drawn corner, a lower-lip drop pulls the drawn lip down.
//!
//! Every field is zero behind the face's front (the projection's `front_fade`) and outside its own region, so a target
//! moves only the face.
//!
//! The sizes are fractions of the face window's width `w`, so a field fits any head kind.

use omuzaki_core::glam::{Vec2, Vec3};

/// The channels realised as a morph target, in the order the targets are added.
pub const MORPH_CHANNELS: [&str; 28] = [
    "jawOpen",
    "jawLeft",
    "jawRight",
    "jawForward",
    "mouthClose",
    "mouthFunnel",
    "mouthPucker",
    "mouthLeft",
    "mouthRight",
    "cheekPuff",
    "mouthStretchLeft",
    "mouthStretchRight",
    "mouthDimpleLeft",
    "mouthDimpleRight",
    "mouthPressLeft",
    "mouthPressRight",
    "mouthLowerDownLeft",
    "mouthLowerDownRight",
    "mouthUpperUpLeft",
    "mouthUpperUpRight",
    "mouthRollLower",
    "mouthRollUpper",
    "mouthShrugLower",
    "mouthShrugUpper",
    "cheekSquintLeft",
    "cheekSquintRight",
    "noseSneerLeft",
    "noseSneerRight",
];

/// `channel` without its `Left` / `Right` ending, and the side as the sign of head-local `x` (the character's left is
/// `+x`, facing `+z`); a channel with no side is itself with side 0.
fn sided(channel: &str) -> (&str, f32) {
    if let Some(base) = channel.strip_suffix("Left") {
        (base, 1.0)
    } else if let Some(base) = channel.strip_suffix("Right") {
        (base, -1.0)
    } else {
        (channel, 0.0)
    }
}

/// Where the fields sit, in the head-local frame.
#[derive(Copy, Clone, Debug)]
pub(super) struct Anchors {
    /// The face window's width in metres.
    pub width: f32,
    /// The mouth's centre on the face plane.
    pub mouth: Vec2,
    /// The head-local `z` over which the face fades in toward the front: nothing moves behind `front[0]`.
    pub front: [f32; 2],
}

/// The jaw's opening angle at full weight, in radians (about 10 degrees).
const JAW_ANGLE: f32 = 0.18;

/// 0 at `from`, 1 at `to` (either order), smooth between.
fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = if (to - from).abs() <= f32::EPSILON { f32::from(u8::from(x >= to)) } else { (x - from) / (to - from) };
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A Gaussian bump of width `sigma` around `centre` on the face plane, cut to exactly 0 beyond three widths.
fn bump(p: Vec3, centre: Vec2, sigma: f32) -> f32 {
    let d2 = (Vec2::new(p.x, p.y) - centre).length_squared();
    let s2 = sigma * sigma;
    if d2 >= 9.0 * s2 { 0.0 } else { (-(d2 / (2.0 * s2))).exp() * (1.0 - d2 / (9.0 * s2)) }
}

impl Anchors {
    fn front(&self, p: Vec3) -> f32 {
        smoothstep(self.front[0], self.front[1], p.z)
    }

    /// How much of the jaw `p` is: below the mouth, in front, inside the face's width.
    fn jaw(&self, p: Vec3) -> f32 {
        let w = self.width;
        let below = smoothstep(self.mouth.y + 0.02 * w, self.mouth.y - 0.06 * w, p.y);
        let inside = smoothstep(0.45 * w, 0.3 * w, p.x.abs());
        below * inside * self.front(p)
    }

    /// How much of the lips `p` is.
    fn lips(&self, p: Vec3, sigma: f32) -> f32 {
        self.near(p, Vec2::ZERO, sigma)
    }

    /// A bump of width `sigma` (in face widths) at `offset` (in face widths) from the mouth's centre, in front.
    fn near(&self, p: Vec3, offset: Vec2, sigma: f32) -> f32 {
        bump(p, self.mouth + offset * self.width, sigma * self.width) * self.front(p)
    }

    /// The sided shapes around the mouth, the cheeks and the nose, at full weight; `side` is `1` for the character's
    /// left, `-1` for its right, `0` for a shape of both lips. Zero for a shape this does not know.
    fn shape(&self, base: &str, side: f32, p: Vec3) -> Vec3 {
        let w = self.width;
        // The mouth's corner on this side, and the lips' halves.
        let corner = || self.near(p, Vec2::new(side * 0.08, 0.0), 0.05);
        let lower = smoothstep(self.mouth.y + 0.01 * w, self.mouth.y - 0.01 * w, p.y);
        let upper = 1.0 - lower;
        match base {
            "mouthStretch" => Vec3::new(side * 0.03 * w, -0.008 * w, 0.0) * corner(),
            "mouthDimple" => Vec3::new(side * 0.015 * w, 0.0, -0.015 * w) * corner(),
            "mouthPress" => Vec3::Y * -(p.y - self.mouth.y) * 0.4 * self.near(p, Vec2::new(side * 0.04, 0.0), 0.06),
            "mouthLowerDown" => Vec3::NEG_Y * 0.02 * w * self.near(p, Vec2::new(side * 0.035, -0.02), 0.05) * lower,
            "mouthUpperUp" => Vec3::Y * 0.02 * w * self.near(p, Vec2::new(side * 0.035, 0.02), 0.05) * upper,
            "mouthRollLower" => {
                Vec3::new(0.0, 0.005 * w, -0.015 * w) * self.near(p, Vec2::new(0.0, -0.02), 0.06) * lower
            }
            "mouthRollUpper" => {
                Vec3::new(0.0, -0.005 * w, -0.015 * w) * self.near(p, Vec2::new(0.0, 0.02), 0.06) * upper
            }
            "mouthShrugLower" => Vec3::new(0.0, 0.015 * w, 0.005 * w) * self.near(p, Vec2::new(0.0, -0.04), 0.07),
            "mouthShrugUpper" => Vec3::new(0.0, 0.012 * w, 0.003 * w) * self.near(p, Vec2::new(0.0, 0.025), 0.06),
            "cheekSquint" => {
                let at = Vec2::new(side * 0.25 - self.mouth.x / w, 0.14);
                Vec3::new(0.0, 0.02 * w, 0.005 * w) * self.near(p, at, 0.08)
            }
            "noseSneer" => {
                let at = Vec2::new(side * 0.06 - self.mouth.x / w, 0.17);
                Vec3::new(0.0, 0.012 * w, 0.004 * w) * self.near(p, at, 0.04)
            }
            _ => Vec3::ZERO,
        }
    }

    /// The displacement of the head-local point `p` (with unit `normal`, or zero) by the channel `channel` at full
    /// weight; zero for a channel that is not in [`MORPH_CHANNELS`].
    pub fn displacement(&self, channel: &str, p: Vec3, normal: Vec3) -> Vec3 {
        let w = self.width;
        let (mouth, lips) = (self.mouth, |sigma| self.lips(p, sigma));
        match channel {
            "jawOpen" => {
                // Rotate about a hinge behind and above the mouth: the chin swings down and back.
                let pivot = Vec3::new(0.0, mouth.y + 0.12 * w, -0.1 * w);
                let (sin, cos) = JAW_ANGLE.sin_cos();
                let arm = p - pivot;
                let turned = Vec3::new(arm.x, arm.y * cos - arm.z * sin, arm.y * sin + arm.z * cos);
                (turned - arm) * self.jaw(p)
            }
            "jawLeft" => Vec3::X * 0.04 * w * self.jaw(p),
            "jawRight" => Vec3::NEG_X * 0.04 * w * self.jaw(p),
            "jawForward" => Vec3::Z * 0.03 * w * self.jaw(p),
            "mouthClose" => {
                let lower = smoothstep(mouth.y + 0.01 * w, mouth.y - 0.02 * w, p.y);
                Vec3::Y * 0.015 * w * lips(0.08) * lower
            }
            "mouthFunnel" => Vec3::new(-(p.x - mouth.x) * 0.15, -(p.y - mouth.y) * 0.1, 0.03 * w) * lips(0.09),
            "mouthPucker" => Vec3::new(-(p.x - mouth.x) * 0.3, -(p.y - mouth.y) * 0.15, 0.04 * w) * lips(0.08),
            "mouthLeft" => Vec3::X * 0.04 * w * lips(0.08),
            "mouthRight" => Vec3::NEG_X * 0.04 * w * lips(0.08),
            "cheekPuff" => {
                let out = if normal == Vec3::ZERO { Vec3::new(p.x, 0.0, p.z).normalize_or_zero() } else { normal };
                let cheeks = [-1.0, 1.0]
                    .map(|side| bump(p, Vec2::new(side * 0.27 * w, mouth.y + 0.06 * w), 0.09 * w))
                    .iter()
                    .sum::<f32>();
                out * 0.035 * w * cheeks * self.front(p)
            }
            _ => {
                let (base, side) = sided(channel);
                self.shape(base, side, p)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchors() -> Anchors {
        Anchors { width: 0.4, mouth: Vec2::new(0.0, -0.2), front: [0.0, 0.07] }
    }

    #[test]
    fn nothing_moves_behind_the_front_or_far_from_its_region() {
        let a = anchors();
        for channel in MORPH_CHANNELS {
            assert_eq!(a.displacement(channel, Vec3::new(0.0, -0.22, -0.05), Vec3::Z), Vec3::ZERO, "{channel}");
            assert_eq!(a.displacement(channel, Vec3::new(0.0, 0.3, 0.2), Vec3::Z), Vec3::ZERO, "{channel}");
        }
    }

    #[test]
    fn the_jaw_opens_down_and_each_field_moves_its_region() {
        let a = anchors();
        let chin = Vec3::new(0.0, -0.26, 0.2);
        assert!(a.displacement("jawOpen", chin, Vec3::Z).y < -0.005);
        assert!(a.displacement("jawLeft", chin, Vec3::Z).x > 0.0);
        assert!(a.displacement("jawRight", chin, Vec3::Z).x < 0.0);
        let lip = Vec3::new(0.02, -0.2, 0.2);
        assert!(a.displacement("mouthPucker", lip, Vec3::Z).z > 0.0);
        assert!(a.displacement("mouthPucker", lip, Vec3::Z).x < 0.0, "pulled toward the centre");
        let cheek = Vec3::new(0.108, -0.176, 0.15);
        assert!(a.displacement("cheekPuff", cheek, Vec3::X).x > 0.005);
    }

    #[test]
    fn every_channel_moves_something_and_the_sided_ones_move_their_own_side() {
        let a = anchors();
        let grid = (0..41_u8).flat_map(|i| {
            (0..41_u8).map(move |j| Vec3::new(-0.2 + 0.01 * f32::from(i), -0.36 + 0.01 * f32::from(j), 0.2))
        });
        let points: Vec<Vec3> = grid.collect();
        for channel in MORPH_CHANNELS {
            let moved: Vec<Vec3> =
                points.iter().copied().filter(|p| a.displacement(channel, *p, Vec3::Z) != Vec3::ZERO).collect();
            assert!(!moved.is_empty(), "{channel} moves nothing");
            let (base, side) = sided(channel);
            if side != 0.0 && !base.starts_with("jaw") && base != "mouth" {
                let own = |p: &Vec3| a.displacement(channel, *p, Vec3::Z).length();
                let (mine, theirs): (f32, f32) = points.iter().fold((0.0, 0.0), |(m, t), p| {
                    if (p.x - a.mouth.x) * side > 0.0 { (m + own(p), t) } else { (m, t + own(p)) }
                });
                assert!(mine > 2.0 * theirs, "{channel} moves its own side most: {mine} vs {theirs}");
            }
        }
    }
}
