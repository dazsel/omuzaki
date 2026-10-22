//! What every behaviour sees of one frame: the incoming bus, the clocks, the cues, and who is allowed to write what.

use omuzaki_anim_api::{ChannelBuf, ChannelMask, LifeParams, LifeSystems, LifeTraits};

use super::noise::{NoiseSeeds, Texture};
use super::spring::{Substeps, side_skew};

/// Which side of the face a channel sits on (the character's own sides, as in `ARKit`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Side {
    Left,
    Right,
    Centre,
}

impl Side {
    /// `+1` for the left, `-1` for the right, `0` on the centre line.
    fn sign(self) -> f32 {
        match self {
            Side::Left => 1.0,
            Side::Right => -1.0,
            Side::Centre => 0.0,
        }
    }
}

/// Which regions are free of a person this frame. A behaviour that needs a whole region is skipped while a person
/// drives any channel of it (the layer's contract, rule 3).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one independent flag per region; a bitset would hide the names
pub(super) struct Gate {
    pub eyes: bool,
    pub gaze: bool,
    pub brows: bool,
    pub mouth: bool,
    pub head: bool,
}

impl Gate {
    pub fn new(taken: ChannelMask) -> Self {
        let free = |region: ChannelMask| !taken.intersects(region);
        Self {
            eyes: free(ChannelMask::EYES),
            gaze: free(ChannelMask::GAZE),
            brows: free(ChannelMask::BROWS),
            mouth: free(ChannelMask::MOUTH),
            head: free(ChannelMask::HEAD),
        }
    }
}

/// How far each region's texture and couplings have eased in since a person last drove it: `0` while driven, rising to
/// `1` over [`FaceParams::hand_back_s`](omuzaki_anim_api::FaceParams::hand_back_s) once free.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Ease {
    pub eyes: f32,
    pub gaze: f32,
    pub brows: f32,
    pub mouth: f32,
    pub head: f32,
}

impl Ease {
    /// Every region fully in, as a layer nobody has driven.
    pub const FULL: Self = Self { eyes: 1.0, gaze: 1.0, brows: 1.0, mouth: 1.0, head: 1.0 };

    /// One frame on: a driven region drops to `0`, a free one rises by `dt / over_s`.
    pub fn advance(&mut self, gate: Gate, dt: f32, over_s: f32) {
        let up = if over_s.is_finite() && over_s > 1e-3 { dt / over_s } else { 1.0 };
        let step = |gain: &mut f32, free: bool| *gain = if free { (*gain + up).min(1.0) } else { 0.0 };
        step(&mut self.eyes, gate.eyes);
        step(&mut self.gaze, gate.gaze);
        step(&mut self.brows, gate.brows);
        step(&mut self.mouth, gate.mouth);
        step(&mut self.head, gate.head);
    }
}

/// Read-only context for one frame.
pub(super) struct Frame<'a> {
    pub params: &'a LifeParams,
    pub systems: LifeSystems,
    pub traits: &'a LifeTraits,
    pub noise: &'a NoiseSeeds,
    /// The incoming bus values (sanitised): the targets everything chases and the base everything rides on.
    pub src: &'a ChannelBuf,
    /// Channels a person drives, which are never written.
    pub taken: ChannelMask,
    pub gate: Gate,
    /// How far each region's texture and couplings have eased in (see [`Ease`]).
    pub ease: Ease,
    /// Whether a loud laugh may close the eyes into ^^ (off when a score owns the arcs).
    pub laugh_arcs: bool,
    /// Seconds since the last frame, capped.
    pub dt: f32,
    /// The layer's own timer: advances by `dt`, so scrubbing the caller's clock cannot derail blinks and glances.
    pub clock: f64,
    /// The caller's clock, which the noise reads so a scrub or a re-render sees the same wobble.
    pub time: f64,
    pub sub: Substeps,
    /// Speech loudness, `0..=1`.
    pub loud: f32,
    /// True on the frame a stressed syllable starts.
    pub onset: bool,
    /// How loud a laugh would be: the speech loudness, raised by audio laughter detection.
    pub laugh_loud: f32,
}

impl Frame<'_> {
    /// Smooth noise in `-1..=1`, varying over about `period` seconds, read on the caller's clock plus `offset`.
    pub fn noise(&self, texture: Texture, period: f32, offset: f64) -> f32 {
        self.noise.sample(texture, self.time + offset, period)
    }

    /// Noise for a texture the `texture` system owns: zero when it is switched off.
    pub fn texture(&self, texture: Texture, period: f32) -> f32 {
        if self.systems.texture { self.noise(texture, period, 0.0) } else { 0.0 }
    }

    /// Texture for one side of the face: the trailing side runs `side_lag_s` behind, and the two amplitudes differ
    /// by up to `side_amp` (both fixed by the seed).
    pub fn sided_texture(&self, texture: Texture, period: f32, side: Side) -> f32 {
        if !self.systems.texture {
            return 0.0;
        }
        let face = &self.params.face;
        let leads = (side == Side::Left) == self.traits.left_leads;
        let lag = if leads || side == Side::Centre { 0.0 } else { -f64::from(self.traits.side_lag_s) };
        let gain = 1.0 + side.sign() * face.side_amp * self.traits.side_amp;
        self.noise(texture, period, lag) * gain
    }

    /// A slow sine on the caller's clock, `-1..=1`. Computed in `f64` so the phase stays exact over a long session.
    #[allow(clippy::cast_possible_truncation)] // a value in -1..=1
    pub fn slow_sine(&self, hz: f32, phase: f32) -> f32 {
        (std::f64::consts::TAU * f64::from(hz) * self.time + f64::from(phase)).sin() as f32
    }

    /// The spring-frequency multiplier of a side: the leading side arrives first. Part of the `texture` system.
    pub fn freq_scale(&self, side: Side) -> f32 {
        if !self.systems.texture {
            return 1.0;
        }
        let lead = if self.traits.left_leads { 1.0 } else { -1.0 };
        1.0 + side_skew(&self.params.springs) * lead * side.sign()
    }
}

/// Hermite smoothstep of `u` clamped to `0..=1`: eases in and out with zero slope at both ends.
pub(super) fn smoothstep(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}
