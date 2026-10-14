//! The One-Euro filter (Casiez et al. 2012): low lag on fast moves, low jitter when still.

use omuzaki_anim_api::OneEuroParams;

/// The state of one One-Euro filter. `Copy` and allocation-free; the parameters are passed in on every call, so one
/// table of [`OneEuroParams`] serves every filter and a settings change takes effect on the next frame without
/// touching the state.
///
/// A direct port of the prototype's `oneEuro` (`arkit.js`), including its guard: a NaN or infinity (a corrupt packet,
/// or an overflow that once drove the speed term to infinity and then NaN) never becomes the filter's memory. The
/// state restarts and the output is `0` (rest) until the next finite sample.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct OneEuro {
    /// The smoothed value.
    x: f32,
    /// The smoothed speed (units per second).
    dx: f32,
    /// False until the first finite sample: that sample passes through unchanged.
    primed: bool,
}

/// Smoothing factor of a first-order low-pass with the given cutoff (Hz) at a step of `dt` seconds.
fn alpha(cutoff: f32, dt: f32) -> f32 {
    1.0 / (1.0 + 1.0 / (std::f32::consts::TAU * cutoff * dt))
}

impl OneEuro {
    /// Forgets the history: the next finite sample passes through unchanged.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Filters `v`, a sample taken `dt` seconds after the previous one. A non-positive `dt` cannot give a speed, so the
    /// sample passes through and the speed restarts at zero.
    pub fn step(&mut self, params: OneEuroParams, v: f32, dt: f32) -> f32 {
        if !v.is_finite() {
            self.reset();
            return 0.0;
        }
        if !self.primed || dt.is_nan() || dt <= 0.0 {
            *self = Self { x: v, dx: 0.0, primed: true };
            return v;
        }
        let speed = (v - self.x) / dt;
        self.dx += alpha(params.d_cutoff, dt) * (speed - self.dx);
        self.x += alpha(params.min_cutoff + params.beta * self.dx.abs(), dt) * (v - self.x);
        if !self.x.is_finite() || !self.dx.is_finite() {
            self.reset();
            return 0.0;
        }
        self.x
    }

    /// [`OneEuro::step`] when smoothing is on (`Some`), otherwise the sample unchanged. Either way the result is finite
    /// (a NaN or infinity reads as `0`), so nothing downstream has to check.
    pub fn filter(&mut self, params: Option<OneEuroParams>, v: f32, dt: f32) -> f32 {
        match params {
            Some(p) => self.step(p, v, dt),
            None if v.is_finite() => v,
            None => 0.0,
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // the first sample and the guard values are exact
mod tests {
    use super::*;

    const PARAMS: OneEuroParams = OneEuroParams { min_cutoff: 3.9, beta: 0.9, d_cutoff: 1.0 };
    const DT: f32 = 1.0 / 30.0;

    #[test]
    fn the_first_sample_passes_through_and_a_constant_stays_put() {
        let mut f = OneEuro::default();
        assert_eq!(f.step(PARAMS, 0.7, DT), 0.7);
        for _ in 0..100 {
            assert!((f.step(PARAMS, 0.7, DT) - 0.7).abs() < 1e-6);
        }
    }

    #[test]
    fn a_step_is_followed_with_lag_and_converges() {
        let mut f = OneEuro::default();
        f.step(PARAMS, 0.0, DT);
        let first = f.step(PARAMS, 1.0, DT);
        assert!(first > 0.0 && first < 1.0, "lags a step: {first}");
        let mut v = first;
        for _ in 0..90 {
            v = f.step(PARAMS, 1.0, DT);
        }
        assert!((v - 1.0).abs() < 1e-3, "converges: {v}");
    }

    #[test]
    fn speed_opens_the_cutoff_so_a_fast_move_lags_less_than_a_slow_one_would() {
        let steady = OneEuroParams { beta: 0.0, ..PARAMS };
        let (mut with_beta, mut without) = (OneEuro::default(), OneEuro::default());
        with_beta.step(PARAMS, 0.0, DT);
        without.step(steady, 0.0, DT);
        let mut gap = (0.0, 0.0);
        for i in 1..=10_u8 {
            let v = f32::from(i) * 0.1; // a steady ramp, 3 units per second
            gap = (v - with_beta.step(PARAMS, v, DT), v - without.step(steady, v, DT));
        }
        assert!(gap.0 < gap.1, "beta closes the gap on a ramp: {gap:?}");
    }

    #[test]
    fn a_non_finite_sample_resets_the_state_and_reads_as_rest() {
        let mut f = OneEuro::default();
        f.step(PARAMS, 0.9, DT);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(f.step(PARAMS, bad, DT), 0.0);
        }
        assert_eq!(f.step(PARAMS, 0.4, DT), 0.4, "restarts from the next finite sample, not from the old state");
    }

    #[test]
    fn an_overflowing_speed_cannot_poison_the_memory() {
        let mut f = OneEuro::default();
        f.step(PARAMS, -3.0e38, DT);
        assert_eq!(f.step(PARAMS, 3.0e38, DT), 0.0, "the speed overflows to infinity: restart at rest");
        assert_eq!(f.step(PARAMS, 0.25, DT), 0.25);
    }

    #[test]
    fn a_useless_dt_passes_the_sample_through() {
        let mut f = OneEuro::default();
        f.step(PARAMS, 0.0, DT);
        for dt in [0.0, -1.0, f32::NAN] {
            assert_eq!(f.step(PARAMS, 0.8, dt), 0.8);
        }
    }

    #[test]
    fn without_params_the_sample_is_unfiltered_but_always_finite() {
        let mut f = OneEuro::default();
        assert_eq!(f.filter(None, 0.3, DT), 0.3);
        assert_eq!(f.filter(None, f32::NAN, DT), 0.0);
        assert_eq!(f.filter(None, f32::INFINITY, DT), 0.0);
        assert!(f.filter(Some(PARAMS), 0.5, DT) == 0.5, "filtering starts fresh: nothing was stored");
    }

    #[test]
    fn reset_forgets() {
        let mut f = OneEuro::default();
        f.step(PARAMS, 1.0, DT);
        f.reset();
        assert_eq!(f, OneEuro::default());
    }
}
