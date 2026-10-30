//! The talk loop: when to gesture, which phrase, and when each stroke strikes.
//!
//! Gesture research is consistent on one point: a gesture's stroke lands on (or just before) the stressed syllable it
//! goes with, and a stroke that lands late reads as a puppet. The voice tells us about a stress only once it has
//! started, so a purely reactive scheduler is always late by its stroke's length. This one does both:
//!
//! - **React.** It prepares a phrase and holds the pre-stroke pose; when a stressed syllable starts it strikes with a
//!   short stroke ([`REACTIVE_STROKE_S`]), so the hit lands within about a tenth of a second of the onset.
//! - **Predict.** Stresses in running speech come in a rhythm. It keeps a running estimate of the interval between
//!   them and of its spread; while the rhythm is steady it starts the stroke *before* the next stress is due, timed so
//!   the hit lands on it. A stress that comes early still triggers a reactive stroke.
//!
//! Between phrases the hands wait at the talk-rest pose. A pause longer than [`PHRASE_END_S`] ends the phrase; one
//! longer than [`REST_AFTER_S`] lets the arms go back to the body's own rest. When the character is not talking, an
//! idle fidget comes every few seconds. Everything random comes from the seeded [`Rng`], so a take replays exactly.
//!
//! Hot path: [`GestureScheduler::step`] allocates nothing; phrases are indices into the library.

use omuzaki_anim_api::{Cues, Handedness};
use omuzaki_core::rng::Rng;

use super::super::arm::{ArmPose, Side};
use super::super::spring::Feel;
use super::super::tune::Tuning;
use super::library::{Gesture, GestureKind, GestureLibrary, Hands};
use crate::poses::Hand;

/// Loudness above which the voice counts as speaking.
pub const VOICED: f32 = 0.05;
/// Seconds of quiet that end a phrase: no new one starts, a held one retracts to the talk-rest pose.
pub const PHRASE_END_S: f32 = 0.6;
/// Seconds of quiet after which the arms leave the talk-rest pose for the body's own rest.
pub const REST_AFTER_S: f32 = 2.2;
/// The reactive stroke's length, seconds: from a stress onset to the hit.
pub const REACTIVE_STROKE_S: f32 = 0.05;
/// The predicted stroke's length, seconds.
pub const PREDICTED_STROKE_S: f32 = 0.16;
/// How far a predicted stroke goes before it waits for its stress: the tension just before a beat.
const APEX_U: f32 = 0.8;
/// Longest a predicted stroke waits there for its stress, seconds.
const MAX_APEX_WAIT_S: f32 = 0.12;
/// The quickest finish of a stroke once its stress comes, seconds.
const FINISH_S: f32 = 0.017;
/// The fastest a stroke moves any of the arm's numbers, degrees a second: a quick human beat, not a snap.
const MAX_STROKE_DEG_S: f32 = 420.0;
/// Seconds the arm springs trail the stroke path at its end: the prediction starts early by this much too.
const SPRING_LAG_S: f32 = 0.018;
/// Seconds a preparation takes at speed 1.
const PREP_S: f32 = 0.36;
/// Seconds the stroke pose is held after the hit, before a recoil or a retraction.
const POST_HOLD_S: f32 = 0.13;
/// Seconds a recoil (back toward the preparation, ready for the next stroke) takes.
const RECOIL_S: f32 = 0.2;
/// Longest wait at the pre-stroke pose for a stress before giving up.
const MAX_HOLD_S: f32 = 1.1;
/// Seconds a retraction takes before the next phrase may be planned.
const RETRACT_S: f32 = 0.45;
/// How far a recoil goes back from the stroke toward the preparation.
const RECOIL_SHARE: f32 = 0.6;
/// Stress intervals outside this window (seconds) are not rhythm: a pause, or two detections of one syllable.
const IOI_WINDOW: (f32, f32) = (0.14, 1.2);
/// How quickly the rhythm estimate follows new intervals.
const IOI_ALPHA: f32 = 0.4;
/// The rhythm counts as steady when its mean deviation is below this share of the interval.
const STEADY: f32 = 0.3;
/// Stresses needed before the rhythm is trusted.
const MIN_ONSETS: u32 = 3;
/// Odds of each class when a phrase is picked while talking (fidgets are picked apart).
const KIND_WEIGHTS: [(GestureKind, f32); 5] = [
    (GestureKind::Beat, 0.5),
    (GestureKind::Metaphoric, 0.22),
    (GestureKind::Deictic, 0.12),
    (GestureKind::Iconic, 0.08),
    (GestureKind::Emblem, 0.08),
];

