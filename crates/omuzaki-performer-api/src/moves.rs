//! Big moves: the per-frame [`BigMovePlanner`] and what it reads (`ai-performer-voice-face.md` §2, §5).
//!
//! The planner turns the floor and the speech into lean, head, body-turn and gaze keys on the channels in
//! [`OWNED_CHANNELS`], inside the score layer: an explicit intent beats a planned move, and a human tracker above it
//! always wins.

use omuzaki_anim_api::blend::Layer;
use omuzaki_anim_api::channel::{Channel, ChannelMask};
use omuzaki_anim_api::frame::Tick;
use omuzaki_anim_api::life::Personality;
use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The only channels a planner writes (§5.1): `headYaw`, `headPitch`, `lean`, `bodyTurn` and the eight `eyeLook*`.
pub const OWNED_CHANNELS: ChannelMask = ChannelMask::from_channels(&[
    Channel::HEAD_YAW,
    Channel::HEAD_PITCH,
    Channel::LEAN,
    Channel::BODY_TURN,
    Channel::EYE_LOOK_DOWN_LEFT,
    Channel::EYE_LOOK_DOWN_RIGHT,
    Channel::EYE_LOOK_IN_LEFT,
    Channel::EYE_LOOK_IN_RIGHT,
    Channel::EYE_LOOK_OUT_LEFT,
    Channel::EYE_LOOK_OUT_RIGHT,
    Channel::EYE_LOOK_UP_LEFT,
    Channel::EYE_LOOK_UP_RIGHT,
]);

/// Who holds the floor (the loop's state, `ai-performer.md` §5.1).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Floor {
    /// Nobody is talking.
    #[default]
    Idle,
    /// The human is talking.
    Listening,
    /// A reply is being written.
    Thinking,
    /// The performer is talking.
    Speaking,
}

/// Whom the performer talks to.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Addressee {
    /// The viewers, through the camera.
    #[default]
    Camera,
    /// The chat.
    Chat,
    /// Another member.
    #[non_exhaustive]
    Member {
        /// Radians from this member to the other one.
        yaw: f32,
    },
}

impl Addressee {
    /// [`Addressee::Member`].
    pub fn member(yaw: f32) -> Self {
        Self::Member { yaw }
    }
}

/// The floor around the member this frame. `Copy`: filled by the stage every frame.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MoveContext {
    /// Who holds the floor.
    pub floor: Floor,
    /// Whom the performer talks to.
    pub addressee: Addressee,
    /// A `look` intent holds `IkGoals.look_at`: leave gaze and head yaw alone.
    pub look_held: bool,
    /// A private turn: start no move, ease the active ones back (§5.1).
    pub private: bool,
}

impl MoveContext {
    /// A context with this floor and addressee, no look held, public.
    pub fn new(floor: Floor, addressee: Addressee) -> Self {
        Self { floor, addressee, look_held: false, private: false }
    }

    /// The same, with a `look` intent holding the gaze.
    #[must_use]
    pub fn look_held(mut self) -> Self {
        self.look_held = true;
        self
    }

    /// The same, private.
    #[must_use]
    pub fn private(mut self) -> Self {
        self.private = true;
        self
    }
}

/// The speech this frame, filled by the stage from the handles and the floor (§5.2). Private chunks leave every flag
/// false.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TalkState {
    /// A chunk is playing.
    pub speaking: bool,
    /// The playing chunk ends in `?`.
    pub question: bool,
    /// A `laugh` style tag or a laugh expression is in force.
    pub laughing: bool,
    /// Time since `speaking` last changed.
    pub since: Seconds,
}

impl TalkState {
    /// A state with these flags, `since` the last change of `speaking`.
    pub fn new(speaking: bool, question: bool, laughing: bool, since: Seconds) -> Self {
        Self { speaking, question, laughing, since }
    }
}

/// How big the moves are, `0..=1` each, from the persona's `life` preset (§5.2).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StyleKnobs {
    /// Scales every amplitude by `0.6 + 0.8 × energy`.
    pub energy: f32,
    /// The chance of looking away while thinking.
    pub gaze_aversion: f32,
    /// How far a question leans in.
    pub lean: f32,
    /// How much the body turns to a co-host.
    pub turn: f32,
}

