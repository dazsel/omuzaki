//! Damped springs: how every life channel chases its target, so everything has momentum instead of fixed easing.

use std::f32::consts::TAU;

use omuzaki_anim_api::{Dynamics, LifeDynamics};

/// Longest sub-step, seconds (240 Hz). Together with [`STABLE_OMEGA_H`] it keeps the integrator stable.
const MAX_SUBSTEP: f32 = 1.0 / 240.0;
/// Largest `omega * h` the semi-implicit Euler integrator is run at: with damping up to [`MAX_DAMPING`] it stays
/// inside the stability bound (`a < -2z + 2 sqrt(z^2 + 1)`, which is 0.47 at `z = 2`).
const STABLE_OMEGA_H: f32 = 0.4;
/// Springs faster than this are clamped, so a wild setting cannot make the sub-step count explode.
const MAX_FREQ_HZ: f32 = 30.0;
/// Damping ratios above this are clamped (the bound above assumes it).
const MAX_DAMPING: f32 = 2.0;
/// One side's spring runs this much faster and the other this much slower (the prototype's 8%), so a symmetric move
/// still has one side arriving first.
pub(super) const SIDE_SKEW: f32 = 0.08;

/// How one frame is cut into sub-steps. Shared by every spring in the frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Substeps {
    n: u32,
    h: f32,
}

impl Substeps {
    /// Sub-steps of at most 1/240 s, and fewer than [`STABLE_OMEGA_H`] radians per step for the fastest spring.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small, positive, ceil-ed count
    pub fn plan(dt: f32, dynamics: &LifeDynamics) -> Self {
        let fastest =
            [dynamics.eye, dynamics.mouth, dynamics.brow, dynamics.head, dynamics.lean, dynamics.gaze, dynamics.slow]
                .iter()
                .map(|d| d.freq_hz)
                .fold(0.0_f32, |m, f| if f.is_finite() { m.max(f) } else { m });
        let omega = TAU * fastest.min(MAX_FREQ_HZ) * (1.0 + SIDE_SKEW);
        let longest = (STABLE_OMEGA_H / omega.max(1e-3)).min(MAX_SUBSTEP);
        // The small slack keeps `1/60 * 240` from rounding up to five steps.
        let n = (dt / longest - 1e-3).ceil().max(1.0) as u32;
        #[allow(clippy::cast_precision_loss)] // at most a few hundred
        let h = dt / n as f32;
        Self { n, h }
    }
}

/// A spring's position and velocity. It starts at its first target, so there is no start-up transient.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(super) struct Spring {
    x: f32,
    v: f32,
    primed: bool,
}

impl Spring {
    /// Where the spring is, or `fallback` before its first step.
    pub fn pos(&self, fallback: f32) -> f32 {
        if self.primed { self.x } else { fallback }
    }

    /// Puts the spring at `target` at rest. A person drives the channel: the spring follows without lag, so handing
    /// back has no jump.
    pub fn snap(&mut self, target: f32) -> f32 {
        let target = if target.is_finite() { target } else { 0.0 };
        *self = Self { x: target, v: 0.0, primed: true };
        target
    }

    /// Snaps to `target` when `follow` is false (a person drives the channel), else chases it. Returns the position.
    pub fn track(&mut self, target: f32, follow: bool, dynamics: Dynamics, freq_scale: f32, sub: Substeps) -> f32 {
        if follow { self.chase(target, dynamics, freq_scale, sub) } else { self.snap(target) }
    }

