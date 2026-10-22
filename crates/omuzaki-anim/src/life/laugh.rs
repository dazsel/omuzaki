//! Laugh detection: a big open-mouthed loud smile makes the eyes squint, the head bob back and, on a really loud one,
//! the eyes close into ^^ arcs.

use std::f32::consts::TAU;

use super::frame::{Frame, smoothstep};
use super::noise::Texture;

/// The shortest time constant a ramp may have, seconds: it keeps a zero or non-finite `ramp_s` from dividing by zero.
const MIN_RAMP_S: f32 = 1e-3;

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
        let p = &f.params.laugh;
        if !f.systems.laugh {
            self.on = false;
        } else if !self.on {
            if smile > p.start_smile && jaw > p.start_jaw && loud > p.start_loud {
                self.on = true;
                self.below_for = 0.0;
            }
        } else {
            let still = smile > p.keep_smile && jaw > p.keep_jaw && loud > p.keep_loud;
            self.below_for = if still { 0.0 } else { self.below_for + dt };
            if self.below_for > p.hold_s {
                self.on = false;
            }
        }
        let ramp = |value: f32, up: bool| (value + if up { dt } else { -dt } / p.ramp_s.max(MIN_RAMP_S)).clamp(0.0, 1.0);
        self.weight = ramp(self.weight, self.on);
        self.arcs = ramp(self.arcs, self.on && loud >= p.arc_loud);
        if self.weight > 0.0 {
            let wander = 1.0 + p.rate_wander * f.noise(Texture::Laugh, 1.3, 0.0);
            self.phase += TAU * f.params.head.laugh_bob_hz * wander * dt;
        } else {
            self.phase = 0.0;
        }
        LaughOut { weight: smoothstep(self.weight), arcs: smoothstep(self.arcs), phase: self.phase }
    }
}
