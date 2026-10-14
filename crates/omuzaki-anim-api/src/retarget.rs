//! Retarget: from a raw tracker frame to calibrated, smoothed bus channels (spec PF-20 to PF-22, FE-06).
//!
//! The prototype's `arkit.js` did this in one function with three hidden mechanisms (a neutral map, a gain table, and
//! per-user ranges). The contract folds them into **one**: every `ARKit` channel has a [`ChannelRange`] that maps the
//! tracker's raw numbers to `0..1`. Rest value = `lo`; a typical webcam's missing amplitude is a smaller `hi - lo`
//! (what the prototype called "gain"); a guided calibration measures both. An uncalibrated user simply gets a
//! [`Calibration`] built from their device's profile.
//!
//! Per channel, in this order (all in the implementation's hot loop, none allocating):
//! 1. optional left/right mirror ([`RetargetConfig::mirror`])
//! 2. range normalise to `0..1`
//! 3. [`Response`]: dead zone, saturation, curve, inversion (PF-22); eye-blink has a built-in dead zone
//! 4. exaggeration on the [`EXAGGERATED`] channels (FE-06): `value^(1/amount)`
//! 5. One-Euro smoothing, parameters from [`OneEuroParams::for_smoothing`] (PF-21)
//!
//! Gaze, head pose and lean come from the frame's head and eye data; see [`Retarget::apply`].

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::blend::Layer;
use crate::channel::{ARKIT_COUNT, Channel, ChannelMask};
use crate::frame::{DeviceProfile, HeadPose, TrackFrame};

/// The narrowest range a [`Calibrator`] will fit: a channel the performer barely moves keeps at least this span, so it
/// is not amplified into noise.
pub const MIN_FIT_SPAN: f32 = 0.15;

/// The smallest span a range may have when it is applied. A channel the user barely moves must not be amplified into noise.
pub const MIN_SPAN: f32 = 0.05;

/// The eye rotation, in radians (40 degrees), that counts as a full-weight `eyeLook*` channel when a source sends
/// gaze as angles ([`EyeAngles`](crate::EyeAngles)).
pub const LOOK_FULL_RAD: f32 = 0.698_131_7;

/// Channels the exaggeration dial boosts (the prototype's list). Blinks, gaze and sideways jaw are deliberately
/// absent: they must stay literal, or a blink would stop meaning "eyes closed".
pub const EXAGGERATED: ChannelMask = ChannelMask::from_channels(&[
    Channel::BROW_DOWN_LEFT,
    Channel::BROW_DOWN_RIGHT,
    Channel::BROW_INNER_UP,
    Channel::BROW_OUTER_UP_LEFT,
    Channel::BROW_OUTER_UP_RIGHT,
    Channel::CHEEK_PUFF,
    Channel::CHEEK_SQUINT_LEFT,
    Channel::CHEEK_SQUINT_RIGHT,
    Channel::EYE_SQUINT_LEFT,
    Channel::EYE_SQUINT_RIGHT,
    Channel::EYE_WIDE_LEFT,
    Channel::EYE_WIDE_RIGHT,
    Channel::MOUTH_SMILE_LEFT,
    Channel::MOUTH_SMILE_RIGHT,
    Channel::MOUTH_FROWN_LEFT,
    Channel::MOUTH_FROWN_RIGHT,
    Channel::MOUTH_STRETCH_LEFT,
    Channel::MOUTH_STRETCH_RIGHT,
    Channel::MOUTH_DIMPLE_LEFT,
    Channel::MOUTH_DIMPLE_RIGHT,
    Channel::MOUTH_PUCKER,
    Channel::MOUTH_FUNNEL,
]);

/// Channels that get the fast One-Euro filter: lag on a blink, a glance or a syllable is visible, so they trade a
/// little jitter rejection for responsiveness.
pub const FAST: ChannelMask = ChannelMask::from_channels(&[
    Channel::EYE_BLINK_LEFT,
    Channel::EYE_BLINK_RIGHT,
    Channel::EYE_LOOK_DOWN_LEFT,
    Channel::EYE_LOOK_DOWN_RIGHT,
    Channel::EYE_LOOK_IN_LEFT,
    Channel::EYE_LOOK_IN_RIGHT,
    Channel::EYE_LOOK_OUT_LEFT,
    Channel::EYE_LOOK_OUT_RIGHT,
    Channel::EYE_LOOK_UP_LEFT,
    Channel::EYE_LOOK_UP_RIGHT,
    Channel::JAW_OPEN,
]);

