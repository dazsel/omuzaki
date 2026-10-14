//! The `ARKit`-52 retarget: raw tracker frames to calibrated, smoothed bus channels (spec PF-20 to PF-23, FE-06), ported
//! from the prototype's `arkit.js`.
//!
//! [`ArkitRetarget`] implements the contract's [`Retarget`] trait. Per `ARKit` channel it does what the contract
//! documents, in this order: normalise by the performer's [`ChannelRange`](omuzaki_anim_api::ChannelRange), shape with the
//! channel's [`Response`] (the built-in blink dead zone, exaggeration on the [`EXAGGERATED`](omuzaki_anim_api::EXAGGERATED)
//! channels), smooth with a One-Euro filter ([`OneEuro`]; the [`FAST`](omuzaki_anim_api::FAST) family for blinks, gaze and
//! jaw), then mirror if asked. Head angles, shift and lean come from the frame's head pose (see `head`) and are held to
//! their channels' legal ranges.
//!
//! The built-in blink dead zone has the prototype's two windows: `0.28..0.8` for a channel still on its default
//! (gain-derived) range, `0.25..0.85` once the performer has a fitted range for it. See [`UNCALIBRATED_BLINK`].
//!
//! Everything that belongs to the *tracker's* channel (its range, response, exaggeration and filter) is applied before
//! mirroring and travels with the value, so a calibration made on the raw tracker stays valid when the preview is
//! mirrored.
//!
//! What moved out of this stage into the face painter (anim-api §3): the prototype's mouth, eye and brow formulas
//! (`mSmile`, `browY`, ...). What is not here either: the `winkArc` hysteresis, which is the grammar stage (FE-05).
//!
//! Hot path: [`Retarget::apply`] touches fixed arrays only. It never allocates, locks or reads a clock; a benchmark
//! (`benches/retarget.rs`) and a counting-allocator test (`tests/retarget_alloc.rs`) hold it to that.

mod filter;
mod gains;
mod head;

use omuzaki_anim_api::{
    ARKIT_COUNT, BlendMode, Calibration, Channel, ChannelBuf, ChannelMask, ChannelRange, DeviceProfile, EXAGGERATED,
    EyeAngles, FAST, Fade, LOOK_FULL_RAD, Layer, OneEuroParams, Response, Retarget, RetargetConfig, TrackFrame,
    TrackStatus,
};

pub use filter::OneEuro;
pub use gains::{default_calibration, gain};

use head::{HeadChannels, HeadTuning};

/// The blink window of a channel that is still on its default range: the prototype's `blinkLo`/`blinkHi` (`0.28..0.8`),
/// used by its gain path (a neutral map and the gain table). The window of a fitted range (`0.25..0.85`) is
/// [`Response::built_in`]. The prototype tuned the two paths apart, so this keeps them apart.
const UNCALIBRATED_BLINK: Response = Response { dead: 0.28, sat: 0.8, gamma: 1.0, invert: false };

/// How far a range's span may be from the default span and still count as the default. "Calibrate neutral"
/// ([`Calibration::rest_at`]) moves `lo` and recomputes `hi` from it, which costs float rounding in the span.
const SPAN_TOLERANCE: f32 = 1e-5;

/// [`RetargetConfig`] compiled into fixed per-channel tables, so a frame does table lookups instead of map searches.
#[derive(Clone, Debug)]
struct Plan {
    /// By tracker channel: the response after the range.
    response: [Response; ARKIT_COUNT],
    /// By tracker channel: the exaggeration amount (`1` for channels that stay literal).
    boost: [f32; ARKIT_COUNT],
    /// By tracker channel: the smoothing, `None` when the dial is at zero.
    filter: [Option<OneEuroParams>; ARKIT_COUNT],
    head: HeadTuning,
}

