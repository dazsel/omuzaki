//! [`LiveScore`]: the score layer played live (`docs/design/drivers.md` §3; anim-api §10).
//!
//! Agents, scripts, automations and the AI performer move a character live through **acts**: an expression shown for
//! a while, a look, a few channels set, a body clip. Each act is a target and an [`Envelope`]: it fades in from the
//! rest face, holds, and fades out (or holds until released). The live score holds the acts playing and, once per
//! performer step, writes them into the performer's score layer and its body clips into the score's pose layers.
//!
//! Acts that play together blend in the order they started: a later act wins where they overlap, by its envelope, so
//! a new one cross-fades over the old. An act **releases** the earlier acts of its own [`Lane`] that it overlaps, so a
//! second expression takes over from the first rather than stacking on it.
//!
//! The score is the lowest driver above the rest face: on a member a webcam drives, the tracker wins wherever it reads
//! the face, unless the member's drive map gates it off (`perform::drive`). That is the PF-23 rule, unchanged.
//!
//! Hot path: [`LiveScore::step`] allocates, locks and frees nothing. A clip is shared ([`Arc`]) with the handle that
//! baked the act, which keeps its own reference until the act has ended (`perform::host`), so the perform thread never
//! drops the last one: nothing is freed there when an act ends.
//!
//! A clip's keys may say where its hands reach on the body (#2414): the score writes those reaches beside the pose
//! layers ([`ScorePose::reaches`]), each at the strength its act plays, and the rig bends the arms to them.

use std::sync::Arc;

use omuzaki_anim::hands::HandRest;
use omuzaki_anim::poses::PoseClip;
use omuzaki_anim_api::{
    BlendMode, BoneMask, ChannelBuf, ChannelMask, Ease, HumanoidBone, HumanoidPose, Layer, PoseLayer, PoseMode, Reaches,
};
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most acts that play at once. A new act past this ends the oldest at once.
pub const MAX_ACTS: usize = 16;
/// The most body clips that blend at once (an outgoing and an incoming one, with room to spare).
pub const MAX_CLIPS: usize = 4;
/// The longest fade or hold an act takes, seconds.
pub const MAX_ACT_S: f32 = 600.0;

/// How an act comes and goes, in seconds of the performer's timeline.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Envelope {
    /// Seconds to fade in from the rest face. `0` snaps.
    pub fade_in: f32,
    /// Seconds to hold at full strength, then fade out; `None` holds until the act is released.
    pub hold: Option<f32>,
    /// Seconds to fade out. `0` snaps.
    pub fade_out: f32,
}

impl Envelope {
    /// In over `fade`, held for `hold` seconds (or until released), out over `fade`.
    pub const fn new(fade: f32, hold: Option<f32>) -> Self {
        Self { fade_in: fade, hold, fade_out: fade }
    }

    /// Every number made finite and held to `0..=`[`MAX_ACT_S`].
    #[must_use]
    pub fn clamped(self) -> Self {
        let held = |s: f32| if s.is_finite() { s.clamp(0.0, MAX_ACT_S) } else { 0.0 };
        Self { fade_in: held(self.fade_in), hold: self.hold.map(held), fade_out: held(self.fade_out) }
    }

    /// The strength at `age` seconds after the start, before easing, and whether the act is over.
    fn at(&self, age: f32) -> (f32, bool) {
        let rise = if self.fade_in > 0.0 { (age / self.fade_in).min(1.0) } else { 1.0 };
        let Some(hold) = self.hold else { return (rise, false) };
        let fall_at = self.fade_in + hold;
        if age < fall_at {
            return (rise, false);
        }
        fall(1.0, age - fall_at, self.fade_out)
    }
}

/// The strength `since` seconds into a fade-out from `from` that takes `over` seconds, and whether it has ended.
fn fall(from: f32, since: f32, over: f32) -> (f32, bool) {
    if over <= 0.0 {
        return (0.0, true);
    }
    let left = from * (1.0 - since / over);
    if left <= 0.0 { (0.0, true) } else { (left, false) }
}

/// Which acts take over from each other: an act releases the earlier acts of its lane that it overlaps.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Expressions: one face at a time.
    Face,
    /// Where the eyes (and the head) point.
    Look,
    /// Channels set by name.
    Channels,
    /// Body clips.
    Body,
}

