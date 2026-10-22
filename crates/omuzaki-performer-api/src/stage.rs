//! The stage seam: where intents become score cues on the performer's cast member (`ai-performer.md` §2, §7).

use std::fmt;

use omuzaki_anim_api::score::{ClipId, ExpressionId};
use omuzaki_core::time::Seconds;
use omuzaki_stage_api::ids::{MemberId, PropId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::intent::{GazeTarget, Intent, PlaceId};
use crate::limits::PLAY_CLIP_MAX_S;

/// The perform pipeline's side of the performer (App `omuzaki-stage`, registered as a `Services` entry so the
/// `performer.intent` op reaches it). Shared by the op and the performer thread, so it takes `&self`.
pub trait Stage: Send + Sync {
    /// Compiles `intent` into a score fragment anchored at `at` on `member`'s live score (§7.2).
    ///
    /// # Errors
    /// [`StageError::UnknownMember`] for a member not in the scene, [`StageError::Refused`] for an intent outside
    /// the member's [`Inventory`] ([`Inventory::check`]).
    fn apply(&self, member: &MemberId, intent: &Intent, at: Seconds) -> Result<(), StageError>;

    /// Drops `member`'s pending cues that start at or after `from` (a cancelled turn, §5.6). Cues already playing
    /// finish their own fades.
    fn release(&self, member: &MemberId, from: Seconds);

    /// What `member` can do now: the names its intents may use, for the prompt and for validation.
    ///
    /// # Errors
    /// [`StageError::UnknownMember`] for a member not in the scene.
    fn inventory(&self, member: &MemberId) -> Result<Inventory, StageError>;
}

/// A clip the member can play.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ClipInfo {
    /// The clip.
    pub id: ClipId,
    /// Tagged `gesture`: usable by `[gesture]`, not only `[clip]`.
    pub gesture: bool,
    /// Its length in seconds.
    pub duration_s: f32,
}

impl ClipInfo {
    /// A clip of `duration_s` seconds, a gesture or not.
    pub fn new(id: ClipId, gesture: bool, duration_s: f32) -> Self {
        Self { id, gesture, duration_s }
    }
}

/// The names a member's intents may use (§7.1 "checked against `inventory`"). Also the prompt's scene block (§6.3).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Inventory {
    /// The character's `ExpressionSet`.
    pub expressions: Vec<ExpressionId>,
    /// The character's clips.
    pub clips: Vec<ClipInfo>,
    /// The other members on stage.
    pub members: Vec<MemberId>,
    /// The scene's props.
    pub props: Vec<PropId>,
}

impl Inventory {
    /// An inventory of these names.
    pub fn new(
        expressions: Vec<ExpressionId>,
        clips: Vec<ClipInfo>,
        members: Vec<MemberId>,
        props: Vec<PropId>,
    ) -> Self {
        Self { expressions, clips, members, props }
    }

    /// Whether `intent` names only things in this inventory. Ranges are not checked here: [`Intent::clamped`]
    /// handles them.
    ///
    /// # Errors
    /// The first [`Refusal`].
    pub fn check(&self, intent: &Intent) -> Result<(), Refusal> {
        match intent {
            Intent::Emote { expression, .. } if !self.expressions.contains(expression) => {
                Err(Refusal::UnknownExpression)
            }
            Intent::Gesture { clip, .. } => match self.clip(clip) {
                None => Err(Refusal::UnknownClip),
                Some(c) if !c.gesture => Err(Refusal::NotAGesture),
                Some(_) => Ok(()),
            },
            Intent::PlayClip { clip } => match self.clip(clip) {
                None => Err(Refusal::UnknownClip),
                Some(c) if c.duration_s > PLAY_CLIP_MAX_S => Err(Refusal::ClipTooLong),
                Some(_) => Ok(()),
            },
            Intent::LookAt { target: GazeTarget::Member(m), .. } | Intent::MoveTo { place: PlaceId::Member(m) } => {
                self.members.contains(m).then_some(()).ok_or(Refusal::UnknownMember)
            }
            Intent::LookAt { target: GazeTarget::Prop(p), .. } | Intent::MoveTo { place: PlaceId::Prop(p) } => {
                self.props.contains(p).then_some(()).ok_or(Refusal::UnknownProp)
            }
            _ => Ok(()),
        }
    }

    fn clip(&self, id: &ClipId) -> Option<&ClipInfo> {
        self.clips.iter().find(|c| &c.id == id)
    }
}

