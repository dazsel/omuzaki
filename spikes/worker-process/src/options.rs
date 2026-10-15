//! What a host run is asked to do, on either platform.

use std::time::Duration;

use crate::Error;

/// What to run.
#[derive(Clone, Debug)]
pub struct HostOptions {
    /// Kills to perform.
    pub kills: u32,
    /// Seed of the kill schedule.
    pub seed: u64,
    /// Frame size.
    pub size: (u32, u32),
    /// Output rate.
    pub fps: f64,
    /// Seconds of undisturbed running before the kills.
    pub baseline_s: u64,
    /// Workers started and stopped one at a time, for cold-start timings no kill can censor.
    pub cold_starts: u32,
    /// Earliest and latest kill, ms after a worker's spawn.
    pub kill_window_ms: (u64, u64),
    /// Extra environment for the worker (for example `MESA_SHADER_CACHE_DISABLE=true`).
    pub worker_env: Vec<(String, String)>,
}

/// The output period for `fps`: finite, above zero and representable as a [`Duration`] of at least one nanosecond.
///
/// # Errors
/// Fails for a rate that is not finite, not positive, so slow the period overflows, or so fast the period rounds to zero.
pub fn period_for(fps: f64) -> Result<Duration, Error> {
    let bad = || Error::Config(format!("--fps {fps} gives no usable output period (need a positive rate below 1e9)"));
    if !(fps.is_finite() && fps > 0.0) {
        return Err(bad());
    }
    match Duration::try_from_secs_f64(1.0 / fps) {
        Ok(period) if !period.is_zero() => Ok(period),
        _ => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_rates_give_their_period() {
        assert_eq!(period_for(30.0).unwrap(), Duration::from_secs_f64(1.0 / 30.0));
        assert_eq!(period_for(1000.0).unwrap(), Duration::from_millis(1));
    }

    #[test]
    fn rates_with_no_usable_period_are_config_errors() {
        // 1e-320 is subnormal: 1 / fps is infinite. 1e300: the period is 1e-300 s, below one nanosecond.
        for fps in [1e-320, 1e300, 0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(period_for(fps), Err(Error::Config(_))), "{fps}");
        }
    }
}
