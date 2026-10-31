//! The living body (spec PF-24, AP-05; master checklist §8, "whole-body inference from face and voice"): an
//! [`Animator`] that gives a character whose only input is a face (a webcam) or a voice (an AI co-host, a short) a
//! body that moves like a person's.
//!
//! A tracked face on a frozen body is the commonest tell of a puppet. Under the head the bus animator moves only the
//! lean, the breath and the body turn, so the chest, shoulders and arms stand still for hours. [`LivingBody`] runs the
//! bus animator and then infers the rest of the body from what the face and the voice already say:
//!
//! | Layer | Reads | Moves |
//! |---|---|---|
//! | Follow | head yaw, pitch and roll | the chest and shoulders take a share of a head turn, with a lag; the head itself stays exactly where tracking put it |
//! | Breath | the life layer's breath | the chest lifts and the shoulders rise on each breath |
//! | Weight shifts and sway | time (seeded) | the hips roll onto one leg every few seconds and the spine counters it; a slow sway under everything |
//! | Emotion posture | smile, frown, brows, wide eyes | proud on a smile, slumped when sad, shoulders up in anger or surprise; a laugh shakes the shoulders |
//! | Speech accents | stressed syllables | a small push of the chest on each stress |
//! | Gestures | loudness and stressed syllables | co-speech gestures whose strokes land on the stresses ([`gesture`]), a talk-rest pose between them, idle fidgets |
//! | Follow-through | the torso's motion | the arms lag behind the torso and swing back |
//! | Reactions | [`Animator::react`] | a blow or a fall knocks an active ragdoll off the pose and back within its recovery time ([`react`]) |
//!
//! Every amount is a [`BodyStyle`] dial over a preset ([`tune`]); a style of all zeros is the bus animator exactly.
//! Bones another source drives ([`Animator::set_tracked`]: a tracked body or hands, a playing body clip) are handed
//! back to it smoothly. Same seed and same inputs give the same motion (the contract's determinism rule).
//!
//! Hot path: [`Animator::animate`] allocates nothing; a counting-allocator test holds it to that.

pub mod arm;
pub mod gesture;
pub mod measure;
pub mod ops;
pub mod react;
pub mod spring;
pub mod tune;

use std::f32::consts::TAU;

use omuzaki_anim_api::{
    Animator, BodyStyle, BoneMask, Bus, Channel, Cue, Cues, HumanoidBone as B, HumanoidPose, Impact, Reaches, Tick,
};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;

use self::arm::{ArmPose, Side};
use self::gesture::scheduler::Output;
use self::gesture::{GestureLibrary, GestureScheduler, Phase, StrokeLog};
use self::react::Reaction;
use self::spring::{Feel, Spring};
use self::tune::Tuning;
use crate::animator::BusAnimator;
use crate::hands::{HandLibrary, HandRest};
use crate::poses::{Hand, Posture};

/// The torso bones the layer turns, root first.
const TORSO: [B; 3] = [B::Spine, B::Chest, B::UpperChest];
/// Shares of the torso's turn taken by spine, chest and upper chest.
const TORSO_SHARE: [f32; 3] = [0.3, 0.45, 0.25];
/// Frequencies of the sway's three sines, Hz: incommensurate, so the sum never visibly repeats.
const SWAY_HZ: [f32; 3] = [0.071, 0.13, 0.217];
/// The laugh's shoulder shake, Hz.
const LAUGH_HZ: f32 = 4.6;
/// How fast a hand shape changes, per second (an exponential approach).
const HAND_RATE: f32 = 14.0;
/// How fast bones go to or come back from another source, Hz.
const YIELD_FEEL: Feel = Feel::new(4.0, 1.0);

/// The bone groups that are handed to another source together.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Group {
    Torso,
    LeftArm,
    RightArm,
    LeftFingers,
    RightFingers,
}

impl Group {
    const ALL: [Self; 5] = [Self::Torso, Self::LeftArm, Self::RightArm, Self::LeftFingers, Self::RightFingers];

