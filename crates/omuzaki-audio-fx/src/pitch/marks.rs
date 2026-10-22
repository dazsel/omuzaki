//! Pitch marks: one per glottal closure while the voice is voiced (the largest LPC-residual peak within each predicted
//! period), and a 5 ms pseudo-period with seeded ±10% jitter while it is not.

use super::f0::Estimate;
use crate::dsp::{noise, samples};

/// The pseudo-period of unvoiced stretches, in milliseconds.
pub const PSEUDO_MS: f64 = 5.0;

/// How far either side of the predicted period a closure is searched for, as a fraction of the period. The search
/// closes 1.25 periods after the last mark, so a mark is confirmed at most half a period (400 samples at 60 Hz) after
/// it.
pub const SEARCH: f64 = 0.25;

/// The marks kept for the synthesis to read.
pub const KEPT: usize = 64;

/// One pitch mark.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Mark {
    /// Its sample, counted from the node's last reset.
    pub at: i64,
    /// The period around it, in samples.
    pub period: f64,
    /// Whether it is a glottal closure (voiced) or a pseudo-mark.
    pub voiced: bool,
}

/// The mark tracker and the marks it has placed.
#[derive(Clone, Debug)]
pub struct Marks {
    ring: [Mark; KEPT],
    count: u64,
    seed: u64,
}

impl Marks {
    /// A tracker with no marks; `seed` drives the unvoiced jitter.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { ring: [Mark { at: 0, period: 240.0, voiced: false }; KEPT], count: 0, seed }
    }

    /// Forgets every mark.
    pub fn reset(&mut self) {
        self.count = 0;
    }

    /// How many marks have been placed since the reset.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// The `i`th mark placed (if it is still kept).
    #[must_use]
    pub fn get(&self, i: u64) -> Option<Mark> {
        (i < self.count && self.count - i <= KEPT as u64).then(|| self.ring[(i % KEPT as u64) as usize])
    }

    /// The newest mark.
    #[must_use]
    pub fn last(&self) -> Option<Mark> {
        self.count.checked_sub(1).and_then(|i| self.get(i))
    }

    /// The index of the latest mark at or before `at`.
    #[must_use]
    pub fn at_or_before(&self, at: i64) -> Option<u64> {
        let oldest = self.count.saturating_sub(KEPT as u64);
        (oldest..self.count).rev().find(|i| self.get(*i).is_some_and(|m| m.at <= at))
    }

    fn place(&mut self, mark: Mark) {
        self.ring[(self.count % KEPT as u64) as usize] = mark;
        self.count += 1;
    }

    /// Takes the residual sample `k` (`residual(j)` reads any recent one) with the latest F0 estimate, placing every
    /// mark that `k` confirms.
    pub fn observe(&mut self, k: i64, residual: impl Fn(i64) -> f64, estimate: Estimate) {
        let Some(mut last) = self.last() else {
            self.place(Mark { at: k, period: samples(PSEUDO_MS) as f64, voiced: false });
            return;
        };
        loop {
            if estimate.voiced {
                let period = estimate.period;
                // Locked on a voiced mark, the next closure is searched near one period on; coming out of an unvoiced
                // stretch the phase is unknown, so the whole next period is searched.
                let (lo, hi) = if last.voiced { (1.0 - SEARCH, 1.0 + SEARCH) } else { (0.0, 1.0) };
                #[allow(clippy::cast_possible_truncation)] // a few hundred samples
                let (from, to) = (last.at + ((period * lo).ceil() as i64).max(1), last.at + (period * hi).floor() as i64);
                if k < to {
                    return;
                }
                let expected = last.at as f64 + period;
                let mut best = (from, f64::MIN);
                for j in from..=to {
                    let off = if last.voiced { (j as f64 - expected) / (SEARCH * period) } else { 0.0 };
                    let score = residual(j).abs() * (1.0 - 0.5 * off * off);
                    if score > best.1 {
                        best = (j, score);
                    }
                }
                // A lost lock: if the strongest peak of the last period is well over the one chosen and outside the
                // window, the closures are there.
                #[allow(clippy::cast_possible_truncation)]
                let start = to - period as i64;
                let (strongest, value) = (start..=to).fold((best.0, 0.0f64), |b, j| {
                    let v = residual(j).abs();
                    if v > b.1 { (j, v) } else { b }
                });
                if last.voiced && value > 2.0 * residual(best.0).abs() && strongest > last.at + 16 && (strongest < from || strongest > to) {
                    best.0 = strongest;
                }
                last = Mark { at: best.0, period, voiced: true };
            } else {
                let jitter = 1.0 + 0.1 * noise(self.seed, self.count);
                let pseudo = samples(PSEUDO_MS) as f64 * jitter;
                #[allow(clippy::cast_possible_truncation)] // a few hundred samples
                let at = last.at + pseudo.round() as i64;
                if k < at {
                    return;
                }
                last = Mark { at, period: pseudo, voiced: false };
            }
            self.place(last);
        }
    }
}
