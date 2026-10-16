//! The imported rest becomes the VRM T-pose; the mesh stays as authored (design D4, ADR 0014 applied to imports).
//!
//! `VRoid` and `UniVRM` files already rest in T (`VRMC_vrm-1.0/tpose.md`). An FBX or a plain glTF often rests in A. Each
//! limb is checked against its T axis from `tpose.md` (definitions 1.4 and 1.6 to 1.8): the arms, hands and the four
//! fingers along ±X (the character's left is +X, VRM faces +Z), the legs along −Y. A bone more than
//! [`CORRECT_ABOVE_DEG`] off has its rest rotated by the swing that aligns it, parents first, so a corrected upper arm
//! carries the forearm and hand with it before they are measured. The inverse bind matrices are untouched, so the mesh
//! keeps its sculpted pose, and `bind` is the original rest read back as a [`HumanoidPose`] through the skeleton's
//! own [`HumanoidFrames`]: posing the T skeleton at `bind` gives back every original joint.
//!
//! The thumb keeps its authored rest (ADR 0014: its azimuth is read from the rig, never assumed); one more than
//! [`REPORT_ABOVE_DEG`] off the spec's 45° between ±X and +Z is reported (`avatar.thumb_off_axis`). A correction
//! above [`REPORT_ABOVE_DEG`] is reported too (`avatar.rest_normalised`); smaller ones are made silently.

use omuzaki_anim_api::{HumanoidFrames, HumanoidPose, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{Finding, IoScene, NodeIx, Report};

use super::avatar::skeleton::{self, unit};
use super::graph;

/// A limb more than this many degrees off its T axis is turned onto it.
pub const CORRECT_ABOVE_DEG: f32 = 1.0;

/// A correction, or a thumb off the spec's axis, larger than this many degrees is reported.
pub const REPORT_ABOVE_DEG: f32 = 10.0;

/// The T rest's outcome: the pose the mesh was authored in, and what was found.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RestReport {
    /// The original rest as a pose of the T skeleton: the identity for a file that already rests in T.
    pub bind: HumanoidPose,
    /// `avatar.rest_normalised` and `avatar.thumb_off_axis` findings.
    pub report: Report,
}

/// The limbs of one side, parents first, without the `left`/`right` prefix: each bone, the bones its direction is
/// measured to (the first one present wins; none present means the bone's first child node), and whether it is a leg
/// (down −Y) rather than an arm, hand or finger (along ±X).
const LIMBS: [(&str, &[&str], bool); 17] = [
    ("UpperLeg", &["LowerLeg"], true),
    ("LowerLeg", &["Foot"], true),
    ("UpperArm", &["LowerArm"], false),
    ("LowerArm", &["Hand"], false),
    ("Hand", &["MiddleProximal", "IndexProximal", "RingProximal", "LittleProximal"], false),
    ("IndexProximal", &["IndexIntermediate"], false),
    ("IndexIntermediate", &["IndexDistal"], false),
    ("IndexDistal", &[], false),
    ("MiddleProximal", &["MiddleIntermediate"], false),
    ("MiddleIntermediate", &["MiddleDistal"], false),
    ("MiddleDistal", &[], false),
    ("RingProximal", &["RingIntermediate"], false),
    ("RingIntermediate", &["RingDistal"], false),
    ("RingDistal", &[], false),
    ("LittleProximal", &["LittleIntermediate"], false),
    ("LittleIntermediate", &["LittleDistal"], false),
    ("LittleDistal", &[], false),
];

/// Turns every limb of `scene`'s humanoid onto its T axis (see the module docs). A scene without a humanoid is left
/// alone with an identity `bind`.
pub fn normalise_rest(scene: &mut IoScene) -> RestReport {
    let mut report = Report::new();
    let Some(humanoid) = scene.avatar.as_ref().map(|a| a.humanoid.clone()) else {
        return RestReport { bind: HumanoidPose::REST, report };
    };
    let node = |name: &str| humanoid.node(name);
    let original = skeleton::build(scene).ok();
    let mut corrections = Vec::new();
    for side in ["left", "right"] {
        let sign = if side == "left" { 1.0 } else { -1.0 };
        for (bone, tails, leg) in LIMBS {
            let name = format!("{side}{bone}");
            let Some(at) = node(&name) else { continue };
            let tail = tails.iter().find_map(|t| node(&format!("{side}{t}"))).or_else(|| first_child(scene, at));
            let Some(tail) = tail else { continue };
            let axis = if leg { Vec3::NEG_Y } else { Vec3::X * sign };
            if let Some(degrees) = swing_onto(scene, at, tail, axis) {
                corrections.push((name, degrees));
            }
        }
    }
    let large: Vec<String> =
        corrections.iter().filter(|(_, d)| *d > REPORT_ABOVE_DEG).map(|(bone, d)| format!("{bone} {d:.1}°")).collect();
    if !large.is_empty() {
        let msg = format!("the model rests off the T-pose; its rest was turned onto it ({})", large.join(", "));
        let hint = "nothing to do: the mesh keeps its sculpted pose, and poses and clips now mean the same as on any character";
        report.push(Finding::info("avatar.rest_normalised", msg).at("avatar/rest").hint(hint));
    }
    for side in ["left", "right"] {
        check_thumb(scene, side, &humanoid, &mut report);
    }
    let bind = match (original, skeleton::build(scene)) {
        (Some(before), Ok(after)) if !corrections.is_empty() => authored_pose(&before, &after, &mut report),
        _ => HumanoidPose::REST,
    };
    RestReport { bind, report }
}

