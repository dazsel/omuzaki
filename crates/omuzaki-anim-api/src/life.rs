//! The life layer (spec PF-24): the procedural "in between" that keeps a character alive when no human is driving it,
//! and fills in when tracking drops. Blinks, saccades, eyes leading the head, smile-to-cheek coupling, laugh bob,
//! speech nods, breathing, asymmetry and springs.
//!
//! The contract is deliberately about **determinism and ownership**, because those are what other systems rely on:
//! - **Seeded.** All randomness comes from [`LifeStreams`], four independent [`Rng`] streams forked from one seed in a
//!   fixed order, so adding a draw to blinks never shifts the saccades. The same seed plus the same inputs replay
//!   exactly (spec §8).
//! - **Never fights a human.** It only changes channels no person drives ([`Ownership::human_mask`](crate::Ownership::human_mask)); where a person
//!   does drive a region, its springs snap to the incoming values so handing back has no jump.
//! - **Measurable.** Every tuning number is a field of [`LifeParams`], so the liveliness metrics (spec §15) can be run
//!   against any set of numbers.
//!
//! Defaults are the prototype's tuned values (`life.js`), converted to radians and seconds.

use omuzaki_core::rng::Rng;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::blend::Bus;
use crate::frame::Tick;

/// Audio-derived hints for the life layer (spec PF-08). All zero when there is no audio.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Cues {
    /// Smoothed speech loudness, `0..1`. Drives head sway, laugh strength and brow kicks.
    pub loudness: f32,
    /// True on the frame a stressed syllable starts: brows kick up, the head nods.
    pub onset: bool,
    /// Probability the person is laughing, `0..1`, from audio laughter detection. The layer also infers laughs from
    /// a wide smile, an open mouth and loudness, as the prototype did; this only strengthens the case.
    pub laugh: f32,
}

/// A closed range `lo..=hi` to draw from. `sample(r)` maps a uniform `r` in `[0, 1)` onto it.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    /// Smallest value.
    pub lo: f32,
    /// Largest value.
    pub hi: f32,
}

impl Span {
    /// A span.
    pub const fn new(lo: f32, hi: f32) -> Self {
        Self { lo, hi }
    }

    /// The value at fraction `r` of the span.
    pub fn sample(self, r: f32) -> f32 {
        self.lo + (self.hi - self.lo) * r
    }

    fn scaled(self, k: f32) -> Self {
        Self { lo: self.lo * k, hi: self.hi * k }
    }
}

/// A damped spring's feel: frequency and damping ratio.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Dynamics {
    /// Natural frequency, Hz. Higher is snappier.
    pub freq_hz: f32,
    /// Damping ratio: `1` settles without overshoot, below `1` overshoots a little.
    pub damping: f32,
}

impl Dynamics {
    const fn new(freq_hz: f32, damping: f32) -> Self {
        Self { freq_hz, damping }
    }
}

/// How channels chase their targets (replaces fixed easing, so everything has momentum).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LifeDynamics {
    /// Lids: stiff.
    pub eye: Dynamics,
    /// Jaw and lips: medium.
    pub mouth: Dynamics,
    /// Brows.
    pub brow: Dynamics,
    /// Head angles: soft, with a slight overshoot.
    pub head: Dynamics,
    /// Body lean.
    pub lean: Dynamics,
    /// Gaze: a saccade lands in about 40 ms.
    pub gaze: Dynamics,
    /// Cheek flush: no overshoot.
    pub slow: Dynamics,
    /// With texture on, the leading side's springs run this fraction faster and the other this fraction slower, so
    /// the two sides of the face do not arrive together. `0` removes the skew.
    pub side_skew: f32,
}

impl Default for LifeDynamics {
    fn default() -> Self {
        Self {
            eye: Dynamics::new(12.0, 0.9),
            mouth: Dynamics::new(8.0, 0.75),
            brow: Dynamics::new(5.0, 0.7),
            head: Dynamics::new(2.5, 0.55),
            lean: Dynamics::new(2.0, 0.65),
            gaze: Dynamics::new(12.0, 0.9),
            slow: Dynamics::new(1.5, 1.0),
            side_skew: 0.08,
        }
    }
}