/// Why an intent is outside the inventory. Its text is what the transcript's `filtered` record says (`"why"`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Refusal {
    /// No such expression.
    UnknownExpression,
    /// No such clip.
    UnknownClip,
    /// The clip is not tagged `gesture`.
    NotAGesture,
    /// The clip is longer than [`PLAY_CLIP_MAX_S`].
    ClipTooLong,
    /// No such member on stage.
    UnknownMember,
    /// No such prop in the scene.
    UnknownProp,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownExpression => "unknown expression",
            Self::UnknownClip => "unknown clip",
            Self::NotAGesture => "not a gesture",
            Self::ClipTooLong => "clip too long",
            Self::UnknownMember => "unknown member",
            Self::UnknownProp => "unknown prop",
        })
    }
}

/// Why the stage refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StageError {
    /// The member is not in the scene.
    #[error("member {member} is not in the scene")]
    #[non_exhaustive]
    UnknownMember {
        /// The member asked for.
        member: MemberId,
    },
    /// The intent is outside the member's inventory.
    #[error("refused: {0}")]
    Refused(Refusal),
    /// The perform pipeline is not running (no scene live).
    #[error("the stage is not live")]
    NotLive,
}

impl StageError {
    /// [`StageError::UnknownMember`].
    pub fn unknown_member(member: &MemberId) -> Self {
        Self::UnknownMember { member: member.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::StyleTag;

    fn inventory() -> Inventory {
        Inventory::new(
            vec![ExpressionId::new("happy").unwrap()],
            vec![
                ClipInfo::new(ClipId::new("wave").unwrap(), true, 1.2),
                ClipInfo::new(ClipId::new("dance").unwrap(), false, 12.0),
                ClipInfo::new(ClipId::new("film").unwrap(), false, 45.0),
            ],
            vec![MemberId::new("kai").unwrap()],
            vec![PropId::new("lamp").unwrap()],
        )
    }

    #[test]
    fn intents_inside_the_inventory_pass() {
        let inv = inventory();
        let ok = [
            Intent::emote(ExpressionId::new("happy").unwrap(), 1.0, 1.5),
            Intent::gesture(ClipId::new("wave").unwrap(), 1.0),
            Intent::play_clip(ClipId::new("dance").unwrap()),
            Intent::look_at(GazeTarget::Camera, 1.0),
            Intent::look_at(GazeTarget::Member(MemberId::new("kai").unwrap()), 1.0),
            Intent::move_to(PlaceId::Prop(PropId::new("lamp").unwrap())),
            Intent::move_to(PlaceId::Home),
            Intent::speak("hi", vec![StyleTag::Calm]),
        ];
        for intent in ok {
            assert_eq!(inv.check(&intent), Ok(()), "{intent:?}");
        }
    }

    #[test]
    fn names_outside_it_are_refused_with_a_reason() {
        let inv = inventory();
        let cases = [
            (Intent::emote(ExpressionId::new("smug").unwrap(), 1.0, 1.5), Refusal::UnknownExpression),
            (Intent::play_clip(ClipId::new("backflip").unwrap()), Refusal::UnknownClip),
            (Intent::gesture(ClipId::new("dance").unwrap(), 1.0), Refusal::NotAGesture),
            (Intent::play_clip(ClipId::new("film").unwrap()), Refusal::ClipTooLong),
            (Intent::move_to(PlaceId::Member(MemberId::new("ann").unwrap())), Refusal::UnknownMember),
            (Intent::look_at(GazeTarget::Prop(PropId::new("door").unwrap()), 1.0), Refusal::UnknownProp),
        ];
        for (intent, why) in cases {
            assert_eq!(inv.check(&intent), Err(why), "{intent:?}");
        }
        assert_eq!(Refusal::UnknownClip.to_string(), "unknown clip");
    }

    #[test]
    fn inventory_and_errors_round_trip() {
        let inv = inventory();
        assert_eq!(serde_json::from_str::<Inventory>(&serde_json::to_string(&inv).unwrap()).unwrap(), inv);
        let e = StageError::unknown_member(&MemberId::new("kai").unwrap());
        assert_eq!(serde_json::from_str::<StageError>(&serde_json::to_string(&e).unwrap()).unwrap(), e);
        assert_eq!(StageError::Refused(Refusal::UnknownClip).to_string(), "refused: unknown clip");
    }
}
