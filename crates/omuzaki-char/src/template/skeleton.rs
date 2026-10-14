//! Skeleton assembly from a template (RG-01, RG-09): a template's joints as the rig's input, and the VRM humanoid they
//! make.
//!
//! The rig is a pure function of joint positions (decision G2, `omuzaki-char-api`'s `rig` module): a bone exists where
//! some published fit knows its joint, its parent is the nearest present ancestor of [`HumanoidBone::parent`], its rest
//! rotation is the identity (the VRM normalised convention, so every pose means the same on every character) and its
//! local translation is its joint minus its parent's, turned back by the parent's bound rotation. This module feeds that rule from a [`BodyLayout`] and applies it
//! when a template is tested.
//!
//! **What a template publishes.** The body's 51 joints (the 15 bones VRM requires bar the head, plus chest, upper
//! chest, neck, shoulders, toes and all 30 finger bones) and the `head` joint at [`BodyLayout::head_base`], which makes
//! 52: enough for a valid skeleton on its own. A [`BodyLayout`] holds no head, so the body part publishes a
//! [`TemplateFit`] instead: the layout's sections and these 52 joints, which is also how the head part learns where the
//! head belongs. The head part's own [`JointFit`](omuzaki_char_api::JointFit) wins over it when it moves the head and
//! adds the eyes and jaw, so the 55-bone rig is the body's joints with the head part's on top.
//!
//! **Rest in the T, bound in the template's pose (decision 0014).** The template sculpts its body in an A-pose (the
//! prototype's: arms hanging at about 31 and 27 degrees from the body) and its joints sit there. The skeleton still
//! rests in the VRM T-pose, so a [`HumanoidPose`] means the same thing on every character: identity rotations, arms and
//! fingers along the X axis, palms facing -Y. [`bind_pose`] is the pose that takes that T rest onto the template's
//! joints (each shoulder, arm, hand and finger bone turned by the shortest arc from X to where it points) and
//! [`assemble`] turns every offset back through it, so skinning at [`HumanoidPose::REST`] shows the T and posing at
//! the bind pose reproduces the layout. The thumb keeps the layout's geometry: its bones are bound with the hand's
//! rotation, so it rests at the natural angle to the palm (`HandRest::of` measures that from the rig).
//!
//! **Hand style never changes the skeleton.** A mitten, a simple hand and a five-finger hand are meshes over the same
//! 30 finger bones (RG-09: a mitten is a style, not a limit of the template), so [`from_template`] reads the numbers
//! that place joints and never [`HandStyle`](omuzaki_char_api::HandStyle).

use std::iter::successors;

use omuzaki_anim_api::{HUMANOID_COUNT, HumanoidBone, HumanoidPose, Skeleton, SkeletonError};
use omuzaki_char_api::{BodyFit, BodyParams, BodyRegion, JointRef, Section, rest_skeleton};
use omuzaki_core::glam::{DQuat, DVec3, Vec3};

use super::Template;
use super::layout::BodyLayout;
use crate::kit::KitError;

