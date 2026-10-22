//! The `pose` argument of the `render.*` ops (RG-06): a pose of the pose library by id, mirrored or blended with a
//! second one, resolved into the [`HumanoidPose`] the stage shows. Without one a render shows the character as it was
//! sculpted.

use std::fmt::Write as _;

use omuzaki_anim::hands::{HandLibrary, HandRest};
use omuzaki_anim::poses::{PoseDef, PoseLibrary, Posture};
use omuzaki_anim_api::{HumanoidPose, Skeleton};
use omuzaki_core::text::nearest;
use omuzaki_ops_api::OpError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::body::Body;

/// A pose of the pose library (`pose.list`) for a render to show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoseArg {
    /// The pose's id in the library, such as `wave-right`.
    pub id: String,
    /// Reflect it left to right (the left hand waves for the right). False by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror: bool,
    /// Mix it with a second library pose, before any mirroring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<PoseBlend>,
}

/// A second pose mixed into a [`PoseArg`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoseBlend {
    /// The second pose's id in the library.
    pub with: String,
    /// How far toward it, 0 to 1: 0 is the first pose, 1 the second, 0.5 halfway on every bone.
    pub weight: f32,
}

/// The library pose `id`, or the refusal naming `field` with the nearest id and every id there is.
fn library_pose(field: &str, id: &str) -> Result<&'static PoseDef, OpError> {
    let library = PoseLibrary::builtin().map_err(|e| OpError::internal(format!("the pose library doesn't load: {e}")))?;
    library.get(id).ok_or_else(|| {
        let ids: Vec<&str> = library.iter().map(|p| p.id.as_str()).collect();
        let near = nearest(id, ids.iter().copied()).map(|n| format!("did you mean '{n}'? ")).unwrap_or_default();
        let mut error = OpError::not_found(format!("there is no pose '{id}' in the pose library"))
            .hint(format!("{near}the poses are {} (pose.list)", ids.join(", ")));
        error.field = Some(field.to_owned());
        error
    })
}

impl PoseArg {
    /// A library pose as it is, by id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), mirror: false, blend: None }
    }

    /// Checks the ids, the mirroring and the weight, naming `field` (the argument this pose came in) and its parts.
    ///
    /// # Errors
    /// `NotFound` for an id the library lacks, with the nearest one; `Invalid` for mirroring a pose that is not
    /// mirrorable or a weight outside 0 to 1.
    pub fn check(&self, field: &str) -> Result<(), OpError> {
        let pose = library_pose(&format!("{field}/id"), &self.id)?;
        if let Some(blend) = &self.blend {
            let other = library_pose(&format!("{field}/blend/with"), &blend.with)?;
            if !(blend.weight.is_finite() && (0.0..=1.0).contains(&blend.weight)) {
                return Err(OpError::invalid(
                    format!("{field}/blend/weight"),
                    format!("weight {} is outside 0 to 1", blend.weight),
                ));
            }
            if self.mirror && !other.mirrorable {
                return Err(not_mirrorable(field, &other.id));
            }
        }
        if self.mirror && !pose.mirrorable {
            return Err(not_mirrorable(field, &pose.id));
        }
        Ok(())
    }

    /// The pose as `body` shows it: the library poses (their hand shapes on the body's own thumbs), blended, then
    /// mirrored.
    ///
    /// # Errors
    /// As [`PoseArg::check`]; `Internal` for a library pose whose hand shape the hand library lacks.
    pub fn resolve(&self, field: &str, body: &Body) -> Result<HumanoidPose, OpError> {
        let skeleton = match body {
            Body::Kit(built) => &built.rig.skeleton,
            Body::Avatar(avatar) => &avatar.skeleton,
        };
        self.resolve_on(field, skeleton)
    }

    /// The pose on a rig of `skeleton`: what [`PoseArg::resolve`] gives for a body, and what a turnaround sheet takes
    /// as [`SheetPose::custom`](crate::sheet::turnaround::SheetPose::custom).
    ///
    /// # Errors
    /// As [`PoseArg::resolve`].
    pub fn resolve_on(&self, field: &str, skeleton: &Skeleton) -> Result<HumanoidPose, OpError> {
        self.check(field)?;
        // A rig whose thumbs can't be measured (a broken thumb chain, a file's own bone axes) gets the nominal thumb.
        let rest = HandRest::of(skeleton).unwrap_or_else(|_| HandRest::nominal());
        Ok(self.posture(field)?.to_humanoid(&rest))
    }

    /// The blended, mirrored posture.
    fn posture(&self, field: &str) -> Result<Posture, OpError> {
        let hands = HandLibrary::builtin().map_err(|e| OpError::internal(format!("the hand library doesn't load: {e}")))?;
        let posture = |id: &str, part: &str| {
            library_pose(&format!("{field}/{part}"), id)?
                .posture(hands)
                .map_err(|e| OpError::internal(format!("the library pose '{id}' doesn't resolve: {e}")))
        };
        let mut out = posture(&self.id, "id")?;
        if let Some(blend) = &self.blend {
            out = Posture::blend(&out, &posture(&blend.with, "blend/with")?, blend.weight);
        }
        Ok(if self.mirror { out.mirrored() } else { out })
    }

    /// A short name for the pose, for a label: its id, then `+ <with> <weight>` for a blend and `mirrored`.
    #[must_use]
    pub fn label(&self) -> String {
        let mut label = self.id.clone();
        if let Some(blend) = &self.blend {
            let _ = write!(label, " + {} {:.2}", blend.with, blend.weight);
        }
        if self.mirror {
            label.push_str(" mirrored");
        }
        label
    }
}

fn not_mirrorable(field: &str, id: &str) -> OpError {
    OpError::invalid(format!("{field}/mirror"), format!("the pose '{id}' has a handedness and is not mirrorable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_misspelt_pose_is_refused_with_the_nearest_id() {
        let error = PoseArg::new("wave-rihgt").check("pose").expect_err("refused");
        assert_eq!(error.field.as_deref(), Some("pose/id"));
        assert!(error.hint.as_deref().is_some_and(|h| h.contains("did you mean 'wave-right'")), "{error:?}");
    }

    #[test]
    fn a_weight_outside_the_unit_range_is_refused() {
        let pose = PoseArg {
            blend: Some(PoseBlend { with: "wave-left".into(), weight: 1.5 }),
            ..PoseArg::new("wave-right")
        };
        assert_eq!(pose.check("pose").expect_err("refused").field.as_deref(), Some("pose/blend/weight"));
    }

    #[test]
    fn mirroring_a_mirror_pair_gives_its_other_half() {
        let right = PoseArg { mirror: true, ..PoseArg::new("wave-right") }.posture("pose").expect("resolves");
        let left = PoseArg::new("wave-left").posture("pose").expect("resolves");
        for bone in omuzaki_anim_api::HumanoidBone::ALL {
            let (a, b) = (right.body.get(bone), left.body.get(bone));
            assert!(a.abs_diff_eq(b, 1e-5) || a.abs_diff_eq(-b, 1e-5), "{bone:?}: {a} vs {b}");
        }
    }

    #[test]
    fn labels_say_what_was_mixed() {
        let pose = PoseArg {
            mirror: true,
            blend: Some(PoseBlend { with: "wave-left".into(), weight: 0.25 }),
            ..PoseArg::new("wave-right")
        };
        assert_eq!(pose.label(), "wave-right + wave-left 0.25 mirrored");
    }
}
