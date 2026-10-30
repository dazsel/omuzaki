//! The pose library (spec RG-06): named, mirrorable humanoid poses that blend, mirror and layer.
//!
//! Two types, on purpose:
//! - [`PoseDef`] is a pose **as authored**: a library file with bone rotations, a hips offset and a hand shape per side
//!   named by id. It is data (`library/poses/*.toml`, embedded with `include_str!`, parsed once by
//!   [`PoseLibrary::builtin`]). The query op `pose.list` lists it.
//! - [`Posture`] is a pose **ready to combine**: the bones as a [`HumanoidPose`] and, per side, the resolved
//!   [`Hand`] (plain angles and a weight per finger bone, `Copy`, no names). [`Posture::blend`],
//!   [`Posture::mirrored`] and [`Posture::layer`] work on it, and [`Posture::to_humanoid`] writes the fingers in and
//!   gives the pose the animator plays. Hand shapes are kept apart from the bones until the end because two shapes mix as angles, which stays a
//!   valid hand, where mixing the finished finger rotations of two hands is only approximately one.
//!
//! # File format
//!
//! One TOML file per pose. Rotations are **degrees, Euler XYZ** (a turn about X, then Y, then Z, applied in that
//! order) in the VRM normalised space where the identity is the T-pose (ADR 0014), so a file reads as what the bone
//! does. A bone not listed stays at the T-pose.
//!
//! ```toml
//! id = "wave-right"
//! summary = "..."
//! tags = ["greeting"]
//! mirrorable = true
//! hips_offset = [0.0, 0.0, 0.0]        # metres; optional
//! [bones]
//! rightUpperArm = [0.0, 0.0, -60.0]
//! [hands.right]                         # optional, per side
//! shape = "open"                        # a hand-shape id (RG-09)
//! weight = 1.0
//! [reach.right]                         # optional, per side (#2414): where the hand goes on the body
//! bone = "upperChest"                   # the bone the target rides on
//! at = [-0.1, 0.05, 0.35]               # from its joint, in arm lengths, along the T-pose axes turned with it
//! pole = [0.0, -1.0, -0.3]              # optional: the side the elbow bends to
//! weight = 1.0
//! ```
//!
//! A reach makes a pose fit any body: the rig bends the arm (two-bone IK, `omuzaki_rig::ik::reach`) so the wrist lands
//! on the target, wherever this body's chest or shoulder is and however long its arms are.
//!
//! Left and right follow the T-pose's mirror rule (`HumanoidPose::mirrored`): the left version of a right-hand pose has
//! the sides swapped, X kept and Y and Z negated, so a library may author only one side and mirror the other.

mod clip;
mod clip_set;
mod def;
mod hips;
mod library;
mod ops;
mod posture;
mod set;

pub use clip::{ClipError, ClipKey, ClipLibrary, Easing, PoseClip};
pub use clip_set::{
    ClipBody, ContactsBody, DeleteClip, KeyArg, KeyBody, ListClips, PoseRef, SetClip, character_clip, find_clip,
};
pub use def::{HandSlot, MAX_REACH, PoseDef, PoseError};
pub use hips::{HipsScaleError, hips_rest_height, retarget_hips, scale_hips};
pub use library::PoseLibrary;
pub use posture::{BoneMix, Hand, PoseMask, Posture};
pub use set::{HandBody, HandsBody, PoseBody, ReachesBody};