/// Where the scheduler is in a phrase.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Not talking: the arms at the body's own rest.
    Rest,
    /// Talking between phrases: the hands wait at the talk-rest pose.
    TalkRest,
    /// Moving to the pre-stroke pose.
    Prep,
    /// Holding the pre-stroke pose for a stress.
    Hold,
    /// Striking.
    Stroke,
    /// Holding the hit, then easing back for the next stroke.
    Recoil,
    /// Going back to the talk-rest pose.
    Retract,
    /// An idle adjustment while not talking.
    Fidget,
}

/// What one arm should do this step.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ArmTarget {
    /// The pose to chase.
    pub pose: ArmPose,
    /// The hand shape to take.
    pub hand: Hand,
    /// How to chase it.
    pub feel: Feel,
    /// How much the pose replaces the body's own arm, `0..=1` (it is chased with its own spring).
    pub engage: f32,
}

/// The scheduler's output for one step.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Output {
    /// Left and right.
    pub arms: [ArmTarget; 2],
    /// Chest accent: pitch, yaw (toward the character's left) and roll, degrees.
    pub chest: [f32; 3],
    /// Where the scheduler is.
    pub phase: Phase,
}

/// What the measure needs from a step: did a stroke land, was it predicted, and did a stress start.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StrokeLog {
    /// A stroke hit on this step.
    pub landed: bool,
    /// That stroke was started from the rhythm, before its stress.
    pub predicted: bool,
    /// A phrase started on this step.
    pub phrase: bool,
}

/// The phrase being played.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Playing {
    /// Index into the library.
    gesture: usize,
    /// The side of the gesturing hand.
    lead: Side,
    /// The pre-stroke poses (lead, other), scaled to the character's size.
    prep: [ArmPose; 2],
    /// The stroke poses, scaled.
    stroke: [ArmPose; 2],
    /// Where the current stroke starts from (the preparation, or the recoil).
    from: [ArmPose; 2],
    /// Where the current stroke lands: the stroke pose, or part of the way there for a late, reactive one.
    to: [ArmPose; 2],
    /// Strokes still to play after the current one.
    strokes_left: u8,
    /// The current stroke's progress `0..=1`, and how fast it goes, per second.
    u: f32,
    rate: f32,
    /// Whether the current stroke's stress has come (a reactive stroke's always has).
    heard: bool,
    /// Seconds a predicted stroke has waited near its hit for the stress.
    wait_s: f32,
    /// The shortest the arm may take for the whole stroke, seconds.
    need: f32,
    /// Whether the current stroke was started from the rhythm.
    predicted: bool,
}

/// The co-speech gesture scheduler for one character.
#[derive(Clone, Debug)]
pub struct GestureScheduler {
    rng: Rng,
    phase: Phase,
    /// Seconds in the current phase.
    phase_s: f32,
    playing: Option<Playing>,
    last_gesture: Option<usize>,
    /// Seconds of quiet so far (0 while speaking).
    quiet_s: f32,
    /// Seconds of talk until the next phrase may start.
    next_phrase_s: f32,
    /// Seconds of idle until the next fidget.
    next_fidget_s: f32,
    /// Pipeline time of the last stress onset.
    last_onset: Option<f64>,
    /// Running estimate of the interval between stresses and its mean deviation, seconds.
    ioi: f32,
    ioi_dev: f32,
    /// Stresses since the last pause, and in all: a speaker's tempo outlasts a pause, so once it is learned one
    /// stress after a pause is enough to place the next.
    onsets: u32,
    learned: u32,
    /// Smoothed loudness while speaking: the phrase's energy.
    energy: f32,
    log: StrokeLog,
    out: Output,
}