/// Maps a tracker's raw value for one channel to `0..1`: `lo` is what it reads at rest, `hi` what it reads at full
/// effort.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelRange {
    /// Raw value at rest (maps to 0).
    pub lo: f32,
    /// Raw value at full effort (maps to 1).
    pub hi: f32,
}

impl ChannelRange {
    /// The raw value is already `0..1` (an `ARKit`-scaled source).
    pub const IDENTITY: Self = Self { lo: 0.0, hi: 1.0 };

    /// A range for a source that reads low by `gain` (the prototype's "gain 2.0" is `hi = 0.5`).
    pub fn from_gain(gain: f32) -> Self {
        Self { lo: 0.0, hi: 1.0 / gain.max(f32::EPSILON) }
    }

    /// `raw` mapped to `0..1`, clamped. The span is never narrower than [`MIN_SPAN`].
    pub fn normalize(self, raw: f32) -> f32 {
        ((raw - self.lo) / (self.hi - self.lo).max(MIN_SPAN)).clamp(0.0, 1.0)
    }
}

/// Everything the retarget needs to know about one performer on one device (spec PF-20). `Copy` and fixed-size so the
/// hot loop reads it without indirection; saved per user and device, and remembered.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Calibration {
    /// The device class the ranges were made for.
    pub profile: DeviceProfile,
    ranges: [ChannelRange; ARKIT_COUNT],
    /// The performer's relaxed head pose; head angles and lean are measured from it.
    pub head_rest: HeadPose,
}

impl Calibration {
    /// Every range is `0..1` and the head rest pose is level: right for `ARKit`-scaled sources, and the neutral
    /// starting point for building others.
    pub fn identity(profile: DeviceProfile) -> Self {
        Self { profile, ranges: [ChannelRange::IDENTITY; ARKIT_COUNT], head_rest: HeadPose::default() }
    }

    /// Ranges from a per-channel gain, for uncalibrated sources that read low. The implementation owns the gain
    /// table (it is tuning data); this only fixes what a gain means.
    pub fn from_gains(profile: DeviceProfile, gain: impl Fn(Channel) -> f32) -> Self {
        let mut cal = Self::identity(profile);
        for info in Channel::arkit() {
            cal.ranges[info.channel.index()] = ChannelRange::from_gain(gain(info.channel));
        }
        cal
    }

    /// The range of an `ARKit` channel (identity for any other channel, which a tracker cannot supply).
    pub fn range(&self, channel: Channel) -> ChannelRange {
        if channel.is_arkit() { self.ranges[channel.index()] } else { ChannelRange::IDENTITY }
    }

    /// Sets the range of an `ARKit` channel; other channels are ignored.
    pub fn set_range(&mut self, channel: Channel, range: ChannelRange) {
        if channel.is_arkit() {
            self.ranges[channel.index()] = range;
        }
    }

    /// The "calibrate neutral" button: takes the frame as the performer's rest face. Each present channel's `lo`
    /// moves to the frame's value and `hi` moves with it, keeping the span (the prototype's neutral subtraction).
    pub fn rest_at(&mut self, frame: &TrackFrame) {
        for c in frame.present {
            let r = &mut self.ranges[c.index()];
            let span = r.hi - r.lo;
            r.lo = frame.get(c);
            r.hi = r.lo + span;
        }
        if let Some(head) = frame.head {
            self.head_rest = head;
        }
    }
}

/// The at-rest form of a [`Calibration`]: ranges by channel name, so a saved calibration survives table changes.
#[derive(Serialize, Deserialize, JsonSchema)]
struct CalibrationFile {
    profile: DeviceProfile,
    ranges: BTreeMap<Channel, ChannelRange>,
    head_rest: HeadPose,
}

impl From<Calibration> for CalibrationFile {
    fn from(c: Calibration) -> Self {
        let ranges = Channel::arkit().iter().map(|i| (i.channel, c.ranges[i.channel.index()])).collect();
        Self { profile: c.profile, ranges, head_rest: c.head_rest }
    }
}

