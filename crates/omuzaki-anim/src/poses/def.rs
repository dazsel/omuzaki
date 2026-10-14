//! A pose as authored: [`PoseDef`], its file format and its checks.

use std::collections::BTreeMap;

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{EulerRot, Quat, Vec3};
use omuzaki_core::id::Slug;
use serde::Deserialize;

use super::posture::{Hand, Posture};
use crate::hands::{HandLibrary, HandRest, HandShapeError};

/// Why a pose file or a pose operation failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PoseError {
    /// A library file is not valid TOML of the expected layout.
    #[error("pose file {file}: {message}")]
    Parse {
        /// Which file (its position in the list).
        file: String,
        /// What the parser said.
        message: String,
    },
    /// An id or tag is not a valid name (`a-z`, `0-9`, `-`, `_`).
    #[error("pose {id:?}: {what} {name:?} is not a valid name: {message}")]
    BadName {
        /// The pose.
        id: String,
        /// `"id"` or `"tag"`.
        what: &'static str,
        /// The name as written.
        name: String,
        /// Which rule it breaks.
        message: String,
    },
    /// The summary is empty; `pose.list` would show nothing.
    #[error("pose {0} has no summary")]
    EmptySummary(String),
    /// A number is not finite, or a weight is outside `0..=1`.
    #[error("pose {id}: {field} is {value}, which is not allowed")]
    BadNumber {
        /// The pose.
        id: String,
        /// Which field.
        field: String,
        /// The value found.
        value: f32,
    },
    /// Two library files use the same id.
    #[error("two poses are called {0}")]
    DuplicateId(String),
    /// A hand names a shape the hand-shape library lacks.
    #[error("pose {id}: no hand shape called {shape:?}")]
    UnknownHandShape {
        /// The pose.
        id: String,
        /// The shape id.
        shape: String,
    },
    /// The pose is marked `mirrorable = false` (it carries a handedness, such as a sign).
    #[error("pose {0} is not mirrorable")]
    NotMirrorable(String),
    /// The hand-shape library failed to load.
    #[error(transparent)]
    Hands(#[from] HandShapeError),
}

/// A hand shape on one side of a pose: the shape's id in the hand-shape library and how strongly it is applied.
#[derive(Clone, Debug, PartialEq)]
pub struct HandSlot {
    /// A shape id from [`HandLibrary`].
    pub shape: String,
    /// `0..=1`; `0` leaves the fingers as the bones say.
    pub weight: f32,
}

/// A pose as authored (see the [module](super)). Check one with [`PoseDef::validate`]; parsing is [`PoseLibrary`].
///
/// [`PoseLibrary`]: super::PoseLibrary
#[derive(Clone, Debug, PartialEq)]
pub struct PoseDef {
    /// The library name (`wave-right`).
    pub id: String,
    /// One line saying what the pose is, for `pose.list`.
    pub summary: String,
    /// Free labels (`greeting`, `idle`) to search by.
    pub tags: Vec<String>,
    /// Parent-local rotation of each bone that is not at the T-pose, in the normalised space.
    pub bones: BTreeMap<HumanoidBone, Quat>,
    /// Hips position relative to the rest hips, metres.
    pub hips_offset: Vec3,
    /// The character's left hand shape, if the pose sets one.
    pub left_hand: Option<HandSlot>,
    /// The character's right hand shape, if the pose sets one.
    pub right_hand: Option<HandSlot>,
    /// Whether [`PoseDef::mirrored`] is meaningful for this pose.
    pub mirrorable: bool,
}

/// Reflects a rotation across the centre plane, the rule of `HumanoidPose::mirrored`.
fn reflected(q: Quat) -> Quat {
    Quat::from_xyzw(q.x, -q.y, -q.z, q.w)
}

impl PoseDef {
    /// The pose reflected left to right: sides swap (bones and hands) and rotations reflect. Exact: mirroring twice
    /// gives back the same pose. The result keeps the id, so rename it if both versions are kept.
    ///
    /// # Errors
    /// [`PoseError::NotMirrorable`] if the pose says it is not.
    pub fn mirrored(&self) -> Result<Self, PoseError> {
        if !self.mirrorable {
            return Err(PoseError::NotMirrorable(self.id.clone()));
        }
        Ok(Self {
            bones: self.bones.iter().map(|(b, q)| (b.mirrored(), reflected(*q))).collect(),
            hips_offset: Vec3::new(-self.hips_offset.x, self.hips_offset.y, self.hips_offset.z),
            left_hand: self.right_hand.clone(),
            right_hand: self.left_hand.clone(),
            ..self.clone()
        })
    }

