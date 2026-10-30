//! A damped spring for the body's motion: semi-implicit Euler in sub-steps short enough to stay stable at any frame
//! rate, so a stalled frame (up to the clamp) never makes a joint explode.

use std::f32::consts::TAU;

/// Longest frame step honoured, seconds; a longer stall is taken as this.
const MAX_DT: f32 = 0.1;
/// Longest sub-step, seconds (240 Hz).
const MAX_SUBSTEP: f32 = 1.0 / 240.0;
/// Largest `omega * h`: inside the semi-implicit integrator's stability bound for damping up to 2.
const STABLE_OMEGA_H: f32 = 0.4;
/// The fastest and the most damped spring honoured.
const MAX_FREQ_HZ: f32 = 30.0;
const MAX_DAMPING: f32 = 2.0;

/// How a spring feels: natural frequency in Hz and damping ratio (`1` settles without overshoot).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Feel {
    /// Natural frequency, Hz.
    pub hz: f32,
    /// Damping ratio.
    pub damping: f32,
}

impl Feel {
    /// A feel.
    pub const fn new(hz: f32, damping: f32) -> Self {
        Self { hz, damping }
    }

    /// The same feel `k` times faster.
    #[must_use]
    pub fn faster(self, k: f32) -> Self {
        Self { hz: self.hz * k, ..self }
    }
}

/// A spring's position and velocity. Before its first step it sits at its first target, with no start-up transient.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Spring {
    x: f32,
    v: f32,
    primed: bool,
}

impl Spring {
    /// A spring at rest at `x`.
    pub const fn at(x: f32) -> Self {
        Self { x, v: 0.0, primed: true }
    }

    /// Where it is (`0` before its first step).
    pub fn pos(&self) -> f32 {
        self.x
    }

    /// How fast it moves, units a second.
    pub fn vel(&self) -> f32 {
        self.v
    }

    /// Chases `target` for `dt` seconds with `feel`; returns the new position. A non-finite target or state puts the
    /// spring at the target (or at 0) at rest.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small counts
    pub fn step(&mut self, target: f32, feel: Feel, dt: f32) -> f32 {
        let target = if target.is_finite() { target } else { 0.0 };
        if !self.primed || !(self.x + self.v).is_finite() {
            *self = Self::at(target);
            return target;
        }
        let dt = if dt.is_finite() { dt.clamp(0.0, MAX_DT) } else { 0.0 };
        if dt <= 0.0 {
            return self.x;
        }
        let hz = if feel.hz.is_finite() { feel.hz.clamp(0.01, MAX_FREQ_HZ) } else { 1.0 };
        let zeta = if feel.damping.is_finite() { feel.damping.clamp(0.0, MAX_DAMPING) } else { 1.0 };
        let omega = TAU * hz;
        let longest = (STABLE_OMEGA_H / omega).min(MAX_SUBSTEP);
        let n = (dt / longest - 1e-3).ceil().max(1.0) as u32;
        let h = dt / n as f32;
        for _ in 0..n {
            let a = omega * omega * (target - self.x) - 2.0 * zeta * omega * self.v;
            self.v += a * h;
            self.x += self.v * h;
        }
        self.x
    }

    /// Adds `dv` to the spring's velocity: an impulse. A spring that has not stepped yet starts at 0.
    pub fn kick(&mut self, dv: f32) {
        if !self.primed {
            *self = Self::at(0.0);
        }
        if dv.is_finite() {
            self.v += dv;
        }
    }

    /// Puts the spring at `x`, at rest.
    pub fn snap(&mut self, x: f32) {
        *self = Self::at(if x.is_finite() { x } else { 0.0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settles_on_its_target_and_survives_a_long_stall() {
        let mut s = Spring::at(0.0);
        for _ in 0..240 {
            s.step(1.0, Feel::new(3.0, 0.9), 1.0 / 60.0);
        }
        assert!((s.pos() - 1.0).abs() < 1e-3, "{}", s.pos());
        s.step(-5.0, Feel::new(30.0, 0.1), 10.0);
        assert!(s.pos().is_finite() && s.pos().abs() < 50.0, "{}", s.pos());
    }

    #[test]
    fn an_underdamped_spring_overshoots_and_a_critical_one_does_not() {
        let peak = |damping| {
            let mut s = Spring::at(0.0);
            (0..120).map(|_| s.step(1.0, Feel::new(4.0, damping), 1.0 / 60.0)).fold(0.0_f32, f32::max)
        };
        assert!(peak(0.4) > 1.1);
        assert!(peak(1.0) <= 1.0 + 1e-4);
    }
}