/// What an act moves.
#[allow(clippy::large_enum_variant)] // held inline in the score's fixed slots: boxing would allocate on the perform thread
#[derive(Clone, Debug, PartialEq)]
pub enum Motion {
    /// The channels in `mask` toward `target` (absolute values, the rest face elsewhere).
    Channels {
        /// The values to show.
        target: ChannelBuf,
        /// The channels it moves.
        mask: ChannelMask,
    },
    /// A body clip from its start, over the bones in `bones`.
    Clip {
        /// The clip, shared with the handle that baked it.
        clip: Arc<PoseClip>,
        /// The thumbs' rest the clip's hand shapes are laid on.
        hands: HandRest,
        /// The bones it moves.
        bones: BoneMask,
        /// How strongly at full strength, `0..=1`.
        weight: f32,
    },
}

impl Motion {
    /// Whether this and `other` move any of the same channels or bones.
    fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Channels { mask: a, .. }, Self::Channels { mask: b, .. }) => a.intersects(*b),
            (Self::Clip { bones: a, .. }, Self::Clip { bones: b, .. }) => {
                HumanoidBone::ALL.into_iter().any(|bone| a.contains(bone) && b.contains(bone))
            }
            _ => false,
        }
    }
}

/// One act: what it moves, how it comes and goes, and its lane.
#[derive(Clone, Debug, PartialEq)]
pub struct Act {
    /// What it moves.
    pub motion: Motion,
    /// How it comes and goes.
    pub envelope: Envelope,
    /// Which acts it takes over from.
    pub lane: Lane,
}

/// Where a playing act is in its life.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Phase {
    /// Queued: it starts on the next step.
    Starting,
    /// Started at this time.
    Playing(f64),
    /// Released, fading out from the next step.
    Releasing { started: f64, from: f32 },
    /// Fading out since `at`, from strength `from`.
    Fading { at: f64, from: f32 },
}

#[derive(Clone, Debug, PartialEq)]
struct Playing {
    id: u64,
    act: Act,
    phase: Phase,
    /// The strength of the last step, eased.
    weight: f32,
}

/// The score's body layers of one step: the first `len` of `layers`, lowest first, and where the clips' hands reach.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ScorePose {
    layers: [PoseLayer; MAX_CLIPS],
    len: usize,
    reaches: Reaches,
}

impl Default for ScorePose {
    fn default() -> Self {
        let none = PoseLayer {
            pose: HumanoidPose::REST,
            mask: BoneMask::EMPTY,
            weight: 0.0,
            mode: PoseMode::Override,
        };
        Self { layers: [none; MAX_CLIPS], len: 0, reaches: Reaches::NONE }
    }
}

impl ScorePose {
    /// The layers to blend, lowest first.
    pub fn layers(&self) -> &[PoseLayer] {
        &self.layers[..self.len]
    }

    /// Where the playing clips' hands reach, each at the strength its clip plays (the strongest per hand).
    pub fn reaches(&self) -> &Reaches {
        &self.reaches
    }

    /// No layer and no reach.
    pub fn clear(&mut self) {
        self.len = 0;
        self.reaches = Reaches::NONE;
    }
}

/// The acts playing on one performer. See the module docs.
#[derive(Clone, Debug)]
pub struct LiveScore {
    /// Oldest first; the first `len` are in use.
    acts: [Option<Playing>; MAX_ACTS],
}

impl Default for LiveScore {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveScore {
    /// No acts.
    pub const fn new() -> Self {
        Self { acts: [const { None }; MAX_ACTS] }
    }

    /// Starts `act` as `id` on the next step, releasing the earlier acts of its lane that it overlaps. With
    /// [`MAX_ACTS`] playing, the oldest ends at once.
    pub fn start(&mut self, id: u64, act: Act) {
        for playing in self.acts.iter_mut().flatten() {
            if playing.act.lane == act.lane && playing.act.motion.overlaps(&act.motion) {
                release(playing);
            }
        }
        if self.acts.iter().all(Option::is_some) {
            self.remove(0);
        }
        if let Some(slot) = self.acts.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(Playing { id, act, phase: Phase::Starting, weight: 0.0 });
        }
    }

    /// Releases act `id`, or every act with `None`: each fades out over its own fade-out from where it is. Returns
    /// how many were released.
    pub fn release(&mut self, id: Option<u64>) -> usize {
        let mut count = 0;
        for playing in self.acts.iter_mut().flatten().filter(|p| id.is_none_or(|id| p.id == id)) {
            release(playing);
            count += 1;
        }
        count
    }

    /// Ends every act at once.
    pub fn clear(&mut self) {
        self.acts = [const { None }; MAX_ACTS];
    }

