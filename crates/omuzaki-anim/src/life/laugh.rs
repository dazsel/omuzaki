//! Laugh detection: a big open-mouthed loud smile makes the eyes squint, the head bob back and, on a really loud one,
//! the eyes close into ^^ arcs.

use std::f32::consts::TAU;

use super::frame::{Frame, smoothstep};
use super::noise::Texture;

// The prototype's thresholds. A laugh starts on the first row and must fall below the second for `HOLD_S` to end, so a
// flicker in loudness cannot make it chatter. (They are not in `LifeParams` yet; see the follow-up issue.)
const START_SMILE: f32 = 0.6;
const START_JAW: f32 = 0.3;
const START_LOUD: f32 = 0.45;
const KEEP_SMILE: f32 = 0.45;
const KEEP_JAW: f32 = 0.2;
const KEEP_LOUD: f32 = 0.3;
const HOLD_S: f32 = 0.12;
/// A laugh ramps in and out over this long.
const RAMP_S: f32 = 0.15;
/// Closing into ^^ needs a really loud laugh.
const ARC_LOUD: f32 = 0.85;
/// The bob rate wanders by this fraction of itself.
const RATE_WANDER: f32 = 0.1;

/// The state of the laugh detector.
#[derive(Clone, Debug, Default)]
pub(super) struct Laugh {
    on: bool,
    below_for: f32,
    weight: f32,
    arcs: f32,
    phase: f32,
}

/// What the laugh asks of the rest of the face this frame.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(super) struct LaughOut {
    /// How much of a laugh is showing, `0..=1` (eased).
    pub weight: f32,
    /// How far the eyes have closed into ^^, `0..=1` (eased).
    pub arcs: f32,
    /// The bob's phase, radians; stays at zero while there is no laugh.
    pub phase: f32,
}

impl Laugh {
    /// Updates the detector from the source's mean net smile, its jaw opening and the speech loudness (raised by
    /// audio laughter detection, which only strengthens the case).
    pub fn step(&mut self, f: &Frame<'_>, smile: f32, jaw: f32, loud: f32) -> LaughOut {
        let dt = f.dt;
        if !f.systems.laugh {
            self.on = false;
        } else if !self.on {
            if smile > START_SMILE && jaw > START_JAW && loud > START_LOUD {
                self.on = true;
                self.below_for = 0.0;
            }
        } else {
            let still = smile > KEEP_SMILE && jaw > KEEP_JAW && loud > KEEP_LOUD;
            self.below_for = if still { 0.0 } else { self.below_for + dt };
            if self.below_for > HOLD_S {
                self.on = false;
            }
        }
        let ramp = |value: f32, up: bool| (value + if up { dt } else { -dt } / RAMP_S).clamp(0.0, 1.0);
        self.weight = ramp(self.weight, self.on);
        self.arcs = ramp(self.arcs, self.on && loud >= ARC_LOUD);
        if self.weight > 0.0 {
            let wander = 1.0 + RATE_WANDER * f.noise(Texture::Laugh, 1.3, 0.0);
            self.phase += TAU * f.params.head.laugh_bob_hz * wander * dt;
        } else {
            self.phase = 0.0;
        }
        LaughOut { weight: smoothstep(self.weight), arcs: smoothstep(self.arcs), phase: self.phase }
    }
}