/// Blink timing (spec PF-24: 15 to 20 a minute, never regular).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BlinkParams {
    /// No blink comes sooner than this after the last, seconds.
    pub gap_floor_s: f32,
    /// Scale of the gap's long tail, seconds (see [`BlinkParams::gap`]).
    pub gap_scale_s: f32,
    /// Gaps longer than this are redrawn, seconds.
    pub gap_cap_s: f32,
    /// Chance that a blink is followed by a second one.
    pub double_chance: f32,
    /// Delay of that second blink, seconds.
    pub double_delay_s: Span,
    /// Lids close fast, seconds.
    pub close_s: Span,
    /// Lids open slower, seconds.
    pub open_s: Span,
    /// How shut each lid gets (`1` fully), drawn per eye so no two blinks are identical.
    pub depth: Span,
    /// How late the lagging eye is, seconds.
    pub eye_lag_s: Span,
    /// Chance of an extra blink when the gaze jumps.
    pub on_glance_chance: f32,
    /// Chance of an extra blink when the head turns fast.
    pub on_head_turn_chance: f32,
    /// After a person lets go of the eyes, or a score's shut eyes open, wait this long before the first blink, seconds.
    pub settle_s: f32,
    /// Retry delay when the source's eyes are already shut, seconds: drawn from this span.
    pub retry_s: Span,
    /// No extra blink (from a glance or a head turn) within this long of the last blink, seconds.
    pub extra_cooldown_s: f32,
    /// Both source lids blinked at least this much count as already shut.
    pub shut_blink: f32,
    /// A source `eyesSqueeze`, or ^^ arcs, at least this closed count as already shut.
    pub shut_arc: f32,
    /// Both source eyes squinted at least this much count as already shut (the prototype's "open below 0.15").
    pub shut_squint: f32,
}

impl Default for BlinkParams {
    fn default() -> Self {
        Self {
            gap_floor_s: 0.8,
            gap_scale_s: 1.6,
            gap_cap_s: 6.0,
            double_chance: 0.12,
            double_delay_s: Span::new(0.28, 0.40),
            close_s: Span::new(0.06, 0.08),
            open_s: Span::new(0.12, 0.18),
            depth: Span::new(0.93, 1.0),
            eye_lag_s: Span::new(0.005, 0.015),
            on_glance_chance: 0.3,
            on_head_turn_chance: 0.4,
            settle_s: 0.3,
            retry_s: Span::new(0.3, 0.6),
            extra_cooldown_s: 0.3,
            shut_blink: 0.6,
            shut_arc: 0.5,
            shut_squint: 0.85,
        }
    }
}

impl BlinkParams {
    /// Draws the time to the next blink: an Erlang-3 gap (three exponentials summed) on top of the floor, redrawn up
    /// to 8 times until it is under the cap. Never regular, rarely long. Consumes 3 to 24 draws from `rng`.
    pub fn gap(&self, rng: &mut Rng) -> f32 {
        for _ in 0..8 {
            let g = self.gap_floor_s
                - self.gap_scale_s * ((1.0 - rng.next_f32()) * (1.0 - rng.next_f32()) * (1.0 - rng.next_f32())).ln();
            if g <= self.gap_cap_s {
                return g;
            }
        }
        self.gap_cap_s
    }
}