/// The rest hand: every bone as the body says.
const NO_HAND: Hand = Hand::NONE;

impl GestureScheduler {
    /// A scheduler drawing its choices from `rng`.
    pub fn new(rng: Rng) -> Self {
        let rest = ArmTarget { pose: ArmPose::HANGING, hand: NO_HAND, feel: Feel::new(1.2, 1.0), engage: 0.0 };
        Self {
            rng,
            phase: Phase::Rest,
            phase_s: 0.0,
            playing: None,
            last_gesture: None,
            quiet_s: REST_AFTER_S,
            next_phrase_s: 0.4,
            next_fidget_s: 6.0,
            last_onset: None,
            ioi: 0.4,
            ioi_dev: 0.4,
            onsets: 0,
            learned: 0,
            energy: 0.0,
            log: StrokeLog::default(),
            out: Output { arms: [rest; 2], chest: [0.0; 3], phase: Phase::Rest },
        }
    }

    /// Back to the state after construction, with the same seed stream continuing.
    pub fn reset(&mut self) {
        let rng = self.rng.clone();
        *self = Self::new(rng);
    }

    /// Where it is.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// What happened on the last step.
    pub fn log(&self) -> StrokeLog {
        self.log
    }

    /// The phrase being played, by its index in the library.
    pub fn playing(&self) -> Option<usize> {
        self.playing.map(|p| p.gesture)
    }

    /// The rhythm the prediction runs on: the mean interval between stresses and whether it is steady enough to trust.
    pub fn rhythm(&self) -> (f32, bool) {
        (self.ioi, self.steady())
    }

    fn steady(&self) -> bool {
        let anchored = if self.learned >= MIN_ONSETS { self.onsets >= 1 } else { self.onsets >= MIN_ONSETS };
        anchored && self.ioi_dev < STEADY * self.ioi
    }

