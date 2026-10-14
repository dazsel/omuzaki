//! The T-pose rule: `vrm.tpose`, a warning when an export's rest pose has an arm or a leg off its T-pose axis.
//!
//! VRM 1.0 requires the rest pose to be a T (`tpose.md`), while exporters never fix a pose (io-api.md D9), so the
//! rule only reports. A model sculpted in an A-pose keeps its look by baking the pose into the inverse bind
//! matrices; the rest pose stays a T (ADR 0014).
//!
//! Each limb segment's direction runs between the world origins of consecutive bones and is compared with its
//! ideal axis: the left arm against +X, the right arm against -X, the legs against -Y. The scene is always in the
//! 1.0 orientation (front +Z, the character's left on +X) for both versions, because the 0.x turn happens after
//! the check, so one set of axes serves both exporters.
//!
//! Out of scope: hands, fingers, spine, neck, head and shoulders. A bone that can't be measured (missing, a
//! segment shorter than 1e-6 m, a non-finite origin, a node on a parent cycle) is skipped silently, so the check is
//! total on broken scenes; `run_export` calls it even after preflight found errors.

use std::fmt::Write;

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{Finding, IoScene, Report};

use crate::avatar::parent_table;

/// Degrees a segment may leave its ideal axis before the rule fires. One value for arms and legs.
pub(crate) const TOLERANCE_DEGREES: f32 = 10.0;

/// Numerical margin on the tolerance: f32 rotations and the `acos` near 1 put a nominal 10 degree bend a few
/// thousandths of a degree either way, and a limb exactly on the limit must stay clean (strict mode would turn the
/// false warning into an error).
const MARGIN_DEGREES: f32 = 0.05;

/// Segments shorter than this (metres) have no usable direction.
const MIN_LENGTH: f32 = 1e-6;

/// One limb segment: the direction from `from`'s world origin to `to`'s must be `axis`.
struct Segment {
    from: &'static str,
    to: &'static str,
    axis: Vec3,
    axis_name: &'static str,
}

const fn seg(from: &'static str, to: &'static str, axis: Vec3, axis_name: &'static str) -> Segment {
    Segment { from, to, axis, axis_name }
}

/// The checked segments, in the order that breaks ties (the earlier row wins).
const SEGMENTS: [Segment; 8] = [
    seg("leftUpperArm", "leftLowerArm", Vec3::X, "+X"),
    seg("leftLowerArm", "leftHand", Vec3::X, "+X"),
    seg("rightUpperArm", "rightLowerArm", Vec3::NEG_X, "-X"),
    seg("rightLowerArm", "rightHand", Vec3::NEG_X, "-X"),
    seg("leftUpperLeg", "leftLowerLeg", Vec3::NEG_Y, "-Y"),
    seg("leftLowerLeg", "leftFoot", Vec3::NEG_Y, "-Y"),
    seg("rightUpperLeg", "rightLowerLeg", Vec3::NEG_Y, "-Y"),
    seg("rightLowerLeg", "rightFoot", Vec3::NEG_Y, "-Y"),
];

/// How far one segment is from its ideal axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Deviation {
    /// The segment's first bone, which the finding names.
    pub(crate) bone: &'static str,
    /// The ideal axis, for the message.
    pub(crate) axis: &'static str,
    /// Angle between the segment and the axis.
    pub(crate) degrees: f32,
}

/// The angle in degrees between `dir` and the unit `axis`; `None` if `dir` is too short or not finite.
fn angle_to(dir: Vec3, axis: Vec3) -> Option<f32> {
    let len = dir.length();
    if !len.is_finite() || len < MIN_LENGTH {
        return None;
    }
    Some((dir / len).dot(axis).clamp(-1.0, 1.0).acos().to_degrees())
}

/// World origin of `node`: the parent chain is collected with a bound of `nodes.len()` steps (a cycle gives
/// `None`), then composed root first. `None` if the result isn't finite.
fn world_origin(scene: &IoScene, parent: &[Option<usize>], node: usize) -> Option<Vec3> {
    let mut chain = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        if chain.len() > scene.nodes.len() {
            return None;
        }
        chain.push(n);
        cur = parent.get(n).copied().flatten();
    }
    let mut world = Mat4::IDENTITY;
    for &n in chain.iter().rev() {
        let t = &scene.nodes.get(n)?.transform;
        world *= Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
    }
    let origin = world.w_axis.truncate();
    origin.is_finite().then_some(origin)
}

/// Every measurable segment's deviation, in table order. Total on broken scenes.
pub(crate) fn deviations(scene: &IoScene) -> Vec<Deviation> {
    let Some(avatar) = &scene.avatar else { return Vec::new() };
    let parent = parent_table(scene);
    let origin = |bone: &str| {
        let node = avatar.humanoid.node(bone)?;
        world_origin(scene, &parent, node.index())
    };
    SEGMENTS
        .iter()
        .filter_map(|s| {
            let degrees = angle_to(origin(s.to)? - origin(s.from)?, s.axis)?;
            Some(Deviation { bone: s.from, axis: s.axis_name, degrees })
        })
        .collect()
}

/// Zero or one `vrm.tpose` warning, naming the worst segment.
pub(crate) fn check(scene: &IoScene) -> Report {
    let mut report = Report::new();
    let off: Vec<Deviation> =
        deviations(scene).into_iter().filter(|d| d.degrees > TOLERANCE_DEGREES + MARGIN_DEGREES).collect();
    // `max_by` keeps the last of equal maxima; fold keeps the first, which is the earlier table row.
    let Some(worst) = off.iter().copied().reduce(|a, b| if b.degrees > a.degrees { b } else { a }) else {
        return report;
    };
    let more = off.len() - 1;
    let mut message = format!(
        "the rest pose is not a T-pose: {} is {:.0} degrees off the {} axis",
        worst.bone, worst.degrees, worst.axis
    );
    if more > 0 {
        let (verb, plural) = if more == 1 { ("is", "") } else { ("are", "s") };
        // Writing to a `String` can't fail.
        let _ = write!(message, " ({more} more bone{plural} {verb} over {TOLERANCE_DEGREES:.0} degrees)");
    }
    report.push(Finding::warning("vrm.tpose", message).at(format!("avatar/humanoid/{}", worst.bone)).hint(
        "pose the model in T before exporting; keep an A-pose sculpt by baking it into the inverse bind \
                 matrices (ADR 0014)",
    ));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_to_an_axis() {
        assert_eq!(angle_to(Vec3::new(2.0, 0.0, 0.0), Vec3::X), Some(0.0));
        assert!((angle_to(Vec3::new(1.0, -1.0, 0.0), Vec3::X).unwrap_or(0.0) - 45.0).abs() < 1e-3);
        assert!((angle_to(Vec3::Z, Vec3::X).unwrap_or(0.0) - 90.0).abs() < 1e-3);
        assert!((angle_to(Vec3::NEG_X, Vec3::X).unwrap_or(0.0) - 180.0).abs() < 1e-3);
    }

    #[test]
    fn unusable_directions_are_skipped() {
        assert_eq!(angle_to(Vec3::new(1e-7, 0.0, 0.0), Vec3::X), None);
        assert_eq!(angle_to(Vec3::ZERO, Vec3::X), None);
        assert_eq!(angle_to(Vec3::new(f32::NAN, 0.0, 0.0), Vec3::X), None);
    }

    #[test]
    fn an_empty_scene_has_nothing_to_measure() {
        assert_eq!(deviations(&IoScene::default()), Vec::new());
        assert_eq!(check(&IoScene::default()), Report::new());
    }
}