    /// Resolves the hand shape ids against `hands`, giving the pose in the form that blends, mirrors and layers.
    ///
    /// # Errors
    /// [`PoseError::UnknownHandShape`] if a hand names a shape `hands` lacks.
    pub fn posture(&self, hands: &HandLibrary) -> Result<Posture, PoseError> {
        let mut body = HumanoidPose { hips_offset: self.hips_offset, ..HumanoidPose::REST };
        for (bone, rotation) in &self.bones {
            body.set(*bone, *rotation);
        }
        let resolve = |slot: &Option<HandSlot>| match slot {
            None => Ok(Hand::NONE),
            Some(s) => hands
                .get(&s.shape)
                .map(|shape| Hand::of(shape, s.weight))
                .ok_or_else(|| PoseError::UnknownHandShape { id: self.id.clone(), shape: s.shape.clone() }),
        };
        Ok(Posture { body, left: resolve(&self.left_hand)?, right: resolve(&self.right_hand)? })
    }

    /// The pose a character plays: bones as authored, fingers from the hand shapes on a rig whose thumbs rest at the
    /// nominal angle. Use [`PoseDef::posture`] and [`Posture::to_humanoid`] to pass the rig's own [`HandRest`].
    ///
    /// # Errors
    /// An unknown hand shape, or a broken built-in hand library.
    pub fn to_humanoid(&self) -> Result<HumanoidPose, PoseError> {
        Ok(self.posture(HandLibrary::builtin()?)?.to_humanoid(&HandRest::nominal()))
    }

    /// Checks the id and tags are names, the summary says something, every number is finite and the weights are in
    /// `0..=1`.
    ///
    /// # Errors
    /// The first [`PoseError`] found.
    pub fn validate(&self) -> Result<(), PoseError> {
        let bad_name = |what, name: &str| {
            Slug::new(name).err().map(|e| PoseError::BadName {
                id: self.id.clone(),
                what,
                name: name.to_owned(),
                message: e.to_string(),
            })
        };
        if let Some(e) = bad_name("id", &self.id).or_else(|| self.tags.iter().find_map(|t| bad_name("tag", t))) {
            return Err(e);
        }
        if self.summary.trim().is_empty() {
            return Err(PoseError::EmptySummary(self.id.clone()));
        }
        let number = |field: String, value: f32, ok: bool| {
            if ok { Ok(()) } else { Err(PoseError::BadNumber { id: self.id.clone(), field, value }) }
        };
        for (axis, v) in ["x", "y", "z"].into_iter().zip(self.hips_offset.to_array()) {
            number(format!("hips_offset.{axis}"), v, v.is_finite())?;
        }
        for (bone, q) in &self.bones {
            for v in q.to_array() {
                number(format!("bones.{bone}"), v, v.is_finite())?;
            }
        }
        for (side, slot) in [("left", &self.left_hand), ("right", &self.right_hand)] {
            if let Some(s) = slot {
                number(format!("hands.{side}.weight"), s.weight, (0.0..=1.0).contains(&s.weight))?;
            }
        }
        Ok(())
    }
}

/// One hand in a file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandFile {
    shape: String,
    #[serde(default = "full_weight")]
    weight: f32,
}

fn full_weight() -> f32 {
    1.0
}

/// The `[hands]` table of a file.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct HandsFile {
    left: Option<HandFile>,
    right: Option<HandFile>,
}

/// A pose file: bones in degrees.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PoseFile {
    id: String,
    summary: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "yes")]
    mirrorable: bool,
    #[serde(default)]
    hips_offset: [f32; 3],
    #[serde(default)]
    bones: BTreeMap<HumanoidBone, [f32; 3]>,
    #[serde(default)]
    hands: HandsFile,
}

fn yes() -> bool {
    true
}

impl From<HandFile> for HandSlot {
    fn from(f: HandFile) -> Self {
        Self { shape: f.shape, weight: f.weight }
    }
}

impl From<PoseFile> for PoseDef {
    fn from(f: PoseFile) -> Self {
        let euler =
            |[x, y, z]: [f32; 3]| Quat::from_euler(EulerRot::XYZ, x.to_radians(), y.to_radians(), z.to_radians());
        Self {
            id: f.id,
            summary: f.summary,
            tags: f.tags,
            bones: f.bones.into_iter().map(|(b, d)| (b, euler(d))).collect(),
            hips_offset: Vec3::from_array(f.hips_offset),
            left_hand: f.hands.left.map(HandSlot::from),
            right_hand: f.hands.right.map(HandSlot::from),
            mirrorable: f.mirrorable,
        }
    }
}