    /// The ids of the acts playing, oldest first, `0` past the last.
    pub fn ids(&self) -> [u64; MAX_ACTS] {
        let mut ids = [0; MAX_ACTS];
        for (id, playing) in ids.iter_mut().zip(self.acts.iter().flatten()) {
            *id = playing.id;
        }
        ids
    }

    /// Whether nothing plays.
    pub fn is_empty(&self) -> bool {
        self.acts.iter().all(Option::is_none)
    }

    /// Advances to `now` and writes the score: `layer` gets the channels of every act over `rest` (an override layer
    /// at full weight over the channels some act moves, absent when none does) and `pose` the clips' body layers.
    /// Allocation-free.
    #[allow(clippy::cast_possible_truncation)] // ages of seconds fit f32
    pub fn step(&mut self, now: Seconds, rest: &ChannelBuf, layer: &mut Layer, pose: &mut ScorePose) {
        let now = now.0;
        let mut i = 0;
        while i < MAX_ACTS {
            let Some(playing) = self.acts[i].as_mut() else { break };
            let (raw, over) = match playing.phase {
                Phase::Starting => {
                    playing.phase = Phase::Playing(now);
                    playing.act.envelope.at(0.0)
                }
                Phase::Playing(started) => playing.act.envelope.at((now - started) as f32),
                Phase::Releasing { started, from } => {
                    // A release is taken from the strength reached, never from full, so it never jumps up.
                    let from = from.min(playing.act.envelope.at((now - started) as f32).0);
                    playing.phase = Phase::Fading { at: now, from };
                    fall(from, 0.0, playing.act.envelope.fade_out)
                }
                Phase::Fading { at, from } => fall(from, (now - at) as f32, playing.act.envelope.fade_out),
            };
            if over {
                self.remove(i);
                continue;
            }
            playing.weight = Ease::Smooth.apply(raw);
            i += 1;
        }
        layer.values = *rest;
        layer.mask = ChannelMask::EMPTY;
        layer.mode = BlendMode::Override;
        pose.clear();
        for playing in self.acts.iter().flatten() {
            let w = playing.weight;
            match &playing.act.motion {
                Motion::Channels { target, mask } => {
                    layer.values.lerp_masked(target, *mask, w);
                    layer.mask |= *mask;
                }
                Motion::Clip { clip, hands, bones, weight } => {
                    if let Some(slot) = pose.layers.get_mut(pose.len) {
                        let age = match playing.phase {
                            Phase::Playing(started) | Phase::Releasing { started, .. } => (now - started) as f32,
                            Phase::Fading { .. } | Phase::Starting => clip.length(),
                        };
                        let posture = clip.sample_posture(age);
                        slot.pose = posture.to_humanoid(hands);
                        slot.mask = *bones;
                        slot.weight = w * weight;
                        slot.mode = PoseMode::Override;
                        pose.len += 1;
                        // A later clip's reach takes over a hand as its strength passes the earlier one's.
                        pose.reaches = pose.reaches.layered(&posture.reach, w * weight);
                    }
                }
            }
        }
        layer.weight = if layer.mask.is_empty() { 0.0 } else { 1.0 };
    }

    /// Removes the act at `index`, keeping the rest in order.
    fn remove(&mut self, index: usize) {
        if index < MAX_ACTS {
            self.acts[index..].rotate_left(1);
            self.acts[MAX_ACTS - 1] = None;
        }
    }
}