/// Why a template could not be assembled into a skeleton.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum AssembleError {
    /// The template's tables do not describe a valid layout.
    #[error("the template cannot build a layout: {0}")]
    Layout(#[from] KitError),
    /// The joints do not make a VRM humanoid (a required bone is missing, or a rest transform is not finite).
    #[error("the joints do not make a humanoid skeleton: {0}")]
    Skeleton(#[from] SkeletonError),
}

/// Where `bone`'s joint is: the layout's own, and for `head` the end of the neck bone (the layout holds no head).
fn joint_of(layout: &BodyLayout, bone: HumanoidBone) -> Option<Vec3> {
    let at = if bone == HumanoidBone::Head { layout.head_base() } else { layout.joint64(bone) };
    at.map(DVec3::as_vec3)
}

/// Every joint a template publishes, in bind space (metres), parents before children: the layout's 51 body joints and
/// `head` at the end of the neck bone. An empty layout (a template that could not build) gives none.
///
/// Positions narrow to `f32` here, once, exactly as the reference builder reads them through
/// [`BodyFit::joint`], so a skeleton from [`assemble`] equals the builder's.
pub fn joints(layout: &BodyLayout) -> Vec<(HumanoidBone, Vec3)> {
    HumanoidBone::ALL.into_iter().filter_map(|bone| joint_of(layout, bone).map(|at| (bone, at))).collect()
}

/// The bone each arm bone points at, on the left: the child whose joint gives the bone its direction. The thumb and
/// the fingertips are absent on purpose: the thumb keeps the layout's geometry and a tip has no child, so both take
/// their parent's rotation. The right side is these, mirrored.
const LEFT_LEADS: [(HumanoidBone, HumanoidBone); 12] = {
    use HumanoidBone as B;
    [
        (B::LeftShoulder, B::LeftUpperArm),
        (B::LeftUpperArm, B::LeftLowerArm),
        (B::LeftLowerArm, B::LeftHand),
        (B::LeftHand, B::LeftMiddleProximal),
        (B::LeftIndexProximal, B::LeftIndexIntermediate),
        (B::LeftIndexIntermediate, B::LeftIndexDistal),
        (B::LeftMiddleProximal, B::LeftMiddleIntermediate),
        (B::LeftMiddleIntermediate, B::LeftMiddleDistal),
        (B::LeftRingProximal, B::LeftRingIntermediate),
        (B::LeftRingIntermediate, B::LeftRingDistal),
        (B::LeftLittleProximal, B::LeftLittleIntermediate),
        (B::LeftLittleIntermediate, B::LeftLittleDistal),
    ]
};

/// Where `bone` points in the T-pose and the child that gives it its direction in the template: `+X` on the left,
/// `-X` on the right. `None` for a bone that takes its parent's rotation.
fn lead_of(bone: HumanoidBone) -> Option<(Vec3, HumanoidBone)> {
    LEFT_LEADS.iter().find_map(|&(b, lead)| {
        if b == bone {
            Some((Vec3::X, lead))
        } else if b.mirrored() == bone {
            Some((Vec3::NEG_X, lead.mirrored()))
        } else {
            None
        }
    })
}

/// The pose that takes the T-pose rest onto `joints`: the template's sculpted pose, as the rig's bind.
///
/// Walking down from the hips, each shoulder, arm, hand and finger bone is turned by the shortest arc from its T-pose
/// direction (`+X` on the left, `-X` on the right) to the direction of its joint towards its lead's ([`LEFT_LEADS`]);
/// a shortest arc adds no twist of its own, so a palm that faces -Y in the T faces the way the layout's palm does. A
/// bone with no lead (the thumb, a fingertip, the rest of the body) keeps its parent's rotation, which leaves the legs
/// and trunk where the layout has them. Rotations are local, in the normalised space of [`HumanoidPose`].
pub fn bind_pose(joints: &[(HumanoidBone, Vec3)]) -> HumanoidPose {
    let mut at: [Option<Vec3>; HUMANOID_COUNT] = [None; HUMANOID_COUNT];
    for &(bone, joint) in joints {
        at[bone.index()] = Some(joint);
    }
    // In f64, so the local rotations (rounded once, to f32) lose nothing to the chain of products down a finger.
    let mut world = [DQuat::IDENTITY; HUMANOID_COUNT];
    let mut pose = HumanoidPose::REST;
    for bone in HumanoidBone::ALL.into_iter().filter(|b| at[b.index()].is_some()) {
        let parent = successors(bone.parent(), |p| p.parent()).find(|p| at[p.index()].is_some());
        let parent_world = parent.map_or(DQuat::IDENTITY, |p| world[p.index()]);
        let aimed = lead_of(bone).and_then(|(tpose, lead)| {
            let direction = (at[lead.index()]? - at[bone.index()]?).as_dvec3().try_normalize()?;
            Some(DQuat::from_rotation_arc(tpose.as_dvec3(), direction))
        });
        world[bone.index()] = aimed.unwrap_or(parent_world);
        if aimed.is_some() {
            pose.set(bone, (parent_world.inverse() * world[bone.index()]).normalize().as_quat());
        }
    }
    pose
}

/// Where each humanoid bone's joint is when `skeleton` (rest rotations all the identity, as [`assemble`] makes it) is
/// posed in `pose`: forward kinematics of the normalised rotations, which for identity rests are the local ones. The
/// hips offset is not applied. Posing at [`bind_pose`] gives back the layout's [`joints`].
pub fn joints_at(skeleton: &Skeleton, pose: &HumanoidPose) -> Vec<(HumanoidBone, Vec3)> {
    // Accumulated in f64, so what is measured is the stored rest offsets and bind rotations, not the arithmetic.
    let mut world: Vec<(DVec3, DQuat)> = Vec::with_capacity(skeleton.len());
    let mut out = Vec::new();
    for def in skeleton.bones() {
        let q = def.humanoid.map_or(DQuat::IDENTITY, |b| pose.get(b).as_dquat());
        let rest = def.rest.translation.as_dvec3();
        let placed = match def.parent.and_then(|p| world.get(p.index())) {
            Some(&(at, rotation)) => (at + rotation * rest, (rotation * q).normalize()),
            None => (rest, q.normalize()),
        };
        world.push(placed);
        out.extend(def.humanoid.map(|b| (b, placed.0.as_vec3())));
    }
    out
}

/// A layout published as the body part's fit: the layout's sections for garments, and [`joints`] for the rig, `head`
/// included, and the [`bind_pose`] the rig is bound in.
#[derive(Clone, Debug)]
pub struct TemplateFit {
    layout: BodyLayout,
    bind: HumanoidPose,
}

impl TemplateFit {
    /// Publishes `layout`.
    pub fn new(layout: BodyLayout) -> Self {
        let bind = bind_pose(&joints(&layout));
        Self { layout, bind }
    }

    /// The layout behind the fit, at full precision.
    pub fn layout(&self) -> &BodyLayout {
        &self.layout
    }
}

impl BodyFit for TemplateFit {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        self.layout.section(region, t)
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        match joint {
            JointRef::Humanoid(bone) => joint_of(&self.layout, *bone),
            JointRef::Named(_) => None,
        }
    }

    fn bind(&self) -> HumanoidPose {
        self.bind
    }
}

