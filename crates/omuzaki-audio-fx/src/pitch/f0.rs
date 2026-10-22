//! The F0 tracker: YIN's cumulative mean normalised difference (de Cheveigné and Kawahara, 2002) on the signal
//! decimated to 16 kHz, with a 0.15 threshold, parabolic refinement, a voiced/unvoiced decision with hysteresis, and
//! continuity against octave jumps.

use crate::dsp::distance;

/// The decimation from 48 kHz to the analysis rate.
pub const DECIMATION: usize = 3;

/// The analysis rate, in Hz.
pub const RATE: f64 = 16_000.0;

/// The lowest F0, in Hz.
pub const F0_MIN: f64 = 60.0;

/// The highest F0, in Hz.
pub const F0_MAX: f64 = 800.0;

/// The longest lag searched, in decimated samples (60 Hz).
pub const TAU_MAX: usize = 267;

/// The shortest lag searched, in decimated samples (800 Hz).
pub const TAU_MIN: usize = 20;

/// The integration window, in decimated samples (10 ms).
pub const WINDOW: usize = 160;

/// The decimated history one estimate reads (27 ms).
pub const SPAN: usize = WINDOW + TAU_MAX;

/// The threshold that makes a frame voiced.
pub const THRESHOLD: f64 = 0.15;

/// The threshold under which a voiced track stays voiced (the hysteresis).
pub const STAY: f64 = 0.3;

/// How much shallower than the deepest dip the chosen one may be.
const NEAR: f64 = 0.05;

/// The quietest frame that can be voiced: mean square of about -66 dBFS.
const SILENCE: f64 = 2.5e-7;

/// One estimate.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Estimate {
    /// The period at 48 kHz, in samples (meaningful when voiced; the last voiced period otherwise).
    pub period: f64,
    /// Whether the frame is voiced.
    pub voiced: bool,
    /// The CMND value at the chosen lag (0 is perfectly periodic).
    pub aperiodicity: f64,
}

/// The tracker's state between frames and its scratch space.
#[derive(Clone, Debug)]
pub struct Yin {
    d: [f64; TAU_MAX + 2],
    last: Estimate,
}

impl Default for Yin {
    fn default() -> Self {
        Self::new()
    }
}

impl Yin {
    /// A tracker that has heard nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self { d: [0.0; TAU_MAX + 2], last: Estimate { period: 400.0, voiced: false, aperiodicity: 1.0 } }
    }

    /// The last estimate.
    #[must_use]
    pub const fn last(&self) -> Estimate {
        self.last
    }

    /// Back to silence.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The local minimum of the CMND at or after `tau`, walking down while it falls.
    fn settle(&self, mut tau: usize) -> usize {
        while tau < TAU_MAX && self.d[tau + 1] < self.d[tau] {
            tau += 1;
        }
        tau
    }

    /// The lowest CMND lag within `lo..=hi`.
    fn lowest(&self, lo: usize, hi: usize) -> usize {
        (lo.max(TAU_MIN)..=hi.min(TAU_MAX)).min_by(|a, b| self.d[*a].total_cmp(&self.d[*b])).unwrap_or(TAU_MIN)
    }

    /// Estimates from `y`, the last [`SPAN`] decimated samples (oldest first).
    pub fn estimate(&mut self, y: &[f64; SPAN]) -> Estimate {
        let energy = y[..WINDOW].iter().map(|v| v * v).sum::<f64>() / WINDOW as f64;
        // The difference function, then the cumulative mean normalisation in place.
        self.d[0] = 1.0;
        let mut running = 0.0;
        for tau in 1..=TAU_MAX {
            let sum = distance(&y[..WINDOW], &y[tau..tau + WINDOW]);
            running += sum;
            #[allow(clippy::cast_precision_loss)] // a lag
            let norm = if running > 0.0 { sum * tau as f64 / running } else { 1.0 };
            self.d[tau] = norm;
        }
        self.d[TAU_MAX + 1] = f64::MAX;
        // The first dip that is almost as deep as the deepest. YIN's own rule (the first dip under the threshold) takes
        // the second harmonic for the fundamental when it sits on F1 and dominates, as it does for /a/ around 360 Hz;
        // the deepest dip alone may be a multiple of the period (every multiple of a steady voice's period dips as
        // deep).
        let deepest = self.d[self.lowest(TAU_MIN, TAU_MAX)];
        let mut tau = (TAU_MIN..=TAU_MAX).find(|t| self.d[*t] <= deepest + NEAR).map_or(TAU_MIN, |t| self.settle(t));
        // Continuity: when the chosen dip is a multiple of a voiced track's period, keep the track's own dip if it is
        // almost as good. Never the other way: a dip at a fraction of the track's period that is as deep as the
        // deepest is real periodicity, so the track was on a multiple.
        if self.last.voiced {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a lag in range
            let previous = (self.last.period / DECIMATION as f64).round() as usize;
            #[allow(clippy::cast_precision_loss)]
            let ratio = tau as f64 / previous.max(1) as f64;
            if (ratio - 2.0).abs() < 0.25 || (ratio - 3.0).abs() < 0.3 {
                let near = self.lowest(previous * 4 / 5, previous * 6 / 5);
                if self.d[near] < STAY && self.d[near] < self.d[tau] + 0.1 {
                    tau = near;
                }
            }
        }
        let aperiodicity = self.d[tau];
        let voiced = energy > SILENCE && aperiodicity < if self.last.voiced { STAY } else { THRESHOLD };
        // Parabolic refinement around the dip.
        #[allow(clippy::cast_precision_loss)]
        let mut lag = tau as f64;
        if tau > 1 && tau < TAU_MAX {
            let (a, b, c) = (self.d[tau - 1], self.d[tau], self.d[tau + 1]);
            let den = a - 2.0 * b + c;
            if den > 0.0 {
                lag += (0.5 * (a - c) / den).clamp(-0.5, 0.5);
            }
        }
        let period = if voiced { lag * DECIMATION as f64 } else { self.last.period };
        self.last = Estimate { period, voiced, aperiodicity };
        self.last
    }
}