/// Starts `playing`'s fade-out on the next step.
fn release(playing: &mut Playing) {
    playing.phase = match playing.phase {
        // Never shown, so it ends on the next step with nothing to fade.
        Phase::Starting => Phase::Fading { at: 0.0, from: 0.0 },
        Phase::Playing(started) => Phase::Releasing { started, from: 1.0 },
        held @ (Phase::Releasing { .. } | Phase::Fading { .. }) => held,
    };
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // values copied through are compared exactly
mod tests {
    use omuzaki_anim::poses::ClipLibrary;
    use omuzaki_anim_api::Channel;

    use super::*;

    fn face(channel: Channel, value: f32, envelope: Envelope) -> Act {
        let mut target = ChannelBuf::NEUTRAL;
        target[channel] = value;
        Act { motion: Motion::Channels { target, mask: ChannelMask::of(channel) }, envelope, lane: Lane::Face }
    }

    fn run(score: &mut LiveScore, t: f64) -> (Layer, ScorePose) {
        let (mut layer, mut pose) = (Layer::absent(BlendMode::Override), ScorePose::default());
        score.step(Seconds(t), &ChannelBuf::NEUTRAL, &mut layer, &mut pose);
        (layer, pose)
    }

    #[test]
    fn an_act_fades_in_holds_and_fades_out_and_then_is_gone() {
        let mut score = LiveScore::new();
        score.start(1, face(Channel::MOUTH_SMILE_LEFT, 0.8, Envelope::new(0.2, Some(1.0))));
        let smile = |l: &Layer| l.values[Channel::MOUTH_SMILE_LEFT];
        assert_eq!(smile(&run(&mut score, 10.0).0), 0.0, "it starts from the rest face");
        assert!((smile(&run(&mut score, 10.1).0) - 0.4).abs() < 1e-6, "half way in (smoothstep is 0.5 at 0.5)");
        let held = run(&mut score, 10.5).0;
        assert_eq!((smile(&held), held.weight), (0.8, 1.0));
        assert!(held.mask.contains(Channel::MOUTH_SMILE_LEFT));
        assert!((smile(&run(&mut score, 11.3).0) - 0.4).abs() < 1e-6, "half way out");
        let gone = run(&mut score, 11.5).0;
        assert_eq!((gone.weight, gone.mask), (0.0, ChannelMask::EMPTY));
        assert!(score.is_empty());
    }

    #[test]
    fn a_new_face_takes_over_from_the_last_without_a_jump_and_a_release_fades_from_where_it_is() {
        let mut score = LiveScore::new();
        let channel = Channel::BROW_INNER_UP;
        score.start(1, face(channel, 1.0, Envelope::new(0.5, None)));
        run(&mut score, 0.0);
        let before = run(&mut score, 1.0).0.values[channel];
        score.start(2, face(channel, 0.2, Envelope::new(0.5, None)));
        let after = run(&mut score, 1.0 + 1.0 / 60.0).0.values[channel];
        assert!((after - before).abs() < 0.05, "a cross-fade, not a cut: {before} then {after}");
        assert_eq!(run(&mut score, 3.0).0.values[channel], 0.2, "the old face is gone once faded");
        assert_eq!(score.ids()[..2], [2, 0]);

        // Released half way in, it falls from the half it reached, never from full.
        score.start(3, face(Channel::JAW_OPEN, 1.0, Envelope::new(1.0, None)));
        run(&mut score, 4.0);
        let half = run(&mut score, 4.5).0.values[Channel::JAW_OPEN];
        assert_eq!(score.release(Some(3)), 1);
        let next = run(&mut score, 4.5 + 1.0 / 60.0).0.values[Channel::JAW_OPEN];
        let then = run(&mut score, 4.5 + 2.0 / 60.0).0.values[Channel::JAW_OPEN];
        assert!((next - half).abs() < 0.05 && then < next, "{half}, {next}, {then}");
    }

    #[test]
    fn past_the_most_acts_the_oldest_ends_and_releasing_all_empties_the_score() {
        let mut score = LiveScore::new();
        // Acts that move nothing in common never release each other, so every one stays until the score is full.
        let none = Motion::Channels { target: ChannelBuf::NEUTRAL, mask: ChannelMask::EMPTY };
        for id in 1..=MAX_ACTS as u64 + 1 {
            score.start(id, Act { motion: none.clone(), envelope: Envelope::new(0.0, None), lane: Lane::Channels });
        }
        assert_eq!(score.ids()[0], 2, "the first act made room");
        assert_eq!(score.release(None), MAX_ACTS);
        run(&mut score, 0.0);
        run(&mut score, 0.1);
        assert!(score.is_empty());
    }

    #[test]
    fn a_body_clip_plays_from_its_start_into_a_pose_layer() {
        let clips = ClipLibrary::builtin().unwrap();
        let wave = Arc::new(clips.get("wave").unwrap().clone());
        let mut score = LiveScore::new();
        let act = Act {
            motion: Motion::Clip { clip: Arc::clone(&wave), hands: HandRest::nominal(), bones: BoneMask::ALL, weight: 1.0 },
            envelope: Envelope::new(0.0, Some(2.0)),
            lane: Lane::Body,
        };
        score.start(1, act);
        run(&mut score, 5.0);
        let (layer, pose) = run(&mut score, 5.25);
        assert_eq!(layer.weight, 0.0, "a clip moves no channels");
        assert_eq!(pose.layers().len(), 1);
        assert_eq!(pose.layers()[0].pose, wave.sample(0.25, &HandRest::nominal()));
        assert_eq!(pose.layers()[0].weight, 1.0);
    }
}
