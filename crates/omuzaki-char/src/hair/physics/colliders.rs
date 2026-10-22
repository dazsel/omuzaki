//! The hair's colliders from the character it hangs on: spheres inside the head, the neck, the upper torso and the
//! shoulders, each following the bone that carries that part of the body, so a simulated lock rests on the back and
//! slides off a shoulder instead of passing through it.
//!
//! Every sphere is inscribed: it sits inside the surface it stands for (the head's nearest radius, the torso's depth),
//! so the styled hair, which the generators keep off the body, never starts inside one (a lock pulled into a collider
//! by its spring would shiver). The hair's own `HairPhysics::colliders` replace these when given.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, HeadSurface, JointRef, SphereCollider};
use omuzaki_core::glam::Vec3;

/// How much of the head's nearest radius the head sphere takes: inside the scalp, under the hair's roots.
const HEAD_SHARE: f32 = 0.9;
/// Where along the torso (hips 0, neck base 1) the upper back's spheres sit, and the bone they follow.
const TORSO_AT: [(f32, JointRef); 3] = [
    (0.95, JointRef::Humanoid(HumanoidBone::UpperChest)),
    (0.8, JointRef::Humanoid(HumanoidBone::UpperChest)),
    (0.62, JointRef::CHEST),
];
/// Where along each arm (shoulder 0, wrist 1) its sphere sits.
const SHOULDER_AT: f32 = 0.06;

/// The colliders for hair over `head` and, when there is one, the body `fit`.
pub fn from_character(head: &dyn HeadSurface, fit: Option<&dyn BodyFit>) -> Vec<SphereCollider> {
    let sphere = |bone: JointRef, centre: Vec3, radius: f32| {
        (centre.is_finite() && radius.is_finite() && radius > 0.0).then(|| SphereCollider {
            bone,
            centre: centre.as_dvec3().to_array(),
            radius: f64::from(radius),
        })
    };
    let centre = head.centre();
    let nearest = [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z]
        .into_iter()
        .map(|dir| head.radius(dir))
        .filter(|r| r.is_finite() && *r > 0.0)
        .fold(f32::INFINITY, f32::min);
    let mut out: Vec<SphereCollider> = sphere(JointRef::HEAD, centre, nearest * HEAD_SHARE).into_iter().collect();
    let Some(fit) = fit else { return out };
    if let Some(s) = fit.section(BodyRegion::Neck, 0.4) {
        out.extend(sphere(JointRef::NECK, s.centre, s.radii.min_element()));
    }
    // The torso is wider than deep: two spheres as deep as it is, side by side, span it at each height.
    for (t, bone) in TORSO_AT {
        let Some(s) = fit.section(BodyRegion::Torso, t) else { continue };
        let radius = s.radii.y.min(s.radii.x);
        let side = (s.radii.x - radius).max(0.0);
        for x in [-side, side] {
            out.extend(sphere(bone.clone(), s.centre + Vec3::new(x, 0.0, 0.0), radius));
        }
    }
    for (region, bone) in [
        (BodyRegion::ArmL, JointRef::Humanoid(HumanoidBone::LeftUpperArm)),
        (BodyRegion::ArmR, JointRef::Humanoid(HumanoidBone::RightUpperArm)),
    ] {
        if let Some(s) = fit.section(region, SHOULDER_AT) {
            out.extend(sphere(bone, s.centre, s.radii.min_element()));
        }
    }
    out
}
