//! Gaze: micro-saccades, rare glances away, fixation drift, and the lead on a head turn.
//!
//! The behaviours produce an **offset** in look units. It chases through a stiff spring (a saccade lands in about
//! 40 ms) and is added to wherever the incoming bus already looks.

use std::f32::consts::TAU;

use omuzaki_anim_api::{Channel, GazeParams};
use omuzaki_core::rng::Rng;

use super::frame::Frame;
use super::noise::Texture;
use super::spring::Spring;
use super::write::Out;

/// The first glance comes sooner than the usual gap: this fraction of a normal draw.
const FIRST_GLANCE: f32 = 0.4;
/// A glance is not started while the eyes lead a head turn; it is retried after this long, seconds.
const GLANCE_RETRY_S: f64 = 0.5;
/// A steered-back saccade aims at the centre with this much random spread, radians.
const STEER_SPREAD: f32 = 1.2;
/// The gaze offset never exceeds this, look units.
const REACH: f32 = 0.8;
/// Fixation drift: noise plus a slow sine per axis (frequency in Hz, sine amplitude as a fraction of the drift).
/// A sine on top keeps every half second moving: noise alone parks at its extremes.
const DRIFT_X: (f32, f32, f32) = (0.83, 0.6, 0.7);
const DRIFT_Y: (f32, f32, f32) = (1.07, 0.5, 0.8);

/// Where the eyes are wandering to, and the timers that decide when they move.
#[derive(Clone, Debug)]
pub(super) struct Wander {
    /// The current micro-saccade position, steered back toward the centre.
    rest: (f32, f32),
    next_saccade: f64,
    glancing: bool,
    glance: (f32, f32),
    glance_until: f64,
    next_glance: f64,
    x: Spring,
    y: Spring,
}

/// What the gaze did this frame.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(super) struct GazeOut {
    /// True on the frame a glance starts; a big gaze move earns a blink.
    pub glance_started: bool,
}

impl Wander {
    /// The first timers are drawn from `rng` (the gaze stream).
    pub fn new(params: &GazeParams, rng: &mut Rng) -> Self {
        Self {
            rest: (0.0, 0.0),
            next_saccade: f64::from(params.saccade_gap_s.sample(rng.next_f32())),
            glancing: false,
            glance: (0.0, 0.0),
            glance_until: 0.0,
            next_glance: f64::from(params.glance_gap_s.sample(rng.next_f32()) * FIRST_GLANCE),
            x: Spring::default(),
            y: Spring::default(),
        }
    }

    /// `lead` is the offset the eyes take toward a turning head; `leading` says a lead is under way.
    pub fn step(
        &mut self,
        frame: &Frame<'_>,
        rng: &mut Rng,
        lead: (f32, f32),
        leading: bool,
        out: &mut Out<'_>,
    ) -> GazeOut {
        let free = frame.gate.gaze;
        let mut result = GazeOut::default();
        let mut target = (0.0, 0.0);
        if free {
            if frame.systems.gaze {
                result.glance_started = self.advance(frame, rng, leading);
                target = if self.glancing { self.glance } else { self.rest };
            }
            target = (target.0 + lead.0, target.1 + lead.1);
        } else {
            self.glancing = false;
        }
        let (dynamics, sub) = (frame.params.springs.gaze, frame.sub);
        let reach = |v: f32| v.clamp(-REACH, REACH);
        let offset = (
            self.x.track(reach(target.0), free, dynamics, 1.0, sub),
            self.y.track(reach(target.1), free, dynamics, 1.0, sub),
        );
        if free {
            Self::write(frame, offset, out);
        }
        result
    }

    /// Runs the glance and saccade timers. Returns true if a glance started.
    fn advance(&mut self, frame: &Frame<'_>, rng: &mut Rng, leading: bool) -> bool {
        let (params, clock) = (&frame.params.gaze, frame.clock);
        let mut glance_started = false;
        if self.glancing && clock >= self.glance_until {
            self.glancing = false;
            self.next_glance = clock + f64::from(params.glance_gap_s.sample(rng.next_f32()));
            self.next_saccade = clock + f64::from(params.saccade_gap_s.sample(rng.next_f32()));
        }
        if !self.glancing && clock >= self.next_glance {
            if leading {
                self.next_glance = clock + GLANCE_RETRY_S;
            } else {
                let side = if rng.next_f32() < 0.5 { -1.0 } else { 1.0 };
                self.glance = (side * params.glance_x.sample(rng.next_f32()), params.glance_y.sample(rng.next_f32()));
                self.glance_until = clock + f64::from(params.glance_hold_s.sample(rng.next_f32()));
                self.glancing = true;
                glance_started = true;
            }
        }
        if !self.glancing && clock >= self.next_saccade {
            self.hop(params, rng);
            self.next_saccade = clock + f64::from(params.saccade_gap_s.sample(rng.next_f32()));
        }
        glance_started
    }

    /// A micro-saccade: a small hop in a random direction, steered back toward the centre if it would wander off.
    fn hop(&mut self, params: &GazeParams, rng: &mut Rng) {
        let size = params.saccade_size.sample(rng.next_f32());
        let aim = |angle: f32, from: (f32, f32)| (from.0 + size * angle.cos(), from.1 + size * angle.sin());
        let mut next = aim(rng.next_f32() * TAU, self.rest);
        if next.0.hypot(next.1) > params.saccade_radius {
            let home = (-self.rest.1).atan2(-self.rest.0);
            next = aim(home + (rng.next_f32() - 0.5) * STEER_SPREAD, self.rest);
        }
        self.rest = next;
    }

    /// Adds the offset, plus fixation drift, to every `eyeLook*` channel by their signed pairs, so each eye keeps its
    /// own convergence.
    fn write(frame: &Frame<'_>, offset: (f32, f32), out: &mut Out<'_>) {
        let drift = |texture: Texture, (hz, sine, period): (f32, f32, f32), phase: f32| {
            if frame.systems.gaze {
                let wobble = frame.noise(texture, period, 0.0) + sine * frame.slow_sine(hz, phase);
                frame.params.gaze.drift * wobble
            } else {
                0.0
            }
        };
        let phase = frame.traits.drift_phase;
        let dx = offset.0 + drift(Texture::GazeX, DRIFT_X, phase[0]);
        let dy = offset.1 + drift(Texture::GazeY, DRIFT_Y, phase[1]);
        // Looking toward the character's left turns the left eye out and the right eye in.
        out.nudge(Channel::EYE_LOOK_OUT_LEFT, Channel::EYE_LOOK_IN_LEFT, dx);
        out.nudge(Channel::EYE_LOOK_IN_RIGHT, Channel::EYE_LOOK_OUT_RIGHT, dx);
        out.nudge(Channel::EYE_LOOK_UP_LEFT, Channel::EYE_LOOK_DOWN_LEFT, dy);
        out.nudge(Channel::EYE_LOOK_UP_RIGHT, Channel::EYE_LOOK_DOWN_RIGHT, dy);
    }
}