/// Gaze behaviour: micro-saccades, glances away, and the eyes leading a head turn.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GazeParams {
    /// Time between micro-saccades, seconds.
    pub saccade_gap_s: Span,
    /// Size of a micro-saccade, look units.
    pub saccade_size: Span,
    /// Saccades are steered back toward centre when they would wander past this, look units.
    pub saccade_radius: f32,
    /// Time between glances away, seconds.
    pub glance_gap_s: Span,
    /// How long a glance lasts, seconds.
    pub glance_hold_s: Span,
    /// Horizontal size of a glance (the side is random), look units.
    pub glance_x: Span,
    /// Vertical size of a glance, look units.
    pub glance_y: Span,
    /// Fixation drift amplitude, look units: no half-second window is ever perfectly still.
    pub drift: f32,
    /// Head speed that triggers the eyes-lead behaviour, radians per second (the prototype's 40 degrees per second).
    pub lead_speed_rad_s: f32,
    /// How long the head is held back so the eyes arrive first, seconds (120 to 200 ms).
    pub lead_hold_s: Span,
    /// Longest the eyes may lead, look units.
    pub lead_max: f32,
    /// The gaze offset never exceeds this, look units.
    pub reach: f32,
    /// The first glance comes sooner than the usual gap: this fraction of a normal draw.
    pub first_glance: f32,
    /// A saccade steered back toward the centre aims with this much random spread, radians.
    pub steer_spread_rad: f32,
    /// Fixation drift sine frequency, Hz, for the horizontal and vertical axes.
    pub drift_hz: [f32; 2],
    /// Fixation drift sine amplitude as a fraction of the drift, horizontal and vertical (a sine on top of the noise
    /// keeps every half second moving: noise alone parks at its extremes).
    pub drift_sine: [f32; 2],
    /// The head speed estimate follows the raw speed with this time constant, seconds, so a 30 fps tracker feeding a
    /// 60 fps loop does not flicker around the lead threshold.
    pub lead_speed_tau_s: f32,
    /// This long below the lead threshold re-arms the eyes-lead trigger, seconds.
    pub lead_calm_s: f32,
    /// A lead ends once the head is within this of its target, radians (about 1.5 degrees).
    pub lead_settled_rad: f32,
    /// A lead ends after this long whatever the head does, seconds.
    pub lead_max_s: f32,
}

impl Default for GazeParams {
    fn default() -> Self {
        Self {
            saccade_gap_s: Span::new(0.25, 1.2),
            saccade_size: Span::new(0.03, 0.08),
            saccade_radius: 0.12,
            glance_gap_s: Span::new(5.0, 12.0),
            glance_hold_s: Span::new(0.3, 0.6),
            glance_x: Span::new(0.25, 0.40),
            glance_y: Span::new(-0.1, 0.2),
            drift: 0.02,
            lead_speed_rad_s: 0.698_131_7,
            lead_hold_s: Span::new(0.12, 0.20),
            lead_max: 0.6,
            reach: 0.8,
            first_glance: 0.4,
            steer_spread_rad: 1.2,
            drift_hz: [0.83, 1.07],
            drift_sine: [0.6, 0.5],
            lead_speed_tau_s: 0.05,
            lead_calm_s: 0.25,
            lead_settled_rad: 0.026_2,
            lead_max_s: 1.5,
        }
    }
}

/// Head and body motion that rides on the source: noise, speech nods, laugh bob, breathing.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HeadParams {
    /// Head noise amplitude about yaw, pitch and roll, radians.
    pub noise_rad: [f32; 3],
    /// Extra sway while talking: noise is scaled by `1 + talk_gain * loudness`.
    pub talk_gain: f32,
    /// Nod on a stressed syllable, radians of pitch.
    pub nod_pitch_rad: f32,
    /// Tilt that alternates sides with each nod, radians of roll.
    pub nod_roll_rad: f32,
    /// Head bob back and up during a laugh, radians (rises with loudness).
    pub laugh_bob_rad: Span,
    /// Laugh bob rate, Hz.
    pub laugh_bob_hz: f32,
    /// Breathing rate, Hz (it wanders by about 10%).
    pub breath_hz: f32,
    /// Head pitch moved by breathing, radians.
    pub breath_pitch_rad: f32,
    /// The breathing rate wanders by this fraction of itself, slowly.
    pub breath_wander: f32,
    /// A nod decays with this time constant, seconds.
    pub nod_decay_s: f32,
    /// The smoothed loudness that makes the head sway more while talking follows the speech with this time constant,
    /// seconds.
    pub talk_tau_s: f32,
    /// The laugh bob swings about its mean by this much, radians (0.7 degrees).
    pub bob_swing_rad: f32,
    /// The laugh's roll jitter, the sine part, radians (0.9 degrees).
    pub roll_jitter_rad: f32,
    /// The laugh's roll jitter, the noise part, radians (0.5 degrees).
    pub roll_noise_rad: f32,
}