    /// Runs one step of `dt` seconds at pipeline time `now` with this step's voice `cues`. `hand_rest` is the hand
    /// shape the talk-rest pose holds. Hot path.
    #[allow(clippy::too_many_arguments)] // the step's inputs and the character's settings, all borrowed
    pub fn step(
        &mut self,
        dt: f32,
        now: f64,
        cues: &Cues,
        tuning: &Tuning,
        handedness: Handedness,
        library: &GestureLibrary,
        hand_rest: &Hand,
    ) -> &Output {
        let dt = if dt.is_finite() { dt.clamp(0.0, 0.1) } else { 0.0 };
        self.log = StrokeLog::default();
        self.phase_s += dt;
        self.hear(dt, now, cues);
        let talking = self.quiet_s < PHRASE_END_S;
        let gestures_on = tuning.gesture_size > 0.0;
        let speed = tuning.speed.max(0.2);
        match self.phase {
            Phase::Rest | Phase::TalkRest => {
                if talking {
                    self.phase = Phase::TalkRest;
                    self.next_phrase_s -= dt;
                    // With a rhythm to go by, a phrase starts on a stress, so its first stroke can be timed for the
                    // next one; without, at once.
                    if gestures_on && self.next_phrase_s <= 0.0 && (cues.onset || !self.steady()) {
                        self.start_phrase(tuning, handedness, library, false);
                    }
                } else if self.quiet_s >= REST_AFTER_S {
                    self.phase = Phase::Rest;
                    self.next_fidget_s -= dt;
                    if self.next_fidget_s <= 0.0 {
                        self.next_fidget_s = tuning.fidget_gap_s.sample(self.rng.next_f32());
                        if self.next_fidget_s.is_finite() {
                            self.start_phrase(tuning, handedness, library, true);
                        }
                    }
                }
            }
            Phase::Prep | Phase::Hold | Phase::Recoil => {
                let more = self.playing.is_some_and(|p| p.strokes_left > 0);
                let ready = more
                    && match self.phase {
                        Phase::Prep => self.phase_s > 0.12,
                        Phase::Recoil => self.phase_s >= POST_HOLD_S,
                        _ => true,
                    };
                let need = self.stroke_need(speed);
                let predicted = if ready && !cues.onset { self.predicted_lead(now, need) } else { None };
                if ready && (cues.onset || predicted.is_some()) {
                    self.from_here(speed);
                    self.strike(predicted.unwrap_or(REACTIVE_STROKE_S).max(need), predicted.is_some(), need);
                } else {
                    match self.phase {
                        Phase::Prep if !talking => self.enter(Phase::Retract),
                        Phase::Prep if self.phase_s >= PREP_S / speed => self.enter(Phase::Hold),
                        Phase::Hold if !talking || self.phase_s > MAX_HOLD_S => self.enter(Phase::Retract),
                        Phase::Recoil if !(more && talking) && self.phase_s >= POST_HOLD_S => {
                            self.enter(Phase::Retract);
                        }
                        Phase::Recoil if self.phase_s >= POST_HOLD_S + RECOIL_S / speed => {
                            self.from_here(speed);
                            self.enter(Phase::Hold);
                        }
                        _ => {}
                    }
                }
            }
            Phase::Stroke => {
                let mut done = self.playing.is_none();
                let mut abandon = false;
                if let Some(p) = self.playing.as_mut() {
                    if cues.onset && !p.heard {
                        // The stress came: finish the stroke now, as fast as the arm may move.
                        p.heard = true;
                        let finish = ((1.0 - p.u) * p.need).max(FINISH_S);
                        p.rate = p.rate.max((1.0 - p.u) / finish);
                    }
                    let cap = if p.heard { 1.0 } else { APEX_U };
                    p.u = (p.u + p.rate * dt).min(cap);
                    if !p.heard && p.u >= APEX_U {
                        p.wait_s += dt;
                    }
                    // No stress came (the rhythm was wrong, or the sentence ended): let the stroke go unstruck.
                    abandon = !p.heard && (p.wait_s > MAX_APEX_WAIT_S || self.quiet_s > 0.15);
                    done = p.u >= 1.0;
                }
                if abandon {
                    self.enter(Phase::Retract);
                } else if done {
                    self.log.landed = true;
                    self.log.predicted = self.playing.is_some_and(|p| p.predicted);
                    self.enter(Phase::Recoil);
                }
            }
            Phase::Retract => {
                if self.phase_s >= RETRACT_S / speed {
                    self.playing = None;
                    self.next_phrase_s = tuning.gesture_gap_s.sample(self.rng.next_f32());
                    self.enter(if talking { Phase::TalkRest } else { Phase::Rest });
                }
            }
            Phase::Fidget => {
                if talking {
                    self.enter(Phase::Retract);
                } else if self.phase_s >= 1.9 / speed {
                    self.playing = None;
                    self.enter(Phase::Rest);
                }
            }
        }
        self.write(tuning, hand_rest, speed, library);
        &self.out
    }