impl StyleKnobs {
    /// Knobs with these values.
    pub fn new(energy: f32, gaze_aversion: f32, lean: f32, turn: f32) -> Self {
        Self { energy, gaze_aversion, lean, turn }
    }

    /// The §5.2 table.
    pub fn for_preset(preset: Personality) -> Self {
        match preset {
            Personality::Energetic => Self::new(0.9, 0.5, 0.7, 0.8),
            Personality::Calm => Self::new(0.4, 0.6, 0.4, 0.5),
            Personality::Shy => Self::new(0.5, 0.9, 0.3, 0.4),
            Personality::Sleepy => Self::new(0.2, 0.7, 0.2, 0.3),
            Personality::Natural => Self::new(0.5, 0.5, 0.5, 0.5),
        }
    }

    /// The factor on every amplitude: `0.6 + 0.8 × energy`.
    pub fn amplitude(&self) -> f32 {
        0.6 + 0.8 * self.energy
    }
}

impl Default for StyleKnobs {
    fn default() -> Self {
        Self::for_preset(Personality::Natural)
    }
}

/// Per frame, no allocation: turns the floor and the speech into keys on the channels it may own.
///
/// The stage samples the member's live score into the `score` layer, then calls `plan` with a scratch `Layer` and
/// copies in only the channels the score did not key. Same seed and inputs give the same `Layer` bits, frame by frame.
pub trait BigMovePlanner: Send {
    /// Writes this frame's moves into `out`: values and mask on [`OWNED_CHANNELS`] only. `rng` is the member's `moves`
    /// stream, forked after life's four.
    fn plan(
        &mut self,
        tick: Tick,
        ctx: &MoveContext,
        talk: &TalkState,
        knobs: &StyleKnobs,
        rng: &mut Rng,
        out: &mut Layer,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knobs_follow_the_preset_table() {
        let row = |p| {
            let k = StyleKnobs::for_preset(p);
            (k.energy, k.gaze_aversion, k.lean, k.turn)
        };
        assert_eq!(row(Personality::Energetic), (0.9, 0.5, 0.7, 0.8));
        assert_eq!(row(Personality::Calm), (0.4, 0.6, 0.4, 0.5));
        assert_eq!(row(Personality::Shy), (0.5, 0.9, 0.3, 0.4));
        assert_eq!(row(Personality::Sleepy), (0.2, 0.7, 0.2, 0.3));
        assert_eq!(row(Personality::Natural), (0.5, 0.5, 0.5, 0.5));
        assert!((StyleKnobs::for_preset(Personality::Energetic).amplitude() - 1.32).abs() < 1e-6);
        assert_eq!(StyleKnobs::default(), StyleKnobs::for_preset(Personality::Natural));
    }

    #[test]
    fn the_planner_owns_twelve_channels_and_no_mouth() {
        assert_eq!(OWNED_CHANNELS.count(), 12);
        assert!(OWNED_CHANNELS.contains(Channel::LEAN) && OWNED_CHANNELS.contains(Channel::EYE_LOOK_UP_RIGHT));
        assert!(!OWNED_CHANNELS.intersects(ChannelMask::MOUTH));
        assert_eq!(OWNED_CHANNELS.without(ChannelMask::GAZE).count(), 4);
    }

    #[test]
    fn cold_types_round_trip() {
        let ctx = MoveContext::new(Floor::Speaking, Addressee::member(0.4)).look_held().private();
        assert_eq!(serde_json::from_str::<MoveContext>(&serde_json::to_string(&ctx).unwrap()).unwrap(), ctx);
        let talk = TalkState::new(true, true, false, Seconds(0.25));
        assert_eq!(serde_json::from_str::<TalkState>(&serde_json::to_string(&talk).unwrap()).unwrap(), talk);
        let knobs = StyleKnobs::for_preset(Personality::Shy);
        assert_eq!(serde_json::from_str::<StyleKnobs>(&serde_json::to_string(&knobs).unwrap()).unwrap(), knobs);
    }
}
