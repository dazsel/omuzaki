//! Tones, sweeps and clicks.

use std::f64::consts::TAU;

use super::SAMPLE_RATE;

/// A sine of `hz` at `amplitude`, starting at phase 0.
#[must_use]
pub fn sine(len: usize, hz: f64, amplitude: f32) -> Vec<f32> {
    let step = TAU * hz / f64::from(SAMPLE_RATE);
    (0..len).map(|n| amplitude * ((n as f64) * step).sin() as f32).collect()
}

/// How a sweep moves from its first frequency to its last.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Sweep {
    /// Equal Hz per second.
    Linear,
    /// Equal octaves per second.
    Logarithmic,
}

/// A sine sweep from `from_hz` to `to_hz` over `len` samples. The phase is the integral of the frequency, so the sweep
/// has no steps. Both frequencies must be positive for [`Sweep::Logarithmic`].
#[must_use]
pub fn sine_sweep(len: usize, from_hz: f64, to_hz: f64, shape: Sweep, amplitude: f32) -> Vec<f32> {
    let seconds = len as f64 / f64::from(SAMPLE_RATE);
    (0..len)
        .map(|n| {
            let t = n as f64 / f64::from(SAMPLE_RATE);
            let phase = match shape {
                Sweep::Linear => TAU * (from_hz * t + 0.5 * (to_hz - from_hz) * t * t / seconds),
                Sweep::Logarithmic => {
                    let k = (to_hz / from_hz).ln() / seconds;
                    TAU * from_hz * ((k * t).exp() - 1.0) / k
                }
            };
            amplitude * phase.sin() as f32
        })
        .collect()
}

/// Silence of `len` samples with a single `1.0` at `at` (silence when `at >= len`).
#[must_use]
pub fn impulse(len: usize, at: usize) -> Vec<f32> {
    let mut out = vec![0.0; len];
    if let Some(slot) = out.get_mut(at) {
        *slot = 1.0;
    }
    out
}

/// Silence of `len` samples with a single `amplitude` sample at `first`, `first + period`, `first + 2 * period`, and so
/// on. A `period` of 0 makes one click.
#[must_use]
pub fn click_train(len: usize, first: usize, period: usize, amplitude: f32) -> Vec<f32> {
    let mut out = vec![0.0; len];
    let mut at = first;
    while at < len {
        out[at] = amplitude;
        if period == 0 {
            break;
        }
        at += period;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_has_the_asked_period_and_peak() {
        let s = sine(48_000, 480.0, 0.5);
        assert!(s[0].abs() < 1e-7 && (s[25] - 0.5).abs() < 1e-5, "{} {}", s[0], s[25]);
        assert!(s[100].abs() < 1e-4, "480 Hz repeats every 100 samples");
    }

    #[test]
    fn sweeps_start_and_end_on_their_frequencies() {
        for shape in [Sweep::Linear, Sweep::Logarithmic] {
            let s = sine_sweep(96_000, 100.0, 1000.0, shape, 1.0);
            let crossings = |x: &[f32]| x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            // Zero crossings per 0.1 s are about the frequency in Hz divided by 10.
            let (start, end) = (crossings(&s[..4800]), crossings(&s[96_000 - 4800..]));
            assert!((9..=12).contains(&start), "{shape:?} start {start}");
            assert!((90..=105).contains(&end), "{shape:?} end {end}");
        }
    }

    #[test]
    fn clicks_land_where_asked() {
        assert_eq!(impulse(5, 2), [0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(impulse(5, 9), [0.0; 5]);
        assert_eq!(click_train(8, 1, 3, 0.5), [0.0, 0.5, 0.0, 0.0, 0.5, 0.0, 0.0, 0.5]);
        assert_eq!(click_train(4, 1, 0, 1.0), [0.0, 1.0, 0.0, 0.0]);
    }
}