    /// Reads the voice: quiet time, the phrase's energy, and the rhythm of stresses.
    fn hear(&mut self, dt: f32, now: f64, cues: &Cues) {
        let loud = if cues.loudness.is_finite() { cues.loudness.clamp(0.0, 1.0) } else { 0.0 };
        if loud > VOICED || cues.onset {
            self.quiet_s = 0.0;
            self.energy += (loud - self.energy) * (dt * 3.0).min(1.0);
        } else {
            self.quiet_s += dt;
        }
        if cues.onset {
            if let Some(last) = self.last_onset {
                #[allow(clippy::cast_possible_truncation)] // an interval of seconds
                let interval = (now - last) as f32;
                if (IOI_WINDOW.0..=IOI_WINDOW.1).contains(&interval) {
                    self.ioi_dev += ((interval - self.ioi).abs() - self.ioi_dev) * IOI_ALPHA;
                    self.ioi += (interval - self.ioi) * IOI_ALPHA;
                    self.onsets += 1;
                    self.learned += 1;
                } else if interval > IOI_WINDOW.1 {
                    // A pause: the stress after it anchors the rhythm again.
                    self.onsets = 1;
                }
            } else {
                self.onsets = 1;
            }
            self.last_onset = Some(now);
        }
    }

    /// The shortest stroke from where the next one starts that keeps the arm under [`MAX_STROKE_DEG_S`]: a big
    /// gesture needs longer than a beat.
    fn stroke_need(&self, speed: f32) -> f32 {
        let Some(p) = self.playing else { return REACTIVE_STROKE_S };
        let travel = (0..2)
            .flat_map(|i| p.from[i].to_array().into_iter().zip(p.stroke[i].to_array()))
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        // The stroke's ease peaks at 4/3 of its mean speed.
        (travel * 4.0 / 3.0 / (MAX_STROKE_DEG_S * speed.max(0.2))).max(REACTIVE_STROKE_S)
    }