impl Default for HeadParams {
    fn default() -> Self {
        Self {
            noise_rad: [0.027_9, 0.020_9, 0.022_7],
            talk_gain: 1.4,
            nod_pitch_rad: 0.069_8,
            nod_roll_rad: 0.034_9,
            laugh_bob_rad: Span::new(0.038_4, 0.057_6),
            laugh_bob_hz: 4.0,
            breath_hz: 0.25,
            breath_pitch_rad: 0.007,
            breath_wander: 0.1,
            nod_decay_s: 0.12,
            talk_tau_s: 0.4,
            bob_swing_rad: 0.012_2,
            roll_jitter_rad: 0.015_7,
            roll_noise_rad: 0.008_7,
        }
    }
}

/// Couplings between channels, and the texture noise that keeps the face from being a still image.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct FaceParams {
    /// Lower lids rise by this much per unit of smile (the cheek push).
    pub smile_to_lids: f32,
    /// Blush added per unit of smile.
    pub smile_to_blush: f32,
    /// Extra squint during a laugh.
    pub laugh_squint: f32,
    /// A big open mouth under raised brows widens the eyes by up to this much.
    pub surprise_widen: f32,
    /// Brow kick on a stressed syllable: `base + per_loud * loudness`.
    pub brow_kick_base: f32,
    /// See `brow_kick_base`.
    pub brow_kick_per_loud: f32,
    /// The kick decays with this time constant, seconds.
    pub brow_kick_decay_s: f32,
    /// Brow noise amplitude, channel units.
    pub brow_noise: f32,
    /// Mouth-corner noise amplitude, channel units.
    pub mouth_noise: f32,
    /// Lid drift amplitude, channel units.
    pub lid_drift: f32,
    /// One side's noise leads the other by this much, seconds.
    pub side_lag_s: Span,
    /// Left and right noise amplitudes differ by up to this fraction.
    pub side_amp: f32,
    /// A coupling (smile, laugh) raises a lid channel at most to this, but never lowers what the source already shows.
    pub coupled_lid_cap: f32,
    /// Jaw opening at which the surprise look starts (`lo`) and is complete (`hi`).
    pub surprise_jaw: Span,
    /// Brow height at which the surprise look starts (`lo`) and is complete (`hi`).
    pub surprise_brows: Span,
    /// The lid drift is noise plus a slow sine at this fraction of its amplitude (noise alone parks at its extremes).
    pub lid_drift_sine: f32,
    /// Frequency of that sine, Hz.
    pub lid_drift_hz: f32,
    /// A brow kick varies by up to this much from one syllable to the next, so no two are identical.
    pub brow_kick_jitter: f32,
}

impl Default for FaceParams {
    fn default() -> Self {
        Self {
            smile_to_lids: 0.35,
            smile_to_blush: 0.25,
            laugh_squint: 0.2,
            surprise_widen: 0.35,
            brow_kick_base: 0.27,
            brow_kick_per_loud: 0.3,
            brow_kick_decay_s: 0.2,
            brow_noise: 0.06,
            mouth_noise: 0.05,
            lid_drift: 0.03,
            side_lag_s: Span::new(0.03, 0.06),
            side_amp: 0.1,
            coupled_lid_cap: 0.6,
            surprise_jaw: Span::new(0.6, 0.9),
            surprise_brows: Span::new(0.3, 0.6),
            lid_drift_sine: 0.4,
            lid_drift_hz: 1.21,
            brow_kick_jitter: 0.03,
        }
    }
}

