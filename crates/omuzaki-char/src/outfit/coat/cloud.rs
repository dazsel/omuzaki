//! The body a coat's skirt must clear, as points: the torso, the legs and the feet sampled ring by ring, the arms with
//! an allowance for the hands, and whatever earlier garments hang below the waist ([`HangReach`]).
//!
//! A hanging skirt is cut from horizontal slices of this cloud. Sampling the surface rather than taking the section
//! whose centre is at a height matters for the limbs that lean: an arm's section at the height of the hip spans several
//! centimetres up and down, and so does the top of a shoulder.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef};
use omuzaki_core::glam::{DVec2, DVec3};

use super::reach::HangReach;
use crate::kit::curve::{TAU, fraction};
use crate::outfit::common::GarmentError;
use crate::outfit::loft::{MIN_EXPONENT, ring_at};

/// Points round each sampled ring.
const RING_POINTS: usize = 32;
/// Rings are this far apart along a region at most, metres: closer than the skirt's slices are thick, so every slice
/// holds a ring of each region it crosses.
const RING_STEP: f64 = 0.004;
/// The fewest rings along a region.
const MIN_RINGS: usize = 8;
/// The most rings along a region.
const MAX_RINGS: usize = 400;
/// The hand beyond the wrist: its length as a share of the forearm, its half-width as a multiple of the wrist's, and
/// the rings it is sampled with. Generous: the mitten and the five-finger hands both fit inside.
const HAND: (f64, f64, usize) = (1.05, 2.2, 8);

/// One point of an arm: where it is and how far along the arm (0 the shoulder, 1 the wrist, above 1 the hand).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ArmPoint {
    /// The point.
    pub p: DVec3,
    /// Along the arm.
    pub t: f64,
}

/// The cloud.
#[derive(Clone, Debug)]
pub struct Cloud {
    body: Vec<DVec3>,
    arms: Vec<ArmPoint>,
    /// The torso's lowest ring, which stands for the pelvis below it, and its height.
    pelvis: (Vec<DVec3>, f64),
    reach: Option<HangReach>,
}

/// How many rings sample `region` of `fit`: one every [`RING_STEP`] along it.
fn rings_of(fit: &dyn BodyFit, region: BodyRegion) -> usize {
    let end = |t: f64| ring_at(fit, region, t, MIN_EXPONENT, 0.0).map(|r| r.centre);
    let length = match (end(0.0), end(1.0)) {
        (Ok(a), Ok(b)) => a.distance(b),
        _ => 0.0,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small count
    let rings = (length / RING_STEP).ceil() as usize;
    rings.clamp(MIN_RINGS, MAX_RINGS)
}

/// The ring of `region` at `t` as points, or nothing where the fit has no section.
fn ring_points(fit: &dyn BodyFit, region: BodyRegion, t: f64) -> Vec<DVec3> {
    ring_at(fit, region, t, MIN_EXPONENT, 0.0)
        .map(|ring| (0..RING_POINTS).map(|k| ring.point(TAU * fraction(k, RING_POINTS))).collect())
        .unwrap_or_default()
}

impl Cloud {
    /// The cloud of `fit`, with the `reach` of the garments under this one.
    ///
    /// # Errors
    /// [`GarmentError::NoSection`] if the fit has no torso.
    pub fn of(fit: &dyn BodyFit, reach: Option<HangReach>) -> Result<Self, GarmentError> {
        let mut body = Vec::new();
        for region in [BodyRegion::Torso, BodyRegion::LegL, BodyRegion::LegR, BodyRegion::FootL, BodyRegion::FootR] {
            let rings = rings_of(fit, region);
            for k in 0..=rings {
                body.extend(ring_points(fit, region, fraction(k, rings)));
            }
        }
        let pelvis = ring_at(fit, BodyRegion::Torso, 0.0, MIN_EXPONENT, 0.0)?;
        let pelvis = (ring_points(fit, BodyRegion::Torso, 0.0), pelvis.centre.y);
        let mut arms = Vec::new();
        for (region, elbow, hand) in [
            (BodyRegion::ArmL, HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand),
            (BodyRegion::ArmR, HumanoidBone::RightLowerArm, HumanoidBone::RightHand),
        ] {
            let rings = rings_of(fit, region);
            for k in 0..=rings {
                let t = fraction(k, rings);
                arms.extend(ring_points(fit, region, t).into_iter().map(|p| ArmPoint { p, t }));
            }
            arms.extend(hand_points(fit, region, elbow, hand));
        }
        Ok(Self { body, arms, pelvis, reach })
    }

    /// The body's points within `half` of height `y`, as `(x, z)`: the torso (its lowest ring below it), the legs, the
    /// feet, and the garments under this one.
    pub fn body_at(&self, y: f64, half: f64) -> Vec<DVec2> {
        let mut out: Vec<DVec2> =
            self.body.iter().filter(|p| (p.y - y).abs() <= half).map(|p| DVec2::new(p.x, p.z)).collect();
        if y < self.pelvis.1 {
            out.extend(self.pelvis.0.iter().map(|p| DVec2::new(p.x, p.z)));
        }
        if let Some(reach) = &self.reach {
            out.extend(reach.points_at(y));
        }
        out
    }

    /// The arms' and hands' points within `half` of height `y`.
    pub fn arms_at(&self, y: f64, half: f64) -> impl Iterator<Item = &ArmPoint> {
        self.arms.iter().filter(move |a| (a.p.y - y).abs() <= half)
    }
}

/// The hand beyond the wrist of `region`, as rings along the forearm's line: a generous solid for the clearance.
fn hand_points(fit: &dyn BodyFit, region: BodyRegion, elbow: HumanoidBone, hand: HumanoidBone) -> Vec<ArmPoint> {
    let Ok(wrist) = ring_at(fit, region, 1.0, MIN_EXPONENT, 0.0) else { return Vec::new() };
    let joint = |bone| fit.joint(&JointRef::Humanoid(bone)).map(omuzaki_core::glam::Vec3::as_dvec3);
    let (Some(elbow), Some(start)) = (joint(elbow), joint(hand).or(Some(wrist.centre))) else { return Vec::new() };
    let Some(along) = (wrist.centre - elbow).try_normalize() else { return Vec::new() };
    let (share, widen, rings) = HAND;
    let length = share * wrist.centre.distance(elbow);
    let radius = wrist.radii.max_element() * widen;
    let side = along.cross(DVec3::Z).try_normalize().unwrap_or(DVec3::X);
    let up = side.cross(along);
    let mut out = Vec::new();
    for k in 0..=rings {
        let c = start + along * (length * fraction(k, rings));
        // A rounded end: the last rings narrow.
        let r = radius * (1.0 - fraction(k, rings).powi(4)).max(0.3);
        out.extend((0..RING_POINTS).map(|j| {
            let a = TAU * fraction(j, RING_POINTS);
            ArmPoint { p: c + (side * a.cos() + up * a.sin()) * r, t: 1.0 + fraction(k, rings) }
        }));
    }
    out
}