    /// When the rhythm is steady and the next stress is due within one stroke of `need` seconds (or the predicted
    /// stroke's length, if longer), the stroke length that lands on it.
    fn predicted_lead(&self, now: f64, need: f32) -> Option<f32> {
        if !self.steady() || self.quiet_s > 0.2 {
            return None;
        }
        let last = self.last_onset?;
        #[allow(clippy::cast_possible_truncation)] // seconds
        let due = (last + f64::from(self.ioi) - now) as f32 - SPRING_LAG_S;
        (0.05..=PREDICTED_STROKE_S.max(need)).contains(&due).then_some(due)
    }

    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.phase_s = 0.0;
    }

    /// Sets where the next stroke starts: where the phase's path has the arm now (the arm springs smooth the rest).
    fn from_here(&mut self, speed: f32) {
        let recoil = self.phase == Phase::Recoil;
        let back = ((self.phase_s - POST_HOLD_S) * speed / RECOIL_S).clamp(0.0, 1.0) * RECOIL_SHARE;
        let prep = self.phase == Phase::Prep;
        if let Some(p) = self.playing.as_mut() {
            if prep {
                p.from = p.prep;
            } else if recoil {
                p.from = [0, 1].map(|i| p.to[i].lerp(&p.prep[i], back));
            }
        }
    }

    /// Starts a stroke `stroke_s` long; `need` is the shortest the arm may take for all of it. A predicted stroke
    /// stops short of the hit ([`APEX_U`]) and finishes when its stress comes.
    fn strike(&mut self, stroke_s: f32, predicted: bool, need: f32) {
        if let Some(p) = self.playing.as_mut() {
            // A reactive stroke is already late: a big one is cut down to a beat that still lands in time.
            let share = if predicted { 1.0 } else { (REACTIVE_STROKE_S / need.max(1e-3)).min(1.0) };
            p.to = [0, 1].map(|i| p.from[i].lerp(&p.stroke[i], share));
            let (stroke_s, need) = if predicted { (stroke_s, need) } else { (REACTIVE_STROKE_S, REACTIVE_STROKE_S) };
            p.u = 0.0;
            p.rate = 1.0 / stroke_s.max(1e-3);
            p.heard = !predicted;
            p.wait_s = 0.0;
            p.need = need;
            p.predicted = predicted;
            p.strokes_left = p.strokes_left.saturating_sub(1);
            self.enter(Phase::Stroke);
        }
    }

    /// Picks a phrase (a fidget when `fidget`) and starts preparing it.
    fn start_phrase(&mut self, tuning: &Tuning, handedness: Handedness, library: &GestureLibrary, fidget: bool) {
        let Some(index) = self.pick(library, fidget) else { return };
        let Some(gesture) = library.at(index) else { return };
        let lead = match gesture.hands {
            Hands::Both => Side::Right,
            Hands::One => {
                let leading = if handedness == Handedness::Right { Side::Right } else { Side::Left };
                if self.rng.next_f32() < tuning.lead_odds { leading } else { leading.other() }
            }
        };
        let [lo, hi] = gesture.strokes;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // at most 8
        let strokes = lo + ((f32::from(hi - lo + 1) * self.rng.next_f32()) as u8).min(hi - lo);
        let (prep, stroke) = scaled(gesture, tuning, self.energy);
        self.playing = Some(Playing {
            gesture: index,
            lead,
            prep,
            stroke,
            from: prep,
            to: stroke,
            strokes_left: strokes,
            u: 0.0,
            rate: 1.0 / REACTIVE_STROKE_S,
            heard: true,
            wait_s: 0.0,
            need: REACTIVE_STROKE_S,
            predicted: false,
        });
        self.last_gesture = Some(index);
        self.log.phrase = true;
        self.enter(if fidget { Phase::Fidget } else { Phase::Prep });
    }

    /// A phrase index: a fidget, or a talking gesture of a class drawn by [`KIND_WEIGHTS`], never the last one twice.
    fn pick(&mut self, library: &GestureLibrary, fidget: bool) -> Option<usize> {
        let wanted = if fidget {
            GestureKind::Fidget
        } else if !self.steady() && self.rng.next_f32() < 0.75 {
            // Without a rhythm to strike ahead of, a stroke can only react: a short beat lands soonest.
            GestureKind::Beat
        } else {
            let mut r = self.rng.next_f32() * KIND_WEIGHTS.iter().map(|(_, w)| w).sum::<f32>();
            let mut kind = GestureKind::Beat;
            for (k, w) in KIND_WEIGHTS {
                if r < w {
                    kind = k;
                    break;
                }
                r -= w;
            }
            kind
        };
        let fits = |i: usize, g: &Gesture| g.kind == wanted && Some(i) != self.last_gesture;
        let count = library.iter().enumerate().filter(|(i, g)| fits(*i, g)).count();
        let count = if count == 0 {
            // Only the last one is of this class (or none is): take any of the class.
            library.iter().filter(|g| g.kind == wanted).count()
        } else {
            count
        };
        if count == 0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let pick = ((self.rng.next_f32() * count as f32) as usize).min(count - 1);
        let strict = library.iter().enumerate().any(|(i, g)| fits(i, g));
        library
            .iter()
            .enumerate()
            .filter(|(i, g)| if strict { fits(*i, g) } else { g.kind == wanted })
            .nth(pick)
            .map(|(i, _)| i)
    }

    /// Writes this step's arm targets from the phase.
    fn write(&mut self, tuning: &Tuning, hand_rest: &Hand, speed: f32, library: &GestureLibrary) {
        let rest_arm =
            ArmTarget { pose: tuning.talk_rest, hand: *hand_rest, feel: Feel::new(1.6 * speed, 0.95), engage: 1.0 };
        let mut arms = [rest_arm; 2];
        let mut chest = [0.0; 3];
        let playing = self.playing.and_then(|p| library.at(p.gesture).map(|g| (p, g)));
        match (self.phase, playing) {
            (Phase::Rest, _) => {
                for arm in &mut arms {
                    arm.engage = 0.0;
                    arm.feel = Feel::new(0.9 * speed, 1.0);
                }
            }
            (Phase::TalkRest | Phase::Retract, _) | (_, None) => {}
            (Phase::Fidget, Some((p, gesture))) => {
                let t = self.phase_s * speed;
                let pose = if t < 0.55 {
                    p.prep
                } else if t < 1.15 {
                    p.stroke
                } else {
                    [ArmPose::HANGING; 2]
                };
                let feel = Feel::new(2.2 * speed, 0.9);
                // A fidget starts from the body's rest, and an arm it does not move stays there.
                arms = [ArmTarget { engage: 0.0, ..rest_arm }; 2];
                arms[p.lead.index()] = ArmTarget { pose: pose[0], hand: gesture.prep.lead_hand, feel, engage: 1.0 };
                if gesture.hands == Hands::Both {
                    arms[p.lead.other().index()] =
                        ArmTarget { pose: pose[1], hand: gesture.prep.other_hand, feel, engage: 1.0 };
                }
            }
            (phase, Some((p, gesture))) => {
                let (pose, key, feel) = match phase {
                    Phase::Prep => (p.prep, &gesture.prep, Feel::new(3.0 * speed, 0.85)),
                    Phase::Stroke => {
                        let u = p.u.clamp(0.0, 1.0);
                        // Accelerate into the hit: the stroke's speed peaks at its end, as a struck beat does.
                        let e = u * u * (2.0 - u);
                        let pose = [0, 1].map(|i| p.from[i].lerp(&p.to[i], e));
                        (pose, if u > 0.25 { &gesture.stroke } else { &gesture.prep }, Feel::new(11.0 * speed, 0.62))
                    }
                    Phase::Recoil => {
                        let back = ((self.phase_s - POST_HOLD_S) * speed / RECOIL_S).clamp(0.0, 1.0) * RECOIL_SHARE;
                        let pose = [0, 1].map(|i| p.to[i].lerp(&p.prep[i], back));
                        (pose, &gesture.stroke, Feel::new(5.0 * speed, 0.8))
                    }
                    // Hold (the other phases are matched above).
                    _ => (p.from, &gesture.prep, Feel::new(3.5 * speed, 0.85)),
                };
                arms[p.lead.index()] = ArmTarget { pose: pose[0], hand: key.lead_hand, feel, engage: 1.0 };
                if gesture.hands == Hands::Both {
                    arms[p.lead.other().index()] =
                        ArmTarget { pose: pose[1], hand: key.other_hand, feel, engage: 1.0 };
                }
                if matches!(phase, Phase::Stroke | Phase::Recoil) {
                    // Yaw and roll toward the lead side: positive yaw turns toward the character's left.
                    let side = if p.lead == Side::Left { 1.0 } else { -1.0 };
                    let k = tuning.gesture_size.min(1.3);
                    let [pitch, yaw, roll] = gesture.stroke.chest;
                    chest = [pitch * k, yaw * side * k, roll * side * k];
                }
            }
        }
        self.out = Output { arms, chest, phase: self.phase };
    }
}

/// The phrase's preparation and stroke poses for both arms, scaled to the character: the preparation comes part of
/// the way from the talk-rest pose for a small gesturer, the stroke travels `gesture_size` (more with energy) of the
/// authored way from the preparation.
fn scaled(gesture: &Gesture, tuning: &Tuning, energy: f32) -> ([ArmPose; 2], [ArmPose; 2]) {
    let size = tuning.gesture_size * (0.85 + 0.5 * energy.clamp(0.0, 1.0));
    let reach = (0.55 + 0.45 * tuning.gesture_size).clamp(0.0, 1.0);
    let rest = tuning.talk_rest;
    let place = |authored: Option<ArmPose>| authored.map_or(rest, |a| rest.lerp(&a, reach));
    let prep = [place(Some(gesture.prep.lead)), place(gesture.prep.other)];
    let to = [place(Some(gesture.stroke.lead)), place(gesture.stroke.other)];
    let stroke = [0, 1].map(|i| prep[i].lerp(&to[i], size).clamped());
    (prep.map(ArmPose::clamped), stroke)
}