/// Laugh detection: when a big open-mouthed loud smile counts as a laugh, and how it ramps.
///
/// A laugh starts when smile, jaw opening and loudness all pass the `start_*` thresholds, and ends once they have
/// stayed below the (lower) `keep_*` thresholds for `hold_s`, so a flicker in loudness cannot make it chatter.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LaughParams {
    /// Net smile needed to start a laugh.
    pub start_smile: f32,
    /// Jaw opening needed to start a laugh.
    pub start_jaw: f32,
    /// Loudness needed to start a laugh.
    pub start_loud: f32,
    /// Net smile below which a running laugh may end.
    pub keep_smile: f32,
    /// Jaw opening below which a running laugh may end.
    pub keep_jaw: f32,
    /// Loudness below which a running laugh may end.
    pub keep_loud: f32,
    /// How long the keep thresholds must be missed before the laugh ends, seconds.
    pub hold_s: f32,
    /// A laugh ramps in and out over this long, seconds.
    pub ramp_s: f32,
    /// Closing into ^^ needs a laugh at least this loud.
    pub arc_loud: f32,
    /// The bob rate wanders by this fraction of itself.
    pub rate_wander: f32,
}

impl Default for LaughParams {
    fn default() -> Self {
        Self {
            start_smile: 0.6,
            start_jaw: 0.3,
            start_loud: 0.45,
            keep_smile: 0.45,
            keep_jaw: 0.2,
            keep_loud: 0.3,
            hold_s: 0.12,
            ramp_s: 0.15,
            arc_loud: 0.85,
            rate_wander: 0.1,
        }
    }
}

/// Every tuning number of the life layer.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LifeParams {
    /// Spring feel per channel family.
    pub springs: LifeDynamics,
    /// Blinks.
    pub blink: BlinkParams,
    /// Gaze.
    pub gaze: GazeParams,
    /// Head and body.
    pub head: HeadParams,
    /// Couplings and texture.
    pub face: FaceParams,
    /// Laugh detection.
    pub laugh: LaughParams,
}

/// A personality preset (spec PF-24). Presets are starting points for [`LifeParams`]; the params are the truth.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Personality {
    /// The prototype's tuned defaults.
    #[default]
    Natural,
    /// Quicker blinks and glances, bigger head motion, snappier springs.
    Energetic,
    /// Slow, still and composed.
    Calm,
    /// Glances away often, small head motion, blushes easily.
    Shy,
    /// Slow heavy blinks, rare glances, soft springs.
    Sleepy,
}

impl LifeParams {
    /// The starting numbers for a personality. The multipliers are first guesses; they are tuned against the
    /// liveliness metrics (spec §15), and only this function changes when they are.
    pub fn for_personality(personality: Personality) -> Self {
        let mut p = Self::default();
        match personality {
            Personality::Natural => {}
            Personality::Energetic => p.tune(0.8, 0.7, 1.4, 1.15, 1.0),
            Personality::Calm => p.tune(1.3, 1.3, 0.6, 0.9, 1.0),
            Personality::Shy => {
                p.tune(1.0, 1.0, 0.7, 1.0, 1.6);
                p.gaze.glance_gap_s = p.gaze.glance_gap_s.scaled(0.6);
                p.gaze.glance_x = p.gaze.glance_x.scaled(1.3);
            }
            Personality::Sleepy => {
                p.tune(0.85, 1.5, 0.8, 0.8, 1.0);
                p.blink.close_s = p.blink.close_s.scaled(1.6);
                p.blink.open_s = p.blink.open_s.scaled(1.8);
                p.gaze.glance_gap_s = p.gaze.glance_gap_s.scaled(1.5);
            }
        }
        p
    }

    /// Scales the five knobs the presets share: blink gap, saccade gap, head noise, spring frequency, blush.
    fn tune(&mut self, blink_gap: f32, saccade_gap: f32, head_noise: f32, spring_freq: f32, blush: f32) {
        self.blink.gap_scale_s *= blink_gap;
        self.gaze.saccade_gap_s = self.gaze.saccade_gap_s.scaled(saccade_gap);
        self.head.noise_rad = self.head.noise_rad.map(|n| n * head_noise);
        self.face.smile_to_blush *= blush;
        for d in [
            &mut self.springs.eye,
            &mut self.springs.mouth,
            &mut self.springs.brow,
            &mut self.springs.head,
            &mut self.springs.lean,
            &mut self.springs.gaze,
        ] {
            d.freq_hz *= spring_freq;
        }
    }
}