impl TryFrom<CalibrationFile> for Calibration {
    type Error = crate::frame::NotArkit;
    fn try_from(f: CalibrationFile) -> Result<Self, Self::Error> {
        let mut cal = Self::identity(f.profile);
        for (channel, range) in f.ranges {
            if !channel.is_arkit() {
                return Err(crate::frame::NotArkit(channel));
            }
            cal.set_range(channel, range);
        }
        cal.head_rest = f.head_rest;
        Ok(cal)
    }
}

impl Serialize for Calibration {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        CalibrationFile::from(*self).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Calibration {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        CalibrationFile::deserialize(d)?.try_into().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Calibration {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Calibration".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        CalibrationFile::json_schema(generator)
    }
}

/// Per-channel shaping after the range (spec PF-22): `v' = clamp((v - dead) / (sat - dead))`, then `v'^gamma`, then
/// optionally inverted.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    /// Input below this reads as 0 (a dead zone).
    pub dead: f32,
    /// Input above this reads as 1 (extra gain at the top).
    pub sat: f32,
    /// Curve exponent: `1` linear, `>1` flattens small values, `<1` lifts them.
    pub gamma: f32,
    /// Flip the result (`1 - v`).
    pub invert: bool,
}

impl Default for Response {
    fn default() -> Self {
        Self { dead: 0.0, sat: 1.0, gamma: 1.0, invert: false }
    }
}

impl Response {
    /// The built-in response of a channel when the user hasn't set one. Only blinks differ: a dead zone so looking
    /// down or squinting doesn't half-close the eye, then a full close (standard practice, from the prototype).
    pub fn built_in(channel: Channel) -> Self {
        if matches!(channel, Channel::EYE_BLINK_LEFT | Channel::EYE_BLINK_RIGHT) {
            Self { dead: 0.25, sat: 0.85, ..Self::default() }
        } else {
            Self::default()
        }
    }

    /// Applies the response to a `0..1` value. `exaggeration` (`>= 1`, pass `1.0` for none) divides the curve
    /// exponent, so `2.0` is a square root: small movements read bigger, the ends stay put.
    pub fn apply(self, v: f32, exaggeration: f32) -> f32 {
        let span = (self.sat - self.dead).max(f32::EPSILON);
        let shaped = ((v - self.dead) / span).clamp(0.0, 1.0);
        // `powf` is skipped for a straight line: it is the common case, exact, and keeps the hot loop cheap.
        let exponent = self.gamma / exaggeration.max(1.0);
        let curved = if (exponent - 1.0).abs() < f32::EPSILON { shaped } else { shaped.powf(exponent) };
        if self.invert { 1.0 - curved } else { curved }
    }
}

/// How hard to smooth: One-Euro filter parameters (Casiez et al. 2012): low lag on fast moves, low jitter when still.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OneEuroParams {
    /// Cutoff at rest, Hz. Lower is smoother and laggier.
    pub min_cutoff: f32,
    /// How much the cutoff opens with speed. Higher keeps fast moves crisp.
    pub beta: f32,
    /// Cutoff for the speed estimate itself, Hz.
    pub d_cutoff: f32,
}

impl OneEuroParams {
    /// The parameters for the smoothing dial (`0` = raw, `1` = very smooth) and a channel family. `None` means
    /// "don't filter". Reproduces the prototype: base cutoff `1.2 + (1 - s) * 4.5` Hz; the fast family gets
    /// `2.6x` that cutoff with `beta 1.2`, the rest `beta 0.9`.
    pub fn for_smoothing(smoothing: f32, fast: bool) -> Option<Self> {
        if smoothing.is_nan() || smoothing <= 0.0 {
            return None;
        }
        let cutoff = 1.2 + (1.0 - smoothing.min(1.0)) * 4.5;
        Some(if fast {
            Self { min_cutoff: cutoff * 2.6, beta: 1.2, d_cutoff: 1.0 }
        } else {
            Self { min_cutoff: cutoff, beta: 0.9, d_cutoff: 1.0 }
        })
    }
}

/// What happens when the tracker loses the face (spec PF-23: "ease to rest and let the life layer take over").
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LostPolicy {
    /// Seconds of lost frames to ignore before reacting (a blink of the tracker is not a lost face).
    pub grace_s: f32,
    /// Seconds over which values ease to rest and the layer's weight fades to 0 after the grace period.
    pub fade_s: f32,
}

