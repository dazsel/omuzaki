//! [`OnsetDetector`]: syllable onsets from the three-band onset strength (PF-08 voice cues, `audio-api.md` §5.3).
//!
//! The strength is the clap test's envelope (§5.3 step 2, the estimator of #911), streamed:
//!
//! 1. three bands, [`ONSET_BANDS`], each a second-order band-pass;
//! 2. log energy per band over [`ONSET_WINDOW_S`] at a 1 ms hop;
//! 3. the first difference, half-wave rectified, summed over the bands;
//! 4. minus a [`MEDIAN_S`] moving median, clamped at zero.
//!
//! One change for a voice: the clap test's window is 4 ms, shorter than a glottal period, so on a vowel its energy
//! ripples with every pulse and each ripple reads as an onset. [`ONSET_WINDOW_S`] spans a period of the lowest voice
//! (70 Hz), so a held vowel is flat and only a rise in level counts. The log keeps the response at the front of a
//! rise: a sound entering the window lifts its log energy most in the first millisecond.
//!
//! Peaks are picked against an adaptive threshold, the median plus [`MAD_K`] median absolute deviations of the last
//! [`THRESHOLD_S`] of strength (never below [`MIN_STRENGTH_DB`], so a silent room does not make every breath an
//! onset), at most one per [`ONSET_REFRACTORY_S`].
//!
//! Everything is allocated in [`OnsetDetector::new`]; [`OnsetDetector::step`] allocates nothing.

use std::f32::consts::PI;

/// The three bands, Hz: low (the first formant and the voice's fundamental), mid (the second formant), high
/// (fricatives and bursts). Bands keep the strength robust when a changer moves energy between them.
pub const ONSET_BANDS: [(f32, f32); 3] = [(150.0, 600.0), (600.0, 2_400.0), (2_400.0, 8_000.0)];
/// The log-energy window, seconds: longer than a period at 70 Hz (14.3 ms), so a held vowel does not ripple.
pub const ONSET_WINDOW_S: f32 = 0.016;
/// The moving median taken off the strength, seconds: what a steady sound keeps rising by is not an onset.
pub const MEDIAN_S: f32 = 0.100;
/// The span the adaptive threshold looks back over, seconds.
pub const THRESHOLD_S: f32 = 1.0;
/// The threshold is the median plus this many median absolute deviations.
pub const MAD_K: f32 = 2.0;
/// The threshold never drops below this, dB of summed rise per millisecond.
pub const MIN_STRENGTH_DB: f32 = 3.5;
/// The shortest gap between onsets, seconds.
pub const ONSET_REFRACTORY_S: f32 = 0.120;
/// Band levels below this are this, dBFS: a room's noise floor has no onsets.
const BAND_FLOOR_DB: f32 = -70.0;
/// How often the threshold is recomputed, in hops (10 ms): it moves over a second, so this is plenty.
const THRESHOLD_EVERY: usize = 10;