/// Which behaviours run. Turn one off to isolate it in a test, or because a score owns it (spec FE-05: each rule can
/// be toggled).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // independent on/off switches; a bitset would hide them from ops and schemas
pub struct LifeSystems {
    /// Blinks.
    pub blink: bool,
    /// Micro-saccades, glances and fixation drift.
    pub gaze: bool,
    /// Eyes leading a head turn.
    pub gaze_lead: bool,
    /// Smile to cheek push and blush; surprise widening.
    pub coupling: bool,
    /// Laugh: squint, head bob, optionally ^^.
    pub laugh: bool,
    /// Brow kicks and head nods on stressed syllables.
    pub speech: bool,
    /// Breathing.
    pub breathing: bool,
    /// Noise on head, brows and mouth corners, and left/right asymmetry.
    pub texture: bool,
}

impl Default for LifeSystems {
    fn default() -> Self {
        Self {
            blink: true,
            gaze: true,
            gaze_lead: true,
            coupling: true,
            laugh: true,
            speech: true,
            breathing: true,
            texture: true,
        }
    }
}

/// Settings for one life layer.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LifeConfig {
    /// The seed. Same seed, same inputs, same output.
    pub seed: u64,
    /// How much of the layer shows: `0` leaves the bus untouched, `1` is the full layer (scales every deviation).
    pub amount: f32,
    /// Which preset `params` started from (informational).
    pub personality: Personality,
    /// Which behaviours run.
    pub systems: LifeSystems,
    /// Whether a loud laugh may close the eyes into ^^ arcs. Off when a score owns the arcs.
    pub laugh_arcs: bool,
    /// The tuning numbers.
    pub params: LifeParams,
}

impl Default for LifeConfig {
    fn default() -> Self {
        Self::new(1)
    }
}

impl LifeConfig {
    /// Natural personality, full strength, every behaviour on.
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            amount: 1.0,
            personality: Personality::Natural,
            systems: LifeSystems::default(),
            laugh_arcs: true,
            params: LifeParams::default(),
        }
    }

    /// The same config with a personality preset's numbers.
    #[must_use]
    pub fn with_personality(mut self, personality: Personality) -> Self {
        self.personality = personality;
        self.params = LifeParams::for_personality(personality);
        self
    }
}

/// Stream ids for [`LifeStreams`]. Part of the determinism contract: changing one changes every replay.
pub mod stream {
    /// Blink gaps, depth, doubles.
    pub const BLINK: u64 = 1;
    /// Saccades and glances.
    pub const GAZE: u64 = 2;
    /// Head lead hold, brow kick jitter.
    pub const HEAD: u64 = 3;
    /// The seed's fixed quirks ([`LifeTraits`](super::LifeTraits)).
    pub const TRAITS: u64 = 4;
}

/// The independent random streams of one life layer, derived from its seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifeStreams {
    /// Blinks.
    pub blink: Rng,
    /// Gaze.
    pub gaze: Rng,
    /// Head.
    pub head: Rng,
    /// The one-time draw of [`LifeTraits`].
    pub traits: Rng,
}

impl LifeStreams {
    /// Derives the four streams. The fork order is fixed, so every implementation gets the same streams from a seed.
    pub fn new(seed: u64) -> Self {
        let mut master = Rng::new(seed ^ 0x9E37_79B9);
        Self {
            blink: master.fork(stream::BLINK),
            gaze: master.fork(stream::GAZE),
            head: master.fork(stream::HEAD),
            traits: master.fork(stream::TRAITS),
        }
    }
}