    fn of(bone: B) -> Option<Self> {
        let name = bone.name();
        let left = name.starts_with("left");
        let finger = ["Thumb", "Index", "Middle", "Ring", "Little"].iter().any(|f| name.contains(f));
        let arm = ["Shoulder", "UpperArm", "LowerArm", "Hand"].iter().any(|a| name.ends_with(a));
        Some(match bone {
            B::Hips | B::Spine | B::Chest | B::UpperChest | B::LeftUpperLeg | B::RightUpperLeg => Self::Torso,
            _ if finger && left => Self::LeftFingers,
            _ if finger => Self::RightFingers,
            _ if arm && left => Self::LeftArm,
            _ if arm => Self::RightArm,
            _ => return None,
        })
    }
}

/// The torso's springs.
#[derive(Clone, Debug, Default)]
struct Torso {
    /// The share of the head's yaw, pitch and roll the chest has taken so far, radians.
    follow: [Spring; 3],
    /// Weight shift, `-1` (onto the left leg) to `1`.
    shift: Spring,
    shift_target: f32,
    next_shift_s: f32,
    /// Emotion posture: slump (spine pitch), chest pitch, shrug, shoulders forward; degrees.
    posture: [Spring; 4],
    /// Laugh strength.
    laugh: Spring,
    laugh_phase: f32,
    /// The chest's push on a stress, degrees.
    accent: Spring,
    /// The gesture's chest accent: pitch, yaw, roll, degrees.
    chest: [Spring; 3],
    /// The torso's rotation the arms lag behind (follow-through), as a rotation vector.
    lag: [Spring; 3],
}

/// One arm's springs.
#[derive(Clone, Debug, Default)]
struct ArmState {
    params: [Spring; 8],
    engage: Spring,
    /// The hand shape, and the first stage it chases through.
    hand: Hand,
    hand_goal: Hand,
}

/// The body that moves on its own; see the [module](self).
pub struct LivingBody {
    bus: BusAnimator,
    style: BodyStyle,
    tuning: Tuning,
    seed: u64,
    rng: Rng,
    sway_phase: [f32; 9],
    cues: Cues,
    tracked: BoneMask,
    yields: [Spring; 5],
    hand_rest: HandRest,
    rest_hand: Hand,
    library: &'static GestureLibrary,
    gestures: GestureScheduler,
    torso: Torso,
    arms: [ArmState; 2],
    reaction: Reaction,
    /// Seconds since the first step.
    time: f64,
}

impl std::fmt::Debug for LivingBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LivingBody")
            .field("style", &self.style)
            .field("seed", &self.seed)
            .field("phase", &self.gestures.phase())
            .finish_non_exhaustive()
    }
}

/// The built-in gesture library (or none, were it broken: the crate's tests rule that out) and the talk-rest hand.
fn libraries() -> (&'static GestureLibrary, Hand) {
    static EMPTY: GestureLibrary = GestureLibrary::EMPTY;
    let library = GestureLibrary::builtin().unwrap_or(&EMPTY);
    let rest = HandLibrary::builtin().ok().and_then(|h| h.get("rest")).map_or(Hand::NONE, |s| Hand::of(s, 0.85));
    (library, rest)
}

impl LivingBody {
    /// A living body with `style`, its random choices drawn from `seed`.
    pub fn new(style: BodyStyle, seed: u64) -> Self {
        let mut rng = Rng::new(seed ^ 0x6c69_7669_6e67_626f);
        let mut sway_rng = rng.fork(1);
        let sway_phase = std::array::from_fn(|_| sway_rng.next_f32() * TAU);
        let gestures = GestureScheduler::new(rng.fork(2));
        let (library, rest_hand) = libraries();
        let style = style.clamped();
        let mut body = Self {
            bus: BusAnimator::default(),
            style,
            tuning: Tuning::of(&style),
            seed,
            rng,
            sway_phase,
            cues: Cues::default(),
            tracked: BoneMask::EMPTY,
            yields: Default::default(),
            hand_rest: HandRest::nominal(),
            rest_hand,
            library,
            gestures,
            torso: Torso::default(),
            arms: Default::default(),
            reaction: Reaction::default(),
            time: 0.0,
        };
        body.torso.next_shift_s = body.tuning.shift_gap_s.sample(body.rng.next_f32());
        body
    }