impl Plan {
    fn compile(config: &RetargetConfig, calibration: &Calibration) -> Self {
        let mut plan = Self {
            response: [Response::default(); ARKIT_COUNT],
            boost: [1.0; ARKIT_COUNT],
            filter: [None; ARKIT_COUNT],
            head: HeadTuning {
                gain: config.head_gain,
                lean_per_metre: config.lean_per_metre,
                filter: OneEuroParams::for_smoothing(config.smoothing, false),
            },
        };
        let default = default_calibration(calibration.profile);
        for info in Channel::arkit() {
            let (c, i) = (info.channel, info.channel.index());
            plan.response[i] = config
                .responses
                .get(&c)
                .copied()
                .unwrap_or_else(|| built_in_response(c, calibration.range(c), default.range(c)));
            if EXAGGERATED.contains(c) {
                plan.boost[i] = config.exaggeration;
            }
            plan.filter[i] = OneEuroParams::for_smoothing(config.smoothing, FAST.contains(c));
        }
        plan
    }
}

/// The response of `channel` when the settings give it none. Only blinks differ from the identity, and they have two
/// windows (see [`UNCALIBRATED_BLINK`]): a range whose span is the profile's default one has not been fitted, however
/// far "calibrate neutral" has moved its `lo`. (The contract has no flag for a fitted range, so the span is the tell.)
fn built_in_response(channel: Channel, range: ChannelRange, default: ChannelRange) -> Response {
    let span = |r: ChannelRange| r.hi - r.lo;
    let is_blink = matches!(channel, Channel::EYE_BLINK_LEFT | Channel::EYE_BLINK_RIGHT);
    if is_blink && (span(range) - span(default)).abs() <= SPAN_TOLERANCE {
        UNCALIBRATED_BLINK
    } else {
        Response::built_in(channel)
    }
}

/// The `ARKit`-52 retarget. One per performer; it owns its filters, calibration and settings.
///
/// Build it with [`ArkitRetarget::new`] (default calibration for the device) or [`ArkitRetarget::with`], then call
/// [`Retarget::apply`] once per tracker frame. Each stage and the way a lost face is handled are described on
/// [`Retarget::apply`] and in the module docs.
#[derive(Clone, Debug)]
pub struct ArkitRetarget {
    calibration: Calibration,
    config: RetargetConfig,
    plan: Plan,
    /// One filter per tracker channel.
    blend_filters: [OneEuro; ARKIT_COUNT],
    head: HeadChannels,
    /// The output of the last tracked frame, kept so a lost face fades from where it was.
    held: ChannelBuf,
    held_mask: ChannelMask,
    /// The layer weight: `1` while tracking, fading to `0` once the face has been lost for longer than the grace.
    weight: Fade,
    /// Seconds the face has been lost for.
    lost_for: f32,
}

impl ArkitRetarget {
    /// A retarget with the [default calibration](default_calibration) for `profile` and default settings.
    pub fn new(profile: DeviceProfile) -> Self {
        Self::with(default_calibration(profile), RetargetConfig::default())
    }

    /// A retarget with the given calibration and settings.
    pub fn with(calibration: Calibration, config: RetargetConfig) -> Self {
        Self {
            plan: Plan::compile(&config, &calibration),
            calibration,
            config,
            blend_filters: [OneEuro::default(); ARKIT_COUNT],
            head: HeadChannels::default(),
            held: ChannelBuf::NEUTRAL,
            held_mask: ChannelMask::EMPTY,
            weight: Fade(1.0),
            lost_for: 0.0,
        }
    }

    /// Processes a tracked frame into `held` and `held_mask`.
    fn track(&mut self, frame: &TrackFrame, dt: f32) {
        let mut values = ChannelBuf::NEUTRAL;
        let mut mask = frame.present & ChannelMask::ARKIT;
        for c in mask {
            let i = c.index();
            let normalised = self.calibration.range(c).normalize(frame.blend[i]);
            // A NaN reads as rest, as in the prototype: `f32::clamp` would pass it on to the response and the filter.
            let normalised = if normalised.is_nan() { 0.0 } else { normalised };
            let shaped = self.plan.response[i].apply(normalised, self.plan.boost[i]);
            values[c] = self.blend_filters[i].filter(self.plan.filter[i], shaped, dt).clamp(0.0, 1.0);
        }
        if let Some(head) = frame.head {
            mask |= self.head.apply(head, self.calibration.head_rest, self.plan.head, dt, &mut values);
        }
        if let Some(eyes) = frame.eyes
            && !frame.present.intersects(ChannelMask::GAZE)
        {
            self.gaze_from_angles(eyes, dt, &mut values);
            mask |= ChannelMask::GAZE;
        }
        if self.config.mirror {
            values.mirror();
            mask = mask.mirrored();
        }
        (self.held, self.held_mask) = (values, mask);
    }