/// The quirks a seed gives a character for good: which side leads, which eye lags in a blink. They make two
/// characters with different seeds move differently, and one character move the same way every time.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LifeTraits {
    /// True if the character's left side leads in noise and spring arrival.
    pub left_leads: bool,
    /// By how much the trailing side lags, seconds (drawn from [`FaceParams::side_lag_s`]).
    pub side_lag_s: f32,
    /// Left/right amplitude difference, `-1..1` times [`FaceParams::side_amp`].
    pub side_amp: f32,
    /// True if the right eye is the one that closes a few milliseconds late in a blink.
    pub right_lid_lags: bool,
    /// Phases of the slow gaze-x, gaze-y and lid drifts, radians.
    pub drift_phase: [f32; 3],
    /// Seed of the smooth noise lattice.
    pub noise_seed: u32,
}

impl LifeTraits {
    /// Draws the traits from the traits stream (always in this order, so they are stable per seed).
    #[allow(clippy::cast_possible_truncation)] // the low 32 bits of a random word are a fine noise seed
    pub fn draw(rng: &mut Rng, face: &FaceParams) -> Self {
        let left_leads = rng.next_f32() < 0.5;
        let side_lag_s = face.side_lag_s.sample(rng.next_f32());
        let side_amp = 2.0 * rng.next_f32() - 1.0;
        let right_lid_lags = rng.next_f32() < 0.5;
        let tau = std::f32::consts::TAU;
        let drift_phase = [rng.next_f32() * tau, rng.next_f32() * tau, rng.next_f32() * tau];
        Self { left_leads, side_lag_s, side_amp, right_lid_lags, drift_phase, noise_seed: rng.next_u64() as u32 }
    }
}

/// Adds life to a bus. One instance per character; it owns its springs, timers and random streams.
pub trait LifeLayer: Send {
    /// Advances one frame, editing `bus` in place. Hot path: no allocation, no locking.
    ///
    /// The contract:
    /// 1. **No time, no change.** If `tick.dt` is zero, negative or not finite, nothing advances and `bus` is
    ///    untouched. A `dt` above 0.25 s is treated as 0.25 s.
    /// 2. **The incoming bus is the target.** Springs chase the values already in `bus`; blinks, gaze and noise ride
    ///    on top. The result is `incoming + amount * (processed - incoming)`.
    /// 3. **Humans are never overridden.** A channel in [`Ownership::human_mask`](crate::Ownership::human_mask) is not written. A behaviour that
    ///    needs a whole region free is skipped while a human drives any channel of it: blinks, lid springs and arcs
    ///    need `EYES`; saccades, glances and drift need `GAZE`; brow kicks and brow noise need `BROWS`; mouth
    ///    springs, mouth-corner noise and smile blush need `MOUTH`; head noise, nods, bob and lean need `HEAD`.
    ///    While skipped, the matching springs snap to the incoming values, so hand-back has no jump.
    /// 4. **Ownership label.** Channels the layer changes whose owner is `Rest`, `Score` or `Audio` are relabelled
    ///    [`Source::Life`](crate::Source::Life); values are unaffected.
    /// 5. **Deterministic.** The output is a pure function of the config (with its seed) and the sequence of
    ///    `(tick, cues, bus)` inputs. No clock, no global RNG, no thread-dependent state.
    fn step(&mut self, tick: Tick, cues: &Cues, bus: &mut Bus);

    /// The settings in use.
    fn config(&self) -> &LifeConfig;

    /// Replaces the settings. If the seed changed this resets the layer; otherwise the state carries on, so
    /// turning a knob mid-performance does not make the character jump.
    fn set_config(&mut self, config: LifeConfig);

    /// Returns to the state just after construction with the current seed (a scrub or a restarted take).
    fn reset(&mut self);
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // tests compare values that are copied or exactly computed, and cast small loop counters
mod tests {
    use super::*;

    #[test]
    fn streams_are_reproducible_and_independent() {
        let a = LifeStreams::new(7);
        assert_eq!(a, LifeStreams::new(7));
        assert_ne!(a.blink, LifeStreams::new(8).blink);
        let (mut x, mut y) = (a.clone(), a);
        // Drawing a lot from one stream must not move another.
        for _ in 0..100 {
            x.blink.next_u64();
        }
        assert_eq!(x.gaze, y.gaze);
        assert_ne!(x.blink, y.blink);
        assert_ne!(y.gaze.next_u64(), y.head.next_u64());
    }

