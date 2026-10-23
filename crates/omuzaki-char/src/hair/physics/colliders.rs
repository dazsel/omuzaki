//! The hair's colliders from the character it hangs on: spheres inside the head, the neck, the upper torso and the
//! shoulders, each following the bone that carries that part of the body, so a simulated lock rests on the back and
//! slides off a shoulder instead of passing through it.
//!
//! Every sphere is inscribed: it sits inside the surface it stands for (the head's nearest radius; for the body, the
//! nearest point of the sections around it, since the torso is shallower off its middle and narrows toward the
//! shoulders), so the styled hair, which the generators keep off the body, never starts inside one (a lock pulled into a collider
//! by its spring would shiver). The hair's own `HairPhysics::colliders` replace these when given.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, HeadSurface, JointRef, SphereCollider};
use omuzaki_core::glam::Vec3;

/// How much of the head's nearest radius the head sphere takes: inside the scalp, under the hair's roots.
const HEAD_SHARE: f32 = 0.9;
/// Where along the neck (base 0, head 1) its sphere sits.
const NECK_AT: f32 = 0.4;
/// Where along the torso (hips 0, neck base 1) the upper back's spheres sit, and the bone they follow.
const TORSO_AT: [(f32, JointRef); 3] = [
    (0.95, JointRef::Humanoid(HumanoidBone::UpperChest)),
    (0.8, JointRef::Humanoid(HumanoidBone::UpperChest)),
    (0.62, JointRef::CHEST),
];
/// Where along each arm (shoulder 0, wrist 1) its sphere sits: clear of the armpit, where the arm meets the torso.
const SHOULDER_AT: f32 = 0.1;
/// The sections [`clearance`] measures against: this many either side of the sphere's, this far apart along the region.
const AROUND: (i32, f32) = (8, 0.02);
/// Points sampled around each section's ellipse.
const RIM: u16 = 48;

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
    if let Some(s) = fit.section(BodyRegion::Neck, NECK_AT) {
        let room = clearance(fit, BodyRegion::Neck, NECK_AT, s.centre);
        out.extend(sphere(JointRef::NECK, s.centre, s.radii.min_element().min(room)));
    }
    // The torso is wider than deep: two spheres as deep as it is, side by side, span it at each height. Off the
    // middle the ellipse is shallower, and the torso narrows toward the shoulders, so each sphere shrinks to the
    // nearest point of the sections around it.
    for (t, bone) in TORSO_AT {
        let Some(s) = fit.section(BodyRegion::Torso, t) else { continue };
        let radius = s.radii.y.min(s.radii.x);
        let side = (s.radii.x - radius).max(0.0);
        for x in [-side, side] {
            let centre = s.centre + Vec3::new(x, 0.0, 0.0);
            let room = clearance(fit, BodyRegion::Torso, t, centre);
            out.extend(sphere(bone.clone(), centre, radius.min(room)));
        }
    }
    for (region, bone) in [
        (BodyRegion::ArmL, JointRef::Humanoid(HumanoidBone::LeftUpperArm)),
        (BodyRegion::ArmR, JointRef::Humanoid(HumanoidBone::RightUpperArm)),
    ] {
        if let Some(s) = fit.section(region, SHOULDER_AT) {
            let room = clearance(fit, region, SHOULDER_AT, s.centre);
            out.extend(sphere(bone, s.centre, s.radii.min_element().min(room)));
        }
    }
    out
}

/// How far `centre` is from the surface of `region` near `t`: the nearest point of the ellipses of the sections
/// [`AROUND`] it, sampled [`RIM`] times each. A sphere this size about `centre` stays inside those sections.
fn clearance(fit: &dyn BodyFit, region: BodyRegion, t: f32, centre: Vec3) -> f32 {
    let mut nearest = f32::INFINITY;
    for k in -AROUND.0..=AROUND.0 {
        #[allow(clippy::cast_precision_loss)] // a handful of stations
        let at = t + k as f32 * AROUND.1;
        if !(0.0..=1.0).contains(&at) {
            continue;
        }
        let Some(s) = fit.section(region, at) else { continue };
        let axis = s.axis.normalize_or(Vec3::Y);
        // `y` runs front to back for every region the colliders use (a foot's would not); `x` is square to it.
        let through = (Vec3::Z - axis * axis.z).normalize_or(Vec3::Z);
        let across = through.cross(axis);
        for m in 0..RIM {
            let angle = std::f32::consts::TAU * f32::from(m) / f32::from(RIM);
            let rim = s.centre + across * (s.radii.x * angle.cos()) + through * (s.radii.y * angle.sin());
            nearest = nearest.min(rim.distance(centre));
        }
    }
    nearest
}