impl Default for LostPolicy {
    fn default() -> Self {
        Self { grace_s: 0.25, fade_s: 0.5 }
    }
}

/// The retarget's user-facing settings. Cold data: stored in the character's performer profile and edited by ops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RetargetConfig {
    /// Swap left and right: for sources whose preview is a selfie mirror (spec PF-27).
    pub mirror: bool,
    /// Exaggeration (FE-06): `1` literal, `1.5` anime. Applies to [`EXAGGERATED`] channels.
    pub exaggeration: f32,
    /// Smoothing dial, `0` raw to `1` very smooth (PF-21). See [`OneEuroParams::for_smoothing`].
    pub smoothing: f32,
    /// Multiplier on head angles.
    pub head_gain: f32,
    /// Lean (radians) per metre of head movement toward the camera. The prototype's 0.9 degrees per cm.
    pub lean_per_metre: f32,
    /// Per-channel overrides of [`Response::built_in`].
    pub responses: BTreeMap<Channel, Response>,
    /// Behaviour when tracking drops.
    pub lost: LostPolicy,
}

impl Default for RetargetConfig {
    fn default() -> Self {
        Self {
            mirror: false,
            exaggeration: 1.0,
            smoothing: 0.4,
            head_gain: 1.0,
            lean_per_metre: 90.0_f32.to_radians(),
            responses: BTreeMap::new(),
            lost: LostPolicy::default(),
        }
    }
}

/// Turns tracker frames into a bus layer. One instance per performer; it owns its filter state.
///
/// Implementations: the real one lives in `omuzaki-anim`; `omuzaki-anim-mock` has a deterministic stand-in. Other
/// retargets (a VRM-expression retarget for exported models, a character with non-`ARKit` controls) implement the
/// same trait.
pub trait Retarget: Send {
    /// Processes one frame into `out`. Hot path: no allocation, no locking, no I/O.
    ///
    /// Contract for `out` (the caller passes the tracker slot of its [`LayerStack`](crate::LayerStack)):
    /// - `values`/`mask`: the `ARKit` channels in `frame.present` (after mirroring), plus `headYaw`, `headPitch`,
    ///   `headRoll`, `headShiftX`, `headShiftY` and `lean` when the frame has a head pose, plus the eight
    ///   `eyeLook*` channels when it has [`EyeAngles`](crate::EyeAngles) and no `eyeLook*` blendshapes.
    /// - `mode` is `Override`.
    /// - `weight` is `1` while tracking. When the frame is [`Lost`](crate::TrackStatus::Lost) for longer than the
    ///   grace period, values ease to rest and the weight fades to `0` over [`LostPolicy::fade_s`], which hands
    ///   ownership to the life layer; it returns to `1` when the face is found again.
    /// - Head angles are `(frame - calibration.head_rest) * head_gain`, with yaw, roll and shift-x negated when
    ///   mirroring; lean is forward-positive.
    ///
    /// `dt` is the time since the previous call (clamped by the caller to `1/120..=0.25`).
    fn apply(&mut self, frame: &TrackFrame, dt: f32, out: &mut Layer);

    /// The calibration in use.
    fn calibration(&self) -> &Calibration;

    /// Replaces the calibration. Filters keep their state, so the face does not jump when a new range arrives.
    fn set_calibration(&mut self, calibration: Calibration);

    /// The settings in use.
    fn config(&self) -> &RetargetConfig;

    /// Replaces the settings (compiling them into the implementation's fixed tables; may allocate, so call it from
    /// the control thread between frames, not inside one).
    fn set_config(&mut self, config: RetargetConfig);

    /// Forgets all filter state and starts from rest, e.g. after a seek in a recorded track.
    fn reset(&mut self);
}

/// The guided calibration routine's prompts (spec PF-20), about 40 seconds in all.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationPrompt {
    /// Relax, look at the camera: gives the rest values (`lo`) and the rest head pose.
    Rest,
    /// A big smile.
    BigSmile,
    /// Raise the eyebrows.
    BrowsUp,
    /// Frown: eyebrows down.
    BrowsDown,
    /// Eyes wide open.
    EyesWide,
    /// Squint.
    Squint,
    /// Open the mouth wide.
    MouthOpen,
    /// Pucker the lips.
    Kiss,
    /// Puff out the cheeks.
    CheeksPuff,
    /// Smile on the performer's left side only.
    SmileLeft,
    /// Smile on the performer's right side only.
    SmileRight,
    /// Close the eyes.
    CloseEyes,
    /// Look left, right, up, down.
    LookAround,
}

