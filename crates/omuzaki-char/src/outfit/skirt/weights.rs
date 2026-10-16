//! The skirt's skin weights: the prototype's `skirtWeights(x, y, z)`.
//!
//! The pleats in front of and behind a thigh ride with it, so a thigh never punches through and a sitting skirt drapes
//! over the lap; the pleats at the far sides and at the waist stay with the hips. A thigh carries at most its cap of a
//! vertex (the prototype's is 95 %). The prototype's hand-over to the hips is the legs' own, so a thigh and the skirt over it move together; the default
//! hands over higher up, where the body's own thigh stops.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, JointRef};
use omuzaki_core::glam::{DVec3, Vec3};

use crate::kit::curve::sstep;
use crate::kit::skin::{MAX_INFLUENCES, limit_influences, normalise};
use crate::outfit::common::GarmentError;

/// The least a thigh may be allowed to carry of a vertex, as a share.
pub const MIN_THIGH_CAP: f64 = 0.5;
/// The prototype's reach: how far from a thigh's centre line its pull fades, as a multiple of the thigh's distance from
/// the body's axis (`0.07 / 0.045` on the prototype's body).
pub const PROTOTYPE_REACH: f64 = 0.07 / 0.045;
/// The prototype's hand-over from the thighs to the hips: the thighs carry the skirt up to `-0.237` of a thigh's length
/// above the hip joint, and the hips from `+0.237` (the legs' own hip blend, `0.045 / 0.19`).
pub const PROTOTYPE_BLEND: [f64; 2] = [-0.045 / 0.19, 0.045 / 0.19];
/// The farthest above the hip joint, and below it, a hand-over may start or end, as a fraction of a thigh.
pub const MAX_BLEND: f64 = 1.0;
/// The smallest and largest reach a skirt may have.
pub const MIN_REACH: f64 = 0.3;
/// See [`MIN_REACH`].
pub const MAX_REACH: f64 = 2.0;
/// The smallest total pull the cap divides by, so a vertex far from both thighs doesn't divide by zero.
const MIN_PULL: f64 = 1e-6;

/// How strongly the thighs hold the skirt below the waist.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThighPull {
    /// The prototype's: a thigh pulls a vertex by a Gaussian of its distance from the thigh's centre line, so the pleats at
    /// the far sides stay on the hips and a vertex between the thighs is held by both only partly.
    Falloff,
    /// Everything below the waist rides on the nearer thighs (shared by the Gaussians, as the prototype's is), so the whole
    /// skirt goes with the legs and no thigh can swing through the pleats the hips would have held behind.
    #[default]
    Whole,
}

/// The skirt's weights, placed relative to the fit's hip joints so they land in the same places on any template.
#[derive(Clone, Debug)]
pub struct SkirtSkin {
    leg_x: f64,
    hip_y: f64,
    thigh: f64,
    cap: f64,
    reach: f64,
    blend: [f64; 2],
    pull: ThighPull,
}

impl SkirtSkin {
    /// The weights over `fit`, a thigh carrying at most `cap` of a vertex and
    /// pulling on vertices within `reach` times its distance from the axis, and handing the skirt over to the hips between
    /// the heights `blend` (fractions of a thigh above the hip joint: thighs below the first, hips above the second).
    ///
    /// # Errors
    /// [`GarmentError::Invalid`] for a fit without both hip joints and a left knee below them, or hips on the axis.
    pub fn new(
        fit: &dyn BodyFit,
        cap: f64,
        reach: f64,
        blend: [f64; 2],
        pull: ThighPull,
    ) -> Result<Self, GarmentError> {
        let joint = |bone| {
            fit.joint(&JointRef::Humanoid(bone))
                .map(Vec3::as_dvec3)
                .filter(|p| p.is_finite())
                .ok_or_else(|| GarmentError::invalid("joint", format!("the fit has no {bone:?} joint")))
        };
        let (hip, knee) = (joint(HumanoidBone::LeftUpperLeg)?, joint(HumanoidBone::LeftLowerLeg)?);
        let leg_x = hip.x.abs();
        let thigh = hip.y - knee.y;
        if leg_x <= 0.0 || thigh <= 0.0 {
            return Err(GarmentError::invalid("joint", "the hips are on the body's axis or the knee isn't below them"));
        }
        Ok(Self { leg_x, hip_y: hip.y, thigh, cap, reach, blend, pull })
    }

    /// The weights of a vertex at `p`: at most three, normalised.
    #[allow(clippy::cast_possible_truncation)] // weights are in 0..=1; the mesh stores f32
    pub fn at(&self, p: DVec3) -> Vec<(JointRef, f32)> {
        let ramp = sstep(self.hip_y + self.blend[1] * self.thigh, self.hip_y + self.blend[0] * self.thigh, p.y);
        let reach = self.reach * self.leg_x;
        let pull = |side: f64| (-((p.x - side * self.leg_x) / reach).powi(2)).exp();
        let (left, right) = (pull(1.0), pull(-1.0));
        let (wl, wr) = match self.pull {
            ThighPull::Falloff => {
                let total = (left + right) * ramp;
                let k = total.min(self.cap) / total.max(MIN_PULL);
                (left * ramp * k, right * ramp * k)
            }
            // The two Gaussians only say how the thighs share the vertex; below the waist it is all theirs.
            ThighPull::Whole => {
                let sum = (left + right).max(MIN_PULL);
                (left / sum * ramp * self.cap, right / sum * ramp * self.cap)
            }
        };
        let mut blend = vec![
            (JointRef::HIPS, (1.0 - wl - wr) as f32),
            (JointRef::Humanoid(HumanoidBone::LeftUpperLeg), wl as f32),
            (JointRef::Humanoid(HumanoidBone::RightUpperLeg), wr as f32),
        ];
        limit_influences(&mut blend, MAX_INFLUENCES);
        normalise(&mut blend);
        blend
    }
}