/// The humanoid skeleton of these joints (a later entry for the same bone wins) as it rests in the T-pose when bound
/// in `bind` ([`bind_pose`] for a template): identity rest rotations, each bone's translation its joint minus its
/// parent's turned back through the parent's bound rotation, parents per [`HumanoidBone::parent`] (the nearest
/// present ancestor when an optional bone is absent). The rule the reference builder applies to published joints
/// (`omuzaki_char_api::rest_skeleton`, which this calls).
///
/// # Errors
/// [`SkeletonError`] if the joints lack a bone VRM requires or are not finite.
pub fn assemble(joints: &[(HumanoidBone, Vec3)], bind: &HumanoidPose) -> Result<Skeleton, SkeletonError> {
    rest_skeleton(joints, bind)
}

/// The skeleton a built layout makes on its own: [`assemble`] over [`joints`] and their [`bind_pose`] (52 bones, the
/// head at the neck's end), resting in the T.
///
/// # Errors
/// [`SkeletonError`] for an empty layout, which has no required bone.
pub fn of_layout(layout: &BodyLayout) -> Result<Skeleton, SkeletonError> {
    let all = joints(layout);
    assemble(&all, &bind_pose(&all))
}

/// The skeleton of `template` for a body's numbers, resting in the T: its proportions, body shape and hand numbers
/// place the joints, the hand *style* does not (see the module docs).
///
/// # Errors
/// [`AssembleError`] if the template cannot build a layout or the layout is not a humanoid.
pub fn from_template(template: &dyn Template, body: &BodyParams) -> Result<Skeleton, AssembleError> {
    let layout = template.try_layout(&body.proportions, &body.shape, &body.hand)?;
    Ok(of_layout(&layout)?)
}

#[cfg(test)]
mod tests {
    use omuzaki_char_api::HandStyle;
    use omuzaki_core::glam::Quat;

    use super::*;
    use crate::template::soft_sd::SoftSd;

    fn default_layout() -> BodyLayout {
        let body = BodyParams::default();
        SoftSd.layout(&body.proportions, &body.shape, &body.hand)
    }