impl CalibrationPrompt {
    /// The whole routine, in the order it is shown.
    pub const ROUTINE: [CalibrationPrompt; 13] = [
        Self::Rest,
        Self::BigSmile,
        Self::BrowsUp,
        Self::BrowsDown,
        Self::EyesWide,
        Self::Squint,
        Self::MouthOpen,
        Self::Kiss,
        Self::CheeksPuff,
        Self::SmileLeft,
        Self::SmileRight,
        Self::CloseEyes,
        Self::LookAround,
    ];

    /// How long the UI shows the prompt, seconds. The first half second is the person reacting; calibrators drop it.
    pub const fn seconds(self) -> f32 {
        match self {
            Self::Rest | Self::LookAround => 4.0,
            Self::SmileLeft | Self::SmileRight | Self::CloseEyes => 2.0,
            _ => 3.0,
        }
    }

    /// True for the prompt that measures rest.
    pub const fn is_rest(self) -> bool {
        matches!(self, Self::Rest)
    }
}

/// Why a calibration could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalibrationError {
    /// Too few tracked frames in the rest prompt (face not found, or the prompt was skipped).
    #[error("only {got} tracked frames in the rest prompt, need {need}: is the face visible?")]
    NotEnoughRest {
        /// Frames collected.
        got: usize,
        /// Frames needed.
        need: usize,
    },
}

/// A finished calibration and what the routine could not measure.
#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationOutcome {
    /// The ranges and rest pose.
    pub calibration: Calibration,
    /// Prompts with too few tracked frames; their channels keep the rest-only range.
    pub skipped: Vec<CalibrationPrompt>,
}

/// Collects frames during the guided routine and fits a [`Calibration`] (cold path: allocation is fine).
///
/// The fit, as in the prototype: `lo` is the **median** of each channel at rest; `hi` is the **95th percentile** over
/// the prompts, taken per prompt and then the largest, because a channel only fires in its own prompt (3 seconds in
/// 38) and pooling them would dilute its percentile down to noise. `hi` is never closer to `lo` than
/// [`MIN_FIT_SPAN`]. The first half second of every prompt is dropped (the person is still reacting), and a prompt
/// with fewer than 5 tracked frames is reported as skipped.
pub trait Calibrator: Send {
    /// Adds a tracked frame captured while `prompt` was showing. Lost frames are ignored.
    fn push(&mut self, prompt: CalibrationPrompt, frame: &TrackFrame);

    /// Fits the calibration from everything pushed so far.
    ///
    /// # Errors
    /// [`CalibrationError::NotEnoughRest`] if the rest prompt has too few tracked frames.
    fn finish(&mut self) -> Result<CalibrationOutcome, CalibrationError>;
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // tests compare values that are copied or exactly computed, and cast small loop counters
mod tests {
    use omuzaki_core::time::Seconds;

    use super::*;