    /// Chases `target` with the spring's natural frequency scaled by `freq_scale`.
    pub fn chase(&mut self, target: f32, dynamics: Dynamics, freq_scale: f32, sub: Substeps) -> f32 {
        if !self.primed || !target.is_finite() || !(self.x + self.v).is_finite() {
            return self.snap(target);
        }
        // No usable frequency means no dynamics: follow the target exactly.
        if !(dynamics.freq_hz > 0.0 && freq_scale > 0.0) {
            return self.snap(target);
        }
        let omega = TAU * dynamics.freq_hz.min(MAX_FREQ_HZ) * freq_scale;
        let stiffness = omega * omega;
        let damping = if dynamics.damping.is_nan() { 1.0 } else { dynamics.damping.clamp(0.0, MAX_DAMPING) };
        let friction = 2.0 * damping * omega;
        // Semi-implicit Euler (velocity first): stable and accurate at the sub-step sizes `Substeps` picks.
        for _ in 0..sub.n {
            self.v += (stiffness * (target - self.x) - friction * self.v) * sub.h;
            self.x += self.v * sub.h;
        }
        self.x
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // exact values copied through; small positive frame counts
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn run(dynamics: Dynamics, seconds: f32, dt: f32) -> Vec<f32> {
        let sub = Substeps::plan(dt, &LifeDynamics { eye: dynamics, ..LifeDynamics::default() });
        let mut s = Spring::default();
        s.snap(0.0);
        (0..(seconds / dt) as u32).map(|_| s.chase(1.0, dynamics, 1.0, sub)).collect()
    }

    #[test]
    fn starts_at_its_first_target_without_a_transient() {
        let mut s = Spring::default();
        let sub = Substeps::plan(DT, &LifeDynamics::default());
        assert_eq!(s.pos(9.0), 9.0);
        assert_eq!(s.chase(0.4, Dynamics { freq_hz: 5.0, damping: 0.7 }, 1.0, sub), 0.4);
        assert_eq!(s.pos(9.0), 0.4);
    }

    #[test]
    fn critically_damped_settles_without_overshoot() {
        let x = run(Dynamics { freq_hz: 3.0, damping: 1.0 }, 3.0, DT);
        assert!(x.iter().all(|&v| v <= 1.0 + 1e-4), "no overshoot");
        assert!((x[x.len() - 1] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn the_head_spring_overshoots_about_twelve_percent() {
        // zeta 0.55: exp(-pi z / sqrt(1 - z^2)) = 12.3%, the prototype's measured step response.
        let x = run(Dynamics { freq_hz: 2.5, damping: 0.55 }, 3.0, DT);
        let peak = x.iter().fold(0.0_f32, |m, &v| m.max(v));
        assert!((peak - 1.12).abs() < 0.02, "peak {peak}");
        assert!((x[x.len() - 1] - 1.0).abs() < 5e-3);
    }

    #[test]
    fn frame_rate_does_not_change_the_response_much() {
        let d = Dynamics { freq_hz: 12.0, damping: 0.9 };
        for dt in [1.0 / 15.0, 1.0 / 30.0, 1.0 / 120.0] {
            let x = run(d, 1.0, dt);
            let v = x[(0.2 / dt) as usize];
            let reference = run(d, 1.0, 1.0 / 240.0)[48];
            assert!((v - reference).abs() < 0.05, "dt {dt}: {v} vs {reference}");
            assert!(x.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn stays_stable_at_absurd_settings() {
        let wild = Dynamics { freq_hz: 5000.0, damping: 50.0 };
        let x = run(wild, 2.0, 0.25);
        assert!(x.iter().all(|v| v.is_finite() && v.abs() < 3.0), "{x:?}");
        let nan = Dynamics { freq_hz: f32::NAN, damping: f32::NAN };
        assert!(run(nan, 1.0, DT).iter().all(|v| v.is_finite()));
    }

    #[test]
    fn snap_and_non_finite_targets_recover() {
        let mut s = Spring::default();
        let d = Dynamics { freq_hz: 8.0, damping: 0.75 };
        let sub = Substeps::plan(DT, &LifeDynamics::default());
        s.chase(1.0, d, 1.0, sub);
        assert_eq!(s.track(0.3, false, d, 1.0, sub), 0.3, "a person drives it: follow exactly");
        assert_eq!(s.chase(f32::NAN, d, 1.0, sub), 0.0, "a bad target resets to a finite value");
        assert_eq!(s.snap(f32::INFINITY), 0.0);
    }

    #[test]
    fn substeps_match_the_prototype_at_default_settings() {
        let sub = Substeps::plan(1.0 / 60.0, &LifeDynamics::default());
        assert_eq!(sub.n, 4);
        assert!((sub.h - 1.0 / 240.0).abs() < 1e-6);
        assert_eq!(Substeps::plan(0.25, &LifeDynamics::default()).n, 60);
        assert_eq!(Substeps::plan(0.001, &LifeDynamics::default()).n, 1);
    }
}