/// Turns node `at` by the swing that points it (toward `tail`) along `axis`, when it is more than
/// [`CORRECT_ABOVE_DEG`] off; returns the angle it turned, in degrees.
fn swing_onto(scene: &mut IoScene, at: NodeIx, tail: NodeIx, axis: Vec3) -> Option<f32> {
    let parent = graph::parents(scene);
    let world = graph::world(scene, &graph::hierarchy_order(scene, &parent, &|_| true));
    let position = |n: NodeIx| world.position(n.index());
    let direction = (position(tail)? - position(at)?).try_normalize()?;
    let degrees = direction.angle_between(axis).to_degrees();
    if degrees.is_nan() || degrees <= CORRECT_ABOVE_DEG {
        return None;
    }
    let swing = Quat::from_rotation_arc(direction, axis);
    // In world space the bone turns by `swing` about its own origin; in its parent's frame that is the swing seen from
    // the parent: `Pw⁻¹ · swing · Pw · local`.
    let up = parent.get(at.index()).copied().flatten().and_then(|p| world.matrices.get(p));
    let parent_rotation = up.map_or(Quat::IDENTITY, |m| unit(m.to_scale_rotation_translation().1));
    let node = scene.nodes.get_mut(at.index())?;
    let local = parent_rotation.conjugate() * swing * parent_rotation * node.transform.rotation;
    node.transform.rotation = unit(local);
    Some(degrees)
}

/// The first child of `node`, if it has one.
fn first_child(scene: &IoScene, node: NodeIx) -> Option<NodeIx> {
    scene.nodes.get(node.index())?.children.first().copied()
}

/// Reports a thumb more than [`REPORT_ABOVE_DEG`] off the spec's axis: 45° between ±X and +Z (`tpose.md` 1.8).
fn check_thumb(scene: &IoScene, side: &str, humanoid: &omuzaki_io_api::Humanoid, report: &mut Report) {
    let chain: Vec<NodeIx> = ["ThumbMetacarpal", "ThumbProximal", "ThumbDistal"]
        .iter()
        .filter_map(|b| humanoid.node(&format!("{side}{b}")))
        .collect();
    let (Some(&root), Some(&next)) = (chain.first(), chain.get(1)) else { return };
    let world = graph::world_of(scene);
    let position = |n: NodeIx| world.position(n.index());
    let (Some(a), Some(b)) = (position(root), position(next)) else { return };
    let Some(direction) = (b - a).try_normalize() else { return };
    let sign = if side == "left" { 1.0 } else { -1.0 };
    let spec = Vec3::new(sign, 0.0, 1.0).normalize();
    let degrees = direction.angle_between(spec).to_degrees();
    if degrees > REPORT_ABOVE_DEG {
        let msg =
            format!("the {side} thumb rests {degrees:.1}° off the VRM thumb axis (45° between the side and the front)");
        let hint = "the thumb keeps its authored rest; poses written for the VRM axis may close the hand differently";
        report.push(
            Finding::info("avatar.thumb_off_axis", msg).at(format!("avatar/humanoid/{side}ThumbMetacarpal")).hint(hint),
        );
    }
}

/// The authored rest as a pose of the corrected skeleton: `before` is the skeleton as the file rested, `after` the
/// same bones in T. Both are built from the same nodes, so their bone ids match.
fn authored_pose(before: &skeleton::Rig, after: &skeleton::Rig, report: &mut Report) -> HumanoidPose {
    let rest: Vec<Transform> = after.skeleton.bones().iter().map(|b| b.rest).collect();
    let mut bind = HumanoidPose::REST;
    let frames = HumanoidFrames::new(&after.skeleton);
    if let Err(e) = frames.to_humanoid(&rest, &before.skeleton.rest_pose(), &mut bind) {
        // Both skeletons come from the same nodes, so their lengths match; if they ever did not, the import must stop.
        let msg = format!("the authored rest could not be read back as a pose: {e}");
        report.push(Finding::error("avatar.rest_unread", msg).at("avatar/rest"));
    }
    bind
}