    #[test]
    fn range_normalizes_clamps_and_never_amplifies_noise() {
        let r = ChannelRange { lo: 0.1, hi: 0.5 };
        assert_eq!(r.normalize(0.1), 0.0);
        assert!((r.normalize(0.3) - 0.5).abs() < 1e-6);
        assert_eq!((r.normalize(0.9), r.normalize(-1.0)), (1.0, 0.0));
        let narrow = ChannelRange { lo: 0.2, hi: 0.21 };
        assert!((narrow.normalize(0.225) - 0.5).abs() < 1e-6, "span is floored at MIN_SPAN");
        assert!((ChannelRange::from_gain(2.0).normalize(0.25) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rest_at_moves_lo_and_keeps_the_span() {
        let mut cal = Calibration::from_gains(DeviceProfile::Webcam, |_| 2.0);
        let mut f = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Webcam);
        f.set(Channel::MOUTH_SMILE_LEFT, 0.1);
        f.head = Some(HeadPose { yaw: 0.2, ..HeadPose::default() });
        cal.rest_at(&f);
        let r = cal.range(Channel::MOUTH_SMILE_LEFT);
        assert!((r.lo - 0.1).abs() < 1e-6 && (r.hi - 0.6).abs() < 1e-6);
        assert_eq!(cal.range(Channel::JAW_OPEN), ChannelRange::from_gain(2.0), "absent channels are untouched");
        assert_eq!(cal.head_rest.yaw, 0.2);
        assert_eq!(cal.range(Channel::HEAD_YAW), ChannelRange::IDENTITY);
    }

    #[test]
    fn calibration_serde_is_named_and_round_trips() {
        let mut cal = Calibration::identity(DeviceProfile::Arkit);
        cal.set_range(Channel::JAW_OPEN, ChannelRange { lo: 0.05, hi: 0.7 });
        let json = serde_json::to_string(&cal).unwrap();
        assert!(json.contains("\"jawOpen\":{\"lo\":0.05,\"hi\":0.7}"), "{json}");
        assert_eq!(serde_json::from_str::<Calibration>(&json).unwrap(), cal);
    }

    #[test]
    fn built_in_response_only_shapes_blinks() {
        let blink = Response::built_in(Channel::EYE_BLINK_LEFT);
        assert_eq!(blink.apply(0.2, 1.0), 0.0, "looking down doesn't half-close the eye");
        assert_eq!(blink.apply(0.9, 1.0), 1.0);
        assert!((blink.apply(0.55, 1.0) - 0.5).abs() < 1e-6);
        assert_eq!(Response::built_in(Channel::JAW_OPEN), Response::default());
    }

    #[test]
    fn exaggeration_lifts_small_values_but_keeps_the_ends() {
        let r = Response::default();
        assert!((r.apply(0.25, 2.0) - 0.5).abs() < 1e-6, "sqrt");
        assert_eq!((r.apply(0.0, 2.0), r.apply(1.0, 2.0)), (0.0, 1.0));
        assert!((r.apply(0.25, 0.5) - 0.25).abs() < 1e-6, "an amount below 1 is not a reduction");
        assert_eq!(Response { invert: true, ..r }.apply(0.0, 1.0), 1.0);
    }

    #[test]
    fn exaggeration_never_touches_literal_channels() {
        for literal in [Channel::EYE_BLINK_LEFT, Channel::EYE_LOOK_IN_LEFT, Channel::JAW_LEFT, Channel::JAW_OPEN] {
            assert!(!EXAGGERATED.contains(literal), "{literal}");
        }
        assert_eq!(EXAGGERATED.count(), 22);
        assert!(FAST.contains(Channel::JAW_OPEN) && FAST.contains(Channel::EYE_BLINK_RIGHT));
        assert_eq!(FAST.count(), 11);
    }

    #[test]
    fn smoothing_dial_matches_the_prototype() {
        assert_eq!(OneEuroParams::for_smoothing(0.0, false), None);
        let p = OneEuroParams::for_smoothing(0.4, false).unwrap();
        assert!((p.min_cutoff - 3.9).abs() < 1e-5 && p.beta == 0.9);
        let fast = OneEuroParams::for_smoothing(0.4, true).unwrap();
        assert!((fast.min_cutoff - 3.9 * 2.6).abs() < 1e-4 && fast.beta == 1.2);
        let smoother = OneEuroParams::for_smoothing(1.0, false).unwrap();
        assert!(smoother.min_cutoff < p.min_cutoff);
    }

    #[test]
    fn routine_is_about_forty_seconds() {
        let total: f32 = CalibrationPrompt::ROUTINE.iter().map(|p| p.seconds()).sum();
        assert_eq!(total, 38.0);
        assert!(CalibrationPrompt::ROUTINE[0].is_rest());
    }

    #[test]
    fn config_defaults_survive_a_partial_file() {
        let cfg: RetargetConfig = serde_json::from_str(
            r#"{"mirror": true, "responses": {"jawOpen": {"dead": 0.1, "sat": 1.0, "gamma": 1.0, "invert": false}}}"#,
        )
        .unwrap();
        assert!(cfg.mirror && cfg.smoothing == 0.4 && cfg.responses.len() == 1);
        assert_eq!(RetargetConfig::default().lost, LostPolicy { grace_s: 0.25, fade_s: 0.5 });
    }
}
