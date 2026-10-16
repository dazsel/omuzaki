//! The trousers' shaping: the leg profile, the rise, the waistband and the cuff, as the functions that push the plain
//! body rings out.
//!
//! A garment keeps its ease only if it stays outside the ring it is shaped from, so every shaping here only grows a ring
//! (a ratio of at least 1, a bump outward): the taper of a leg is how little it grows, never a shrink.

use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kit::curve::{lerp, sstep};
use crate::outfit::loft::Ring;

/// The least width ratio of a leg: the body's own section, plus the ease.
pub const MIN_RATIO: f64 = 1.0;
/// The most: a skirt-wide leg.
pub const MAX_RATIO: f64 = 3.0;
/// How far from the middle of the body each leg's inner wall may reach, metres: the two legs' cloth touches there
/// rather than crossing.
pub const MID_GAP: f64 = 0.0005;
/// The leg's width ratio starts to grow over this much of the leg after the gusset, as a fraction of the leg region.
pub const RATIO_RAMP: f64 = 0.10;
/// The length over which a cuff rises to its full roll, as a fraction of the leg region (about 6 mm on soft-sd).
pub const CUFF_RAMP: f64 = 0.016;
/// The waistband rises to its full thickness over this much of the torso region.
pub const BAND_RAMP: f64 = 0.02;

/// How wide the trousers are at three places, as ratios of the body's own section there plus the ease: `1` is as close
/// as the ease allows, larger is looser. The ratios between are smooth steps. Straight, tapered, flared and baggy legs
/// are different numbers, not different kinds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct LegProfile {
    /// At the top of the leg, just under the crotch.
    pub thigh: f64,
    /// At the knee.
    pub knee: f64,
    /// At the end of the leg region (the ankle). A shorter pair is this same leg cut off higher up.
    pub hem: f64,
}

impl LegProfile {
    /// A straight leg: the cloth hangs from the thigh without following the calf in.
    pub fn straight() -> Self {
        Self { thigh: 1.05, knee: 1.25, hem: 1.6 }
    }

    /// A tapered leg: the cloth follows the leg in toward the ankle.
    pub fn tapered() -> Self {
        Self { thigh: 1.05, knee: 1.1, hem: 1.05 }
    }

    /// A flared leg: wide at the hem.
    pub fn flared() -> Self {
        Self { thigh: 1.05, knee: 1.2, hem: 2.4 }
    }

    /// A baggy leg: loose all the way down.
    pub fn baggy() -> Self {
        Self { thigh: 1.5, knee: 1.6, hem: 1.8 }
    }
}

impl Default for LegProfile {
    fn default() -> Self {
        Self::straight()
    }
}

impl Validate for LegProfile {
    fn check(&self, c: &mut Check) {
        c.range("thigh", self.thigh, MIN_RATIO, MAX_RATIO);
        c.range("knee", self.knee, MIN_RATIO, MAX_RATIO);
        c.range("hem", self.hem, MIN_RATIO, MAX_RATIO);
    }
}

/// Where the waistband's top edge sits on the torso.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rise {
    /// On the hips.
    Low,
    /// At the waist, the torso's narrowest place.
    #[default]
    Mid,
    /// Above the waist.
    High,
}

impl Rise {
    /// The band's top edge, as a fraction of the torso region (placeholders fitted to the soft-sd body: its hips, waist
    /// and ribs).
    pub fn height(self) -> f64 {
        match self {
            Self::Low => 0.18,
            Self::Mid => 0.32,
            Self::High => 0.48,
        }
    }
}

/// How one leg is cut: the profile with where the leg starts, where it ends and what turns up at the hem.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LegCut {
    /// The ratios.
    pub profile: (f64, f64, f64),
    /// Where the knee is along the leg region.
    pub knee_at: f64,
    /// Where the leg proper starts: the ratios grow from 1 here.
    pub starts: f64,
    /// Where it ends.
    pub hem: f64,
    /// How high the cuff reaches above the hem, as a fraction of the leg region (0 for none).
    pub cuff_height: f64,
    /// How far the cuff stands out, metres.
    pub cuff_roll: f64,
}

impl LegCut {
    /// The width ratio at `t` along the leg: 1 where the leg starts, then the thigh, knee and hem ratios joined by
    /// smooth steps.
    pub fn ratio(&self, t: f64) -> f64 {
        let (thigh, knee, hem) = self.profile;
        let k = if t <= self.knee_at {
            lerp(thigh, knee, sstep(0.0, self.knee_at, t))
        } else {
            lerp(knee, hem, sstep(self.knee_at, 1.0, t))
        };
        1.0 + (k - 1.0) * sstep(self.starts, self.starts + RATIO_RAMP, t)
    }

    /// How far the cuff stands out at `t`, metres.
    pub fn roll(&self, t: f64) -> f64 {
        if self.cuff_height <= 0.0 {
            return 0.0;
        }
        let top = self.hem - self.cuff_height;
        self.cuff_roll * sstep(top - CUFF_RAMP, top, t)
    }

    /// The point of `ring` at `phi`, shaped: grown by [`LegCut::ratio`] about the ring's centre, rolled out at the cuff,
    /// and kept on its own side of the middle of the body.
    pub fn point(&self, ring: &Ring, t: f64, phi: f64) -> DVec3 {
        let p = ring.point(phi);
        let out = p - ring.centre;
        let radial = out.try_normalize().unwrap_or(DVec3::ZERO);
        let mut q = ring.centre + out * self.ratio(t) + radial * self.roll(t);
        let side = ring.centre.x.signum();
        q.x = side * (side * q.x).max(MID_GAP);
        q
    }
}

/// The waistband: a thicker band round the top of the torso part, standing `thickness` out for `height` below the top
/// edge and easing out to the plain cloth below that.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Band {
    /// The top edge, as a fraction of the torso region.
    pub top: f64,
    /// How tall it is, as a fraction of the torso region.
    pub height: f64,
    /// How far it stands out, metres.
    pub thickness: f64,
}

impl Band {
    /// How far the band stands out at `t`, metres.
    pub fn thickness_at(&self, t: f64) -> f64 {
        let bottom = self.top - self.height;
        self.thickness * sstep(bottom - BAND_RAMP, bottom, t)
    }

    /// The point of `ring` at `phi`, with the band's thickness added outward.
    pub fn point(&self, ring: &Ring, t: f64, phi: f64) -> DVec3 {
        let p = ring.point(phi);
        let radial = (p - ring.centre).try_normalize().unwrap_or(DVec3::ZERO);
        p + radial * self.thickness_at(t)
    }
}