/// A second-order section, transposed direct form II.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn normalised(b: [f32; 3], a0: f32, a1: f32, a2: f32) -> Self {
        Self { b: b.map(|x| x / a0), a: [a1 / a0, a2 / a0], z: [0.0; 2] }
    }

    /// A band-pass from `lo` to `hi` Hz at `rate` (0 dB at its centre); `hi` is kept below Nyquist.
    pub(crate) fn band_pass(lo: f32, hi: f32, rate: f32) -> Self {
        let hi = hi.min(0.45 * rate);
        let centre = (lo * hi).sqrt();
        let q = centre / (hi - lo).max(1.0);
        let w = 2.0 * PI * centre / rate;
        let alpha = w.sin() / (2.0 * q);
        Self::normalised([alpha, 0.0, -alpha], 1.0 + alpha, -2.0 * w.cos(), 1.0 - alpha)
    }

    /// A Butterworth high-pass at `hz`.
    pub(crate) fn high_pass(hz: f32, rate: f32) -> Self {
        let w = 2.0 * PI * hz / rate;
        let (cos, alpha) = (w.cos(), w.sin() / std::f32::consts::SQRT_2);
        let b = f32::midpoint(1.0, cos);
        Self::normalised([b, -2.0 * b, b], 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    /// A Butterworth low-pass at `hz`, kept below Nyquist.
    pub(crate) fn low_pass(hz: f32, rate: f32) -> Self {
        let w = 2.0 * PI * hz.min(0.45 * rate) / rate;
        let (cos, alpha) = (w.cos(), w.sin() / std::f32::consts::SQRT_2);
        let b = 0.5 * (1.0 - cos);
        Self::normalised([b, 2.0 * b, b], 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    #[inline]
    pub(crate) fn step(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The median of `values`, reordering them. 0 for none.
fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mid = values.len() / 2;
    *values.select_nth_unstable_by(mid, f32::total_cmp).1
}

/// A ring of the newest values, with a scratch copy for medians.
#[derive(Clone, Debug)]
struct History {
    values: Box<[f32]>,
    scratch: Box<[f32]>,
    head: usize,
    filled: usize,
}

impl History {
    fn new(len: usize) -> Self {
        let len = len.max(1);
        Self { values: vec![0.0; len].into(), scratch: vec![0.0; len].into(), head: 0, filled: 0 }
    }

    fn push(&mut self, value: f32) {
        self.values[self.head] = value;
        self.head = (self.head + 1) % self.values.len();
        self.filled = (self.filled + 1).min(self.values.len());
    }

    /// The values held, copied into the scratch.
    fn copy(&mut self) -> &mut [f32] {
        let n = self.filled;
        self.scratch[..n].copy_from_slice(&self.values[..n]);
        &mut self.scratch[..n]
    }

    fn median(&mut self) -> f32 {
        median(self.copy())
    }

    /// The median plus `k` median absolute deviations.
    fn median_plus_mad(&mut self, k: f32) -> f32 {
        let values = self.copy();
        let m = median(values);
        for v in values.iter_mut() {
            *v = (*v - m).abs();
        }
        m + k * median(values)
    }
}

/// The streaming onset detector. One per signal; feed it with [`step`](Self::step) from one thread.
#[derive(Clone, Debug)]
pub struct OnsetDetector {
    bands: [Biquad; 3],
    /// Samples per 1 ms hop, and how many of this hop have been seen.
    hop: usize,
    in_hop: usize,
    /// Each band's energy summed over this hop.
    acc: [f32; 3],
    /// Each band's energy per hop over the window, circular.
    window: Box<[[f32; 3]]>,
    head: usize,
    /// Each band's log energy at the previous hop.
    last_db: [f32; 3],
    raw: History,
    strength: History,
    threshold: f32,
    hops: usize,
    /// The strength one and two hops ago, for peak picking.
    prev: [f32; 2],
    refractory: u32,
    refractory_hops: u32,
}

impl OnsetDetector {
    /// A detector for mono samples at `rate` Hz. Cold: allocates.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small counts
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(1) as f32;
        let hop = (rate / 1000.0).round().max(1.0) as usize;
        let hops_per_s = rate / hop as f32;
        let hops = |s: f32| (s * hops_per_s).round().max(1.0) as usize;
        Self {
            bands: ONSET_BANDS.map(|(lo, hi)| Biquad::band_pass(lo, hi, rate)),
            hop,
            in_hop: 0,
            acc: [0.0; 3],
            window: vec![[0.0; 3]; hops(ONSET_WINDOW_S)].into(),
            head: 0,
            last_db: [BAND_FLOOR_DB; 3],
            raw: History::new(hops(MEDIAN_S)),
            strength: History::new(hops(THRESHOLD_S)),
            threshold: MIN_STRENGTH_DB,
            hops: 0,
            prev: [0.0; 2],
            refractory: 0,
            refractory_hops: hops(ONSET_REFRACTORY_S) as u32,
        }
    }

    /// Samples per hop.
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Feeds one sample. Returns how many samples ago an onset was, the sample the onset's peak hop is centred on,
    /// when this sample completes the hop after that peak (a peak is known one hop late). Allocates nothing.
    pub fn step(&mut self, x: f32) -> Option<usize> {
        let x = if x.is_finite() { x } else { 0.0 };
        for (band, acc) in self.bands.iter_mut().zip(self.acc.iter_mut()) {
            let y = band.step(x);
            *acc += y * y;
        }
        self.in_hop += 1;
        if self.in_hop < self.hop {
            return None;
        }
        self.in_hop = 0;
        self.end_hop().then_some(self.hop + self.hop / 2)
    }

    /// Ends a hop: the strength of this hop, and whether the previous one was an onset.
    #[allow(clippy::cast_precision_loss)] // window lengths are small
    fn end_hop(&mut self) -> bool {
        self.window[self.head] = self.acc;
        self.head = (self.head + 1) % self.window.len();
        self.acc = [0.0; 3];
        let samples = (self.window.len() * self.hop) as f32;
        let mut raw = 0.0;
        for b in 0..3 {
            let energy = self.window.iter().map(|e| e[b]).sum::<f32>() / samples;
            let db = (10.0 * (energy + 1e-12).log10()).max(BAND_FLOOR_DB);
            raw += (db - self.last_db[b]).max(0.0);
            self.last_db[b] = db;
        }
        self.raw.push(raw);
        let strength = (raw - self.raw.median()).max(0.0);

        if self.hops.is_multiple_of(THRESHOLD_EVERY) {
            self.threshold = self.strength.median_plus_mad(MAD_K).max(MIN_STRENGTH_DB);
        }
        self.hops = self.hops.wrapping_add(1);
        self.strength.push(strength);

        self.refractory = self.refractory.saturating_sub(1);
        let [one, two] = self.prev;
        self.prev = [strength, one];
        let peak = one > self.threshold && one > two && one >= strength && self.refractory == 0;
        if peak {
            // Counted from the peak's hop, which has ended already.
            self.refractory = self.refractory_hops.saturating_sub(1);
        }
        peak
    }

    /// The threshold in force, dB of summed rise per millisecond. For meters and tests.
    pub fn threshold(&self) -> f32 {
        self.threshold
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod tests {
    use super::*;

    fn onsets(rate: u32, samples: &[f32]) -> Vec<usize> {
        let mut detector = OnsetDetector::new(rate);
        samples.iter().enumerate().filter_map(|(i, &x)| detector.step(x).map(|ago| i + 1 - ago)).collect()
    }

    fn tone(rate: u32, seconds: f32, hz: f32, level: f32) -> Vec<f32> {
        let n = (seconds * rate as f32) as usize;
        (0..n).map(|i| level * (2.0 * PI * hz * i as f32 / rate as f32).sin()).collect()
    }

    #[test]
    fn a_tone_burst_is_one_onset_at_its_start() {
        for rate in [16_000, 48_000] {
            let mut samples = vec![0.0; rate as usize / 2];
            samples.extend(tone(rate, 0.4, 440.0, 0.3));
            samples.extend(vec![0.0; rate as usize / 2]);
            let found = onsets(rate, &samples);
            assert_eq!(found.len(), 1, "{rate}: {found:?}");
            let error = found[0].abs_diff(rate as usize / 2);
            assert!(error <= rate as usize / 500, "{rate}: {found:?}");
        }
    }

    #[test]
    fn silence_and_a_steady_tone_have_none() {
        assert_eq!(onsets(48_000, &vec![0.0; 48_000]), Vec::<usize>::new());
        // At most the tone's own start (the first hop has no median to stand out against), then nothing.
        assert!(onsets(48_000, &tone(48_000, 2.0, 200.0, 0.3)).len() <= 1);
    }

    #[test]
    fn the_filters_pass_their_band() {
        let rate = 48_000.0;
        let gain = |mut f: Biquad, hz: f32| {
            let n = 48_000;
            let out: Vec<f32> = (0..n).map(|i| f.step((2.0 * PI * hz * i as f32 / rate).sin())).collect();
            let tail = &out[n / 2..];
            (tail.iter().map(|y| y * y).sum::<f32>() / tail.len() as f32 * 2.0).sqrt()
        };
        assert!((gain(Biquad::band_pass(600.0, 2_400.0, rate), 1_200.0) - 1.0).abs() < 0.02);
        assert!(gain(Biquad::band_pass(600.0, 2_400.0, rate), 100.0) < 0.15);
        assert!(gain(Biquad::high_pass(100.0, rate), 30.0) < 0.15);
        assert!(gain(Biquad::low_pass(4_000.0, rate), 12_000.0) < 0.15);
        assert!((gain(Biquad::low_pass(4_000.0, rate), 500.0) - 1.0).abs() < 0.02);
    }
}
