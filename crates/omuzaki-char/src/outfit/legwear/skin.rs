//! The legwear's skin weights and vertex colours: the prototype's `W(x, y)` and `rgb(i, j, x, y, z)` of `buildLeg`.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef};

use super::section::COLUMNS;
use crate::kit::curve::{TAU, fraction, sstep};
use crate::kit::skin::{MAX_INFLUENCES, limit_influences, normalise};
use crate::outfit::common::GarmentError;

/// Half the width of the hand-over at the hip, as a fraction of the thigh (`0.045 / 0.19` on the prototype's body): the
/// thigh takes over from the hips between `hip + half` and `hip - half`.
const HIP_HALF: f64 = 0.045 / 0.19;
/// Where the knee hand-over starts and ends, below the knee joint upward, as fractions of the thigh (the prototype's
/// `0.195` and `0.268` against a knee joint at `0.19` and a thigh of `0.19`).
const KNEE_FROM: f64 = 0.005 / 0.19;
const KNEE_TO: f64 = 0.078 / 0.19;
/// The inner thigh's darkening at its darkest, and the back of the knee's extra.
const INNER_DARK: f64 = 0.10;
const KNEE_DARK: f64 = 0.05;
/// The knee crease's height above the knee joint, and its half-width (`0.235` and `0.03` against a joint at `0.19`),
/// as fractions of the thigh.
const CREASE_ABOVE: f64 = 0.045 / 0.19;
const CREASE_WIDTH: f64 = 0.03 / 0.19;
/// The share of its colour the band at a stocking's top keeps, and how wide it is, metres.
pub const BAND_SHADE: f64 = 0.8;
pub const BAND_WIDTH: f64 = 0.01;

/// Which leg.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Side {
    /// The character's left (+x).
    Left,
    /// The character's right.
    Right,
}

impl Side {
    /// Both legs, left first (the prototype builds the left and mirrors it).
    pub const BOTH: [Self; 2] = [Self::Left, Self::Right];

    /// The leg's region.
    pub fn region(self) -> BodyRegion {
        match self {
            Self::Left => BodyRegion::LegL,
            Self::Right => BodyRegion::LegR,
        }
    }

    fn upper(self) -> HumanoidBone {
        match self {
            Self::Left => HumanoidBone::LeftUpperLeg,
            Self::Right => HumanoidBone::RightUpperLeg,
        }
    }

    fn lower(self) -> HumanoidBone {
        match self {
            Self::Left => HumanoidBone::LeftLowerLeg,
            Self::Right => HumanoidBone::RightLowerLeg,
        }
    }

    /// `+1` where the leg's inner side (toward the other leg) is `-sin(phi)`, `-1` where it is `+sin(phi)`.
    fn inner_sign(self) -> f64 {
        match self {
            Self::Left => -1.0,
            Self::Right => 1.0,
        }
    }
}

/// The weights of one leg by height: the hips, the thigh and the shin handing over at the hip and the knee, placed
/// relative to that leg's joints so they land in the same places on any template.
#[derive(Clone, Debug)]
pub struct LegSkin {
    side: Side,
    upper: JointRef,
    lower: JointRef,
    /// The hip joint's height and the knee joint's, metres.
    hip_y: f64,
    knee_y: f64,
}

impl LegSkin {
    /// The weights of `side` of `fit`.
    ///
    /// # Errors
    /// [`GarmentError::Invalid`] for a fit without that leg's hip and knee joints, or a knee not below the hip.
    pub fn new(fit: &dyn BodyFit, side: Side) -> Result<Self, GarmentError> {
        let (upper, lower) = (JointRef::Humanoid(side.upper()), JointRef::Humanoid(side.lower()));
        let height = |joint: &JointRef| {
            fit.joint(joint)
                .map(|p| f64::from(p.y))
                .filter(|y| y.is_finite())
                .ok_or_else(|| GarmentError::invalid("joint", format!("the fit has no {joint:?}")))
        };
        let (hip_y, knee_y) = (height(&upper)?, height(&lower)?);
        if hip_y <= knee_y {
            return Err(GarmentError::invalid("joint", format!("the knee ({knee_y}) isn't below the hip ({hip_y})")));
        }
        Ok(Self { side, upper, lower, hip_y, knee_y })
    }

    fn thigh(&self) -> f64 {
        self.hip_y - self.knee_y
    }

    /// The weights at height `y`: at most four, normalised.
    #[allow(clippy::cast_possible_truncation)] // weights are in 0..=1; the mesh stores f32
    pub fn at(&self, y: f64) -> Vec<(JointRef, f32)> {
        let thigh = self.thigh();
        // 1 below the hip hand-over and 0 above it: how much of the vertex belongs to the leg.
        let up = sstep(self.hip_y + HIP_HALF * thigh, self.hip_y - HIP_HALF * thigh, y);
        // 1 below the knee hand-over and 0 above it: how much of the leg is the shin.
        let lo = sstep(self.knee_y + KNEE_TO * thigh, self.knee_y + KNEE_FROM * thigh, y);
        let mut blend = vec![
            (JointRef::HIPS, (1.0 - up) as f32),
            (self.upper.clone(), (up * (1.0 - lo)) as f32),
            (self.lower.clone(), (up * lo) as f32),
        ];
        limit_influences(&mut blend, MAX_INFLUENCES);
        normalise(&mut blend);
        blend
    }

    /// The grey of the vertex in column `j` at height `y`, `0..=1`: darker on the inner thigh, and behind the knee. `band`
    /// darkens it further (a stocking's top edge).
    #[allow(clippy::cast_possible_truncation)] // a grey in 0..=1; the mesh stores f32
    pub fn tone(&self, j: usize, y: f64, band: bool) -> f32 {
        let thigh = self.thigh();
        let phi = TAU * fraction(j, COLUMNS - 1);
        let inner = (self.side.inner_sign() * phi.sin()).max(0.0) * INNER_DARK;
        let crease = self.knee_y + CREASE_ABOVE * thigh;
        let knee = (-((y - crease) / (CREASE_WIDTH * thigh)).powi(2)).exp() * KNEE_DARK * (-phi.cos()).max(0.0);
        let k = 1.0 - inner - knee;
        (if band { k * BAND_SHADE } else { k }) as f32
    }
}