    #[test]
    fn traits_are_stable_per_seed_and_in_range() {
        let face = FaceParams::default();
        let t = |seed| LifeTraits::draw(&mut LifeStreams::new(seed).traits, &face);
        assert_eq!(t(3), t(3));
        assert_ne!(t(3), t(4));
        for seed in 0..50 {
            let tr = t(seed);
            assert!((0.03..=0.06).contains(&tr.side_lag_s) && (-1.0..=1.0).contains(&tr.side_amp));
            assert!(tr.drift_phase.iter().all(|p| (0.0..std::f32::consts::TAU).contains(p)));
        }
    }

    #[test]
    fn blink_gaps_are_never_regular_and_average_near_the_prototype_rate() {
        let blink = BlinkParams::default();
        let mut rng = Rng::new(11);
        let gaps: Vec<f32> = (0..2000).map(|_| blink.gap(&mut rng)).collect();
        assert!(gaps.iter().all(|g| (blink.gap_floor_s..=blink.gap_cap_s).contains(g)));
        let mean = gaps.iter().sum::<f32>() / gaps.len() as f32;
        // Erlang-3 mean is floor + 3 * scale = 5.6 s before the 6 s cap pulls it down; extra blinks and doubles
        // bring the whole layer to about 17 a minute.
        assert!((3.5..5.6).contains(&mean), "mean gap {mean}");
        let var = gaps.iter().map(|g| (g - mean).powi(2)).sum::<f32>() / gaps.len() as f32;
        assert!(var.sqrt() > 0.8, "gaps vary");
    }

    #[test]
    fn defaults_match_the_prototype_tuning() {
        let p = LifeParams::default();
        assert_eq!((p.blink.gap_floor_s, p.blink.double_chance), (0.8, 0.12));
        assert_eq!(p.springs.head, Dynamics::new(2.5, 0.55));
        assert!((p.head.noise_rad[0].to_degrees() - 1.6).abs() < 0.01);
        assert!((p.head.nod_pitch_rad.to_degrees() - 4.0).abs() < 0.01);
        assert!((p.gaze.lead_speed_rad_s.to_degrees() - 40.0).abs() < 0.01);
    }

    #[test]
    fn personalities_differ_in_the_expected_direction() {
        let natural = LifeParams::for_personality(Personality::Natural);
        assert_eq!(natural, LifeParams::default());
        let energetic = LifeParams::for_personality(Personality::Energetic);
        let calm = LifeParams::for_personality(Personality::Calm);
        let shy = LifeParams::for_personality(Personality::Shy);
        let sleepy = LifeParams::for_personality(Personality::Sleepy);
        assert!(
            energetic.blink.gap_scale_s < natural.blink.gap_scale_s
                && calm.blink.gap_scale_s > natural.blink.gap_scale_s
        );
        assert!(energetic.head.noise_rad[0] > calm.head.noise_rad[0]);
        assert!(shy.gaze.glance_gap_s.hi < natural.gaze.glance_gap_s.hi);
        assert!(shy.face.smile_to_blush > natural.face.smile_to_blush);
        assert!(
            sleepy.blink.open_s.lo > natural.blink.open_s.lo
                && sleepy.springs.head.freq_hz < natural.springs.head.freq_hz
        );
    }

    #[test]
    fn config_round_trips_and_fills_defaults() {
        let cfg = LifeConfig::new(9).with_personality(Personality::Shy);
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(serde_json::from_str::<LifeConfig>(&json).unwrap(), cfg);
        let partial: LifeConfig = serde_json::from_str(r#"{"seed": 5, "systems": {"blink": false}}"#).unwrap();
        assert_eq!(partial.seed, 5);
        assert!(!partial.systems.blink && partial.systems.gaze && partial.amount == 1.0);
    }
}