    #[test]
    fn a_template_publishes_52_joints_in_hierarchy_order_with_the_head_at_the_neck_end() {
        let layout = default_layout();
        let all = joints(&layout);
        assert_eq!(all.len(), 52);
        let absent: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| all.iter().all(|(a, _)| a != b)).collect();
        assert_eq!(absent, [HumanoidBone::LeftEye, HumanoidBone::RightEye, HumanoidBone::Jaw]);
        assert!(all.windows(2).all(|w| w[0].0.index() < w[1].0.index()), "parents come before children");
        let head = all.iter().find(|(b, _)| *b == HumanoidBone::Head).map(|(_, at)| *at);
        assert_eq!(head, layout.head_base().map(DVec3::as_vec3));
        assert_eq!(joints(&BodyLayout::empty()), []);
    }

    #[test]
    fn the_published_fit_reports_the_joints_and_still_wraps_the_body() {
        let layout = default_layout();
        let fit = TemplateFit::new(layout.clone());
        assert!(fit.envelope(), "garments wrap the body through this fit");
        for (bone, at) in joints(&layout) {
            assert_eq!(fit.joint(&JointRef::Humanoid(bone)), Some(at), "{bone}");
        }
        assert_eq!(fit.joint(&JointRef::Humanoid(HumanoidBone::Jaw)), None, "the eyes and jaw are the head part's");
        assert_eq!(fit.section(BodyRegion::Torso, 0.5), layout.section(BodyRegion::Torso, 0.5));
        assert_eq!(fit.layout().head_base(), layout.head_base());
    }

    #[test]
    fn a_bone_rests_with_an_identity_rotation_and_hangs_from_its_parent() {
        let layout = default_layout();
        let skeleton = of_layout(&layout).unwrap();
        assert_eq!(skeleton.len(), 52);
        for (bone, _) in joints(&layout) {
            let def = skeleton.get(skeleton.humanoid_bone(bone).unwrap()).unwrap();
            assert_eq!(def.rest.rotation, Quat::IDENTITY, "{bone}");
            assert_eq!(def.parent, bone.parent().and_then(|p| skeleton.humanoid_bone(p)), "{bone}");
        }
    }

    #[test]
    fn the_rest_is_the_t_pose_and_posing_at_the_bind_reproduces_the_layout() {
        let layout = default_layout();
        let all = joints(&layout);
        let (skeleton, bind) = (of_layout(&layout).unwrap(), bind_pose(&all));
        let at = |bone, points: &[(HumanoidBone, Vec3)]| points.iter().find(|(b, _)| *b == bone).unwrap().1;
        let rest = joints_at(&skeleton, &HumanoidPose::REST);
        for (side, shoulder, elbow, wrist, fingertip) in [
            (
                1.0,
                HumanoidBone::LeftUpperArm,
                HumanoidBone::LeftLowerArm,
                HumanoidBone::LeftHand,
                HumanoidBone::LeftMiddleDistal,
            ),
            (
                -1.0,
                HumanoidBone::RightUpperArm,
                HumanoidBone::RightLowerArm,
                HumanoidBone::RightHand,
                HumanoidBone::RightMiddleDistal,
            ),
        ] {
            let (a, b, c, d) = (at(shoulder, &rest), at(elbow, &rest), at(wrist, &rest), at(fingertip, &rest));
            assert!((b.y - a.y).abs() < 1e-6 && (c.y - b.y).abs() < 1e-6, "level arm: {a} {b} {c}");
            assert!((b.z - a.z).abs() < 1e-6 && (c.z - b.z).abs() < 1e-6, "straight arm: {a} {b} {c}");
            assert!((b.x - a.x) * side > 0.0 && (c.x - b.x) * side > 0.0, "pointing outwards");
            assert!(((d - c).normalize() * side - Vec3::X).length() < 0.1, "the fingers continue along X");
        }
        let at_bind = joints_at(&skeleton, &bind);
        let worst = all.iter().map(|&(bone, p)| p.distance(at(bone, &at_bind))).fold(0.0, f32::max);
        assert!(worst < 1e-6, "posing at the bind is {worst:e} m off the layout");
    }

    #[test]
    fn the_legs_and_trunk_take_no_bind_rotation() {
        let bind = bind_pose(&joints(&default_layout()));
        for bone in HumanoidBone::ALL {
            let leg_or_trunk = !successors(Some(bone), |p| p.parent())
                .any(|a| matches!(a, HumanoidBone::LeftShoulder | HumanoidBone::RightShoulder));
            if leg_or_trunk {
                assert_eq!(bind.get(bone), Quat::IDENTITY, "{bone}");
            }
        }
        assert_ne!(bind.get(HumanoidBone::LeftUpperArm), Quat::IDENTITY);
        assert_eq!(bind.get(HumanoidBone::LeftThumbProximal), Quat::IDENTITY, "the thumb moves with the hand");
    }

    #[test]
    fn an_absent_optional_bone_hands_its_children_to_the_nearest_present_ancestor() {
        let layout = default_layout();
        let without_chest: Vec<_> = joints(&layout).into_iter().filter(|(b, _)| *b != HumanoidBone::Chest).collect();
        let skeleton = assemble(&without_chest, &HumanoidPose::REST).unwrap();
        let id = |bone| skeleton.humanoid_bone(bone).unwrap();
        assert_eq!(skeleton.get(id(HumanoidBone::UpperChest)).unwrap().parent, Some(id(HumanoidBone::Spine)));
        let (spine, upper) = (layout.joint64(HumanoidBone::Spine).unwrap(), layout.joint64(HumanoidBone::UpperChest));
        let rest = skeleton.get(id(HumanoidBone::UpperChest)).unwrap().rest.translation;
        assert!((rest - (upper.unwrap() - spine).as_vec3()).length() < 1e-6, "the offset spans the missing bone");
    }

    #[test]
    fn a_layout_without_the_required_bones_is_an_error_not_a_skeleton() {
        assert!(of_layout(&BodyLayout::empty()).is_err());
        let layout = default_layout();
        let no_hips: Vec<_> = joints(&layout).into_iter().filter(|(b, _)| *b != HumanoidBone::Hips).collect();
        assert!(assemble(&no_hips, &HumanoidPose::REST).is_err());
    }

    #[test]
    fn hand_style_does_not_change_the_skeleton() {
        let skeletons: Vec<_> = [HandStyle::Mitten, HandStyle::Simple, HandStyle::FiveFinger]
            .into_iter()
            .map(|hands| from_template(&SoftSd, &BodyParams { hands, ..BodyParams::default() }).unwrap())
            .collect();
        assert!(skeletons.windows(2).all(|pair| pair[0] == pair[1]));
        let is_finger = |bone: HumanoidBone| {
            successors(bone.parent(), |p| p.parent())
                .any(|a| matches!(a, HumanoidBone::LeftHand | HumanoidBone::RightHand))
        };
        let fingers = HumanoidBone::ALL.into_iter().filter(|b| is_finger(*b));
        assert_eq!(fingers.filter(|b| skeletons[0].humanoid_bone(*b).is_some()).count(), 30);
    }

    #[test]
    fn assembling_a_skeleton_is_within_its_budget() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};

        // The best of many runs, so a busy machine cannot fail the test: it measures the work, not the scheduler.
        let best = |work: &dyn Fn()| {
            (0..200).map(|_| (Instant::now(), work())).map(|(start, ())| start.elapsed()).min().unwrap_or_default()
        };
        let (layout, body) = (default_layout(), BodyParams::default());
        let assembly = best(&|| drop(black_box(of_layout(black_box(&layout)))));
        let whole = best(&|| drop(black_box(from_template(&SoftSd, black_box(&body)))));
        // The unoptimised profile gets 2 ms: a 1 ms bound had no headroom on a slower machine. Release keeps 1 ms.
        let budget = Duration::from_millis(if cfg!(debug_assertions) { 2 } else { 1 });
        eprintln!("skeleton assembly: {assembly:?} from a layout, {whole:?} from the template (layout included)");
        assert!(assembly < budget, "the skeleton took {assembly:?} against {budget:?}");
        // The layout blends the leg into the torso's outline. Best of 200, unoptimised, under a full parallel
        // test run: 3.9 ms on one CI machine, 4.97 ms on the slowest seen; 0.76 ms optimised. The
        // unoptimised bound is 2 x the slowest, rounded up.
        let whole_budget = Duration::from_millis(if cfg!(debug_assertions) { 10 } else { 2 });
        assert!(whole < whole_budget, "layout and skeleton took {whole:?} against {whole_budget:?}");
    }
}