    /// The same body for a rig whose thumbs rest as `rest` (`HandRest::of(skeleton)`), so its hand shapes fit.
    #[must_use]
    pub fn with_hand_rest(mut self, rest: HandRest) -> Self {
        self.hand_rest = rest;
        self
    }

    /// The physical reaction playing, if any ([`Animator::react`]).
    pub fn reaction(&self) -> &Reaction {
        &self.reaction
    }

    /// The gesture scheduler's phase.
    pub fn phase(&self) -> Phase {
        self.gestures.phase()
    }

    /// The gesture scheduler.
    pub fn gestures(&self) -> &GestureScheduler {
        &self.gestures
    }

    /// What the gesture scheduler did on the last step.
    pub fn stroke_log(&self) -> StrokeLog {
        self.gestures.log()
    }

    /// The gesture playing, by id.
    pub fn playing(&self) -> Option<&str> {
        self.gestures.playing().and_then(|i| self.library.at(i)).map(|g| g.id.as_str())
    }

    /// The tuning in effect.
    pub fn tuning(&self) -> &Tuning {
        &self.tuning
    }

    fn off(&self) -> bool {
        self.style.amount <= 0.0
    }

    /// The torso: follow, sway, weight shift, breath, emotion, laugh and the accents, as additive deltas.
    #[allow(clippy::too_many_lines, clippy::many_single_char_names)] // one pass over the torso's layers, in order
    fn torso(&mut self, dt: f32, bus: &Bus, gesture: &Output, out: &mut HumanoidPose) {
        let v = &bus.values;
        let f = |c: Channel| if v[c].is_finite() { v[c] } else { 0.0 };
        let t = &self.tuning;
        let r = f32::to_radians;
        let torso = &mut self.torso;

        // Follow: a share of the head's turn, lagging behind it.
        let head = [f(Channel::HEAD_YAW), f(Channel::HEAD_PITCH), f(Channel::HEAD_ROLL)];
        let follow: [f32; 3] =
            std::array::from_fn(|i| torso.follow[i].step(head[i] * t.follow[i], Feel::new(1.1, 0.85), dt));

        // Weight shifts: a new side every few seconds, usually the other one.
        torso.next_shift_s -= dt;
        if torso.next_shift_s <= 0.0 {
            let flip = self.rng.next_f32() < 0.75;
            let side = match (torso.shift_target == 0.0, flip) {
                (true, _) => {
                    if self.rng.next_f32() < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                (false, true) => -torso.shift_target.signum(),
                (false, false) => torso.shift_target.signum(),
            };
            torso.shift_target = side * self.rng.range_f32(0.45, 1.0);
            torso.next_shift_s = t.shift_gap_s.sample(self.rng.next_f32());
        }
        let shift = torso.shift.step(torso.shift_target, Feel::new(0.32, 0.92), dt) * r(t.shift_deg);

        // Sway: three slow sines per axis, seeded phases.
        #[allow(clippy::cast_possible_truncation)] // a phase in radians
        let sway: [f32; 3] = std::array::from_fn(|axis| {
            (0..3)
                .map(|k| {
                    let phase = (self.time * f64::from(SWAY_HZ[k] * TAU)) as f32 + self.sway_phase[axis * 3 + k];
                    phase.sin() / 3.0
                })
                .sum::<f32>()
                * r(t.sway_deg)
        });

        // Emotion: posture from the face.
        let smile = f32::midpoint(f(Channel::MOUTH_SMILE_LEFT), f(Channel::MOUTH_SMILE_RIGHT));
        let frown = f32::midpoint(f(Channel::MOUTH_FROWN_LEFT), f(Channel::MOUTH_FROWN_RIGHT));
        let inner = f(Channel::BROW_INNER_UP);
        let down = f32::midpoint(f(Channel::BROW_DOWN_LEFT), f(Channel::BROW_DOWN_RIGHT));
        let wide = f32::midpoint(f(Channel::EYE_WIDE_LEFT), f(Channel::EYE_WIDE_RIGHT));
        let jaw = f(Channel::JAW_OPEN);
        let joy = smile.clamp(0.0, 1.0);
        let sad = (frown * 0.8 + inner * 0.45 - smile).clamp(0.0, 1.0);
        let anger = ((down - smile * 0.5) * (1.0 - inner * 0.5)).clamp(0.0, 1.0);
        let surprise = (wide * 1.6 * (0.4 + inner)).clamp(0.0, 1.0);
        let e = t.emotion;
        let [slump, posture_in] = t.posture_deg;
        let targets = [
            slump + e * (sad * 7.0 - joy * 2.5 + anger * 2.0),
            e * (-joy * 2.0 - surprise * 3.0 + anger * 1.5),
            e * (anger * 5.0 + surprise * 6.0 - sad * 3.0 + joy * 1.0),
            posture_in + e * (sad * 5.0 + anger * 2.5 - joy * 3.0),
        ];
        let feels = [Feel::new(0.8, 0.95), Feel::new(1.0, 0.9), Feel::new(2.4, 0.75), Feel::new(0.9, 0.95)];
        let posture: [f32; 4] = std::array::from_fn(|i| torso.posture[i].step(targets[i], feels[i], dt));

        // Laugh: from the voice's laugh cue, or a wide open smile with a loud voice.
        let laughing = self.cues.laugh.max(joy * jaw.clamp(0.0, 1.0) * 2.0 * self.cues.loudness.clamp(0.0, 1.0));
        let laugh = torso.laugh.step(laughing.clamp(0.0, 1.0) * e, Feel::new(2.5, 1.0), dt).max(0.0);
        torso.laugh_phase = (torso.laugh_phase + dt * LAUGH_HZ * TAU) % TAU;
        let shake = laugh * torso.laugh_phase.sin();

        // A small push of the chest on each stress, with the voice's energy.
        if self.cues.onset && self.style.amount > 0.0 {
            let push = 18.0 * (0.4 + self.cues.loudness.clamp(0.0, 1.0)) * self.style.gestures.clamp(0.3, 1.0);
            torso.accent.kick(push);
        }
        let accent = torso.accent.step(0.0, Feel::new(3.2, 0.55), dt);
        let chest: [f32; 3] = std::array::from_fn(|i| torso.chest[i].step(gesture.chest[i], Feel::new(4.0, 0.8), dt));

        // Breath: the life layer's breath channel, 0.5 at rest.
        let breath = f(Channel::BREATH) - 0.5;

        // Compose the deltas.
        let hips_roll = shift;
        let hips = Quat::from_rotation_y(shift * 0.3) * Quat::from_rotation_z(hips_roll);
        add(out, B::Hips, hips);
        // The legs keep their direction in the world, so the feet stay put while the pelvis tilts.
        add(out, B::LeftUpperLeg, hips.inverse());
        add(out, B::RightUpperLeg, hips.inverse());
        let pitch = follow[1] + r(posture[0] + accent + chest[0]) + sway[0];
        let yaw = follow[0] + r(chest[1]) + sway[1];
        let roll = follow[2] + r(chest[2] + shake * 0.4) + sway[2];
        // The spine counters the pelvis (an S-curve), the chest settles the shoulders level.
        let counter = [-1.4 * hips_roll, 0.4 * hips_roll, 0.0];
        let chest_pitch = r(posture[1]) - r(t.breath_deg[0]) * breath * 2.0 + r(shake.abs() * 1.5);
        for (i, bone) in TORSO.into_iter().enumerate() {
            let portion = TORSO_SHARE[i];
            let extra_pitch = if bone == B::Chest { chest_pitch } else { 0.0 };
            let delta = Quat::from_rotation_y(yaw * portion)
                * Quat::from_rotation_x(pitch * portion + extra_pitch)
                * Quat::from_rotation_z(roll * portion + counter[i]);
            add(out, bone, delta);
        }
        // Shoulders: breath, emotion, the laugh's shake. Positive Z lifts the left shoulder; the right mirrors.
        let shrug = r(posture[2] + t.breath_deg[1] * breath * 2.0 + shake * 3.0);
        let forward = r(posture[3]);
        add(out, B::LeftShoulder, Quat::from_rotation_z(shrug) * Quat::from_rotation_y(-forward));
        add(out, B::RightShoulder, Quat::from_rotation_z(-shrug) * Quat::from_rotation_y(forward));
    }

    /// The arms: the gesture targets through their springs, blended over the body's own arms.
    fn arms(&mut self, dt: f32, gesture: &Output, incoming: &HumanoidPose, out: &mut HumanoidPose) -> [Hand; 2] {
        let mut hands = [Hand::NONE; 2];
        for side in [Side::Left, Side::Right] {
            let target = gesture.arms[side.index()];
            let state = &mut self.arms[side.index()];
            let goal = target.pose.to_array();
            let params: [f32; 8] = std::array::from_fn(|i| state.params[i].step(goal[i], target.feel, dt));
            let engage = state.engage.step(target.engage, Feel::new(1.3, 1.0), dt).clamp(0.0, 1.0);
            // Two first-order stages: the fingers start and stop smoothly instead of snapping toward a new shape.
            let k = (dt * HAND_RATE).min(1.0);
            state.hand_goal = Hand::blend(&state.hand_goal, &target.hand, k);
            state.hand = Hand::blend(&state.hand, &state.hand_goal, k);
            hands[side.index()] = Hand::blend(&Hand::NONE, &state.hand, engage);
            if engage <= 1e-4 {
                continue;
            }
            let pose = ArmPose::from_array(params).clamped();
            for (bone, q) in side.bones().into_iter().zip(pose.rotations(side)) {
                let from = incoming.get(bone);
                out.set(bone, from.slerp(q, engage).normalize());
            }
        }
        hands
    }

    /// The arms lag behind the torso's turn and swing back: the follow-through.
    fn follow_through(&mut self, dt: f32, out: &mut HumanoidPose) {
        let torso = TORSO.iter().fold(Quat::IDENTITY, |q, b| q * out.get(*b));
        // One hemisphere, so the rotation vector never jumps by a full turn.
        let torso = if torso.w < 0.0 { -torso } else { torso };
        let now = torso.to_scaled_axis();
        let lagged =
            Vec3::from_array(std::array::from_fn(|i| self.torso.lag[i].step(now[i], Feel::new(1.25, 0.32), dt)));
        let lag = torso.inverse() * Quat::from_scaled_axis(lagged);
        for side in [Side::Left, Side::Right] {
            let engaged = self.arms[side.index()].engage.pos().clamp(0.0, 1.0);
            let k = self.tuning.arms * (1.0 - 0.6 * engaged);
            let bone = side.bones()[1];
            add(out, bone, Quat::IDENTITY.slerp(lag, k.clamp(0.0, 1.0)));
        }
    }

    /// Hands bones another source drives back to it, smoothly: back to `driven`, the pose as that source left it, before
    /// the bus animator's lean, breath and turn (a clip or a tracked body already carries its own).
    fn yield_tracked(&mut self, dt: f32, driven: &HumanoidPose, out: &mut HumanoidPose) {
        for (i, group) in Group::ALL.into_iter().enumerate() {
            let held = B::ALL.into_iter().any(|b| Group::of(b) == Some(group) && self.tracked.contains(b));
            let y = self.yields[i].step(if held { 1.0 } else { 0.0 }, YIELD_FEEL, dt).clamp(0.0, 1.0);
            if y <= 1e-4 {
                continue;
            }
            for bone in B::ALL.into_iter().filter(|b| Group::of(*b) == Some(group)) {
                let q = if y >= 0.999 { driven.get(bone) } else { out.get(bone).slerp(driven.get(bone), y) };
                out.set(bone, q);
            }
            if group == Group::Torso && y >= 0.999 {
                out.hips_offset = driven.hips_offset;
            }
        }
    }
}

impl LivingBody {
    /// The physical reaction, over everything else (tracking included): a blow moves what it hits.
    fn react_step(&mut self, dt: f32, out: &mut HumanoidPose) {
        if self.reaction.active() {
            self.reaction.step(dt);
            self.reaction.apply(out);
        }
    }
}

/// `delta` added onto `bone` in its parent's T space (`PoseMode::Add`).
fn add(pose: &mut HumanoidPose, bone: B, delta: Quat) {
    pose.set(bone, (delta * pose.get(bone)).normalize());
}

/// The world rotation of `bone`'s parent chain up to and including `bone`, from the hips.
fn chain(pose: &HumanoidPose, last: B) -> Quat {
    let mut bones = [B::Hips; 8];
    let mut n = 0;
    let mut at = Some(last);
    while let Some(b) = at {
        if n < bones.len() {
            bones[n] = b;
            n += 1;
        }
        at = b.parent();
    }
    bones[..n].iter().rev().fold(Quat::IDENTITY, |q, b| q * pose.get(*b))
}

impl Animator for LivingBody {
    fn animate(&mut self, tick: Tick, bus: &Bus, cues: &[Cue], out: &mut HumanoidPose) {
        let driven = *out;
        self.bus.animate(tick, bus, cues, out);
        let dt = if tick.dt.is_finite() { tick.dt.clamp(0.0, 0.1) } else { 0.0 };
        if self.off() {
            self.react_step(dt, out);
            return;
        }
        self.time += f64::from(dt);
        let incoming = *out;
        let gesture = *self.gestures.step(
            dt,
            tick.time.0,
            &self.cues,
            &self.tuning,
            self.style.handedness,
            self.library,
            &self.rest_hand,
        );
        let before = chain(out, B::UpperChest);
        let hands = self.arms(dt, &gesture, &incoming, out);
        self.torso(dt, bus, &gesture, out);
        self.follow_through(dt, out);
        let posture = Posture { body: *out, left: hands[0], right: hands[1], reach: Reaches::NONE };
        *out = posture.to_humanoid(&self.hand_rest);
        self.yield_tracked(dt, &driven, out);
        // The head stays where tracking put it: the neck takes back whatever the torso turned.
        let after = chain(out, B::UpperChest);
        if !self.tracked.contains(B::Neck) {
            let neck = out.get(B::Neck);
            out.set(B::Neck, (after.inverse() * before * neck).normalize());
        }
        self.cues.onset = false;
        self.react_step(dt, out);
    }

    fn reset(&mut self) {
        let style = self.style;
        let hand_rest = self.hand_rest;
        let tracked = self.tracked;
        *self = Self::new(style, self.seed).with_hand_rest(hand_rest);
        self.tracked = tracked;
    }

    fn hear(&mut self, cues: &Cues) {
        // An onset is one-shot: keep it until a step has consumed it.
        let onset = self.cues.onset || cues.onset;
        self.cues = *cues;
        self.cues.onset = onset;
    }

    fn set_tracked(&mut self, bones: BoneMask) {
        self.tracked = bones;
    }

    fn set_style(&mut self, style: &BodyStyle) -> bool {
        self.style = style.clamped();
        self.tuning = Tuning::of(&self.style);
        true
    }

    fn style(&self) -> Option<BodyStyle> {
        Some(self.style)
    }

    fn react(&mut self, impact: &Impact) -> bool {
        self.reaction.hit(impact);
        true
    }
}

impl Default for LivingBody {
    fn default() -> Self {
        Self::new(BodyStyle::default(), 7)
    }
}