    /// The eight `eyeLook*` channels from eye angles, smoothed like blendshape gaze.
    fn gaze_from_angles(&mut self, eyes: EyeAngles, dt: f32, values: &mut ChannelBuf) {
        // Both eyes' mean turn toward image-right is a look toward the character's left; pitch is down-positive.
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let x = f32::midpoint(finite(eyes.left_yaw), finite(eyes.right_yaw)) / LOOK_FULL_RAD;
        let y = -f32::midpoint(finite(eyes.left_pitch), finite(eyes.right_pitch)) / LOOK_FULL_RAD;
        let mut look = ChannelBuf::NEUTRAL;
        look.set_gaze(x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));
        for c in ChannelMask::GAZE {
            let i = c.index();
            values[c] = self.blend_filters[i].filter(self.plan.filter[i], look[c], dt).clamp(0.0, 1.0);
        }
    }

    /// Fades the held output toward rest and the layer weight toward zero once the face has been lost for longer than
    /// the grace period. Filters stay untouched until the fade is complete, then restart, so a returning face is not
    /// dragged toward where it was long ago.
    fn lose(&mut self, dt: f32, out: &mut Layer) {
        let lost = self.config.lost;
        self.lost_for += dt.max(0.0);
        out.values = self.held;
        if self.lost_for <= lost.grace_s {
            return;
        }
        let past_grace = self.lost_for - lost.grace_s;
        let progress = (past_grace / lost.fade_s.max(f32::EPSILON)).clamp(0.0, 1.0);
        for c in self.held_mask {
            out.values[c] *= 1.0 - progress;
        }
        // Only the part of this frame after the grace period counts toward the fade, so a long frame that crosses the
        // boundary does not start the fade early.
        self.weight.toward(0.0, past_grace.min(dt), lost.fade_s);
        if progress >= 1.0 {
            self.reset_filters();
        }
    }

    fn reset_filters(&mut self) {
        self.blend_filters = [OneEuro::default(); ARKIT_COUNT];
        self.head.reset();
    }
}

impl Retarget for ArkitRetarget {
    fn apply(&mut self, frame: &TrackFrame, dt: f32, out: &mut Layer) {
        match frame.status {
            TrackStatus::Tracking => {
                self.lost_for = 0.0;
                self.track(frame, dt);
                self.weight.toward(1.0, dt, self.config.lost.fade_s);
                out.values = self.held;
            }
            TrackStatus::Lost => self.lose(dt, out),
        }
        out.mask = self.held_mask;
        out.weight = self.weight.0;
        out.mode = BlendMode::Override;
    }

    fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    /// Keeps the filter state, like [`set_config`](Self::set_config). The blink window follows the new ranges.
    fn set_calibration(&mut self, calibration: Calibration) {
        self.plan = Plan::compile(&self.config, &calibration);
        self.calibration = calibration;
    }

    fn config(&self) -> &RetargetConfig {
        &self.config
    }

    /// Keeps the filter state, so moving the smoothing dial does not make the face jump. (The prototype restarted its
    /// filters, which showed as a one-frame snap.) Turning smoothing off drops the state, so turning it back on starts
    /// clean instead of from a stale value.
    fn set_config(&mut self, config: RetargetConfig) {
        self.plan = Plan::compile(&config, &self.calibration);
        if self.plan.head.filter.is_none() {
            self.reset_filters();
        }
        self.config = config;
    }

    fn reset(&mut self) {
        self.reset_filters();
        (self.held, self.held_mask, self.weight, self.lost_for) =
            (ChannelBuf::NEUTRAL, ChannelMask::EMPTY, Fade(1.0), 0.0);
    }
}

#[cfg(test)]
mod tests;
