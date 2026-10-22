//! Rate conversion of decoded audio to the engine's 48 kHz: a Kaiser-windowed sinc, evaluated at each output sample's
//! exact rational position in the input, so it has no delay and no drift over a long file.
//!
//! The ratio is reduced (44.1 to 48 kHz is 147:160), so an output's position falls on one of `up` phases between
//! input samples; each phase's taps are computed once and an output is one dot product over the input. A rate pair
//! with more than [`MAX_PHASES`] phases (an odd rate no real file uses) evaluates the kernel per output instead. The
//! cutoff sits at 95 % of the lower rate's Nyquist frequency with [`ZERO_CROSSINGS`] lobes each side, so the stop band
//! is below -90 dB and the pass band flat to well past 20 kHz from 44.1 kHz.

use omuzaki_audio_api::node::SAMPLE_RATE;

/// Kernel lobes on each side of the centre.
const ZERO_CROSSINGS: f64 = 32.0;

/// The cutoff as a fraction of the lower rate's Nyquist frequency.
const ROLLOFF: f64 = 0.95;

/// Kaiser window shape: about -90 dB of stop band.
const KAISER_BETA: f64 = 9.0;

/// The most phases tabulated (11.025 kHz has 640, 44.1 kHz 160).
const MAX_PHASES: u64 = 4_096;

/// Converts one channel from its rate to 48 kHz as input arrives: [`push`](Self::push) a chunk at a time, then
/// [`finish`](Self::finish) for the tail. Output sample `n` is the input at time `n · from / 48000` exactly.
#[derive(Clone, Debug)]
pub(crate) struct Resampler {
    /// Input samples per output sample, as the reduced ratio `down / up`.
    up: u64,
    down: u64,
    /// Input samples the kernel reaches on each side of an output's position; an output reads `2 · reach` of them,
    /// starting `reach - 1` before its position.
    reach: u64,
    kernel: Kernel,
    /// Each phase's taps, one after another; empty past [`MAX_PHASES`].
    phases: Vec<f32>,
    /// Scratch: the input window at the edges, and the taps when they aren't tabulated.
    window: Vec<f32>,
    taps: Vec<f32>,
    /// Input not yet consumed; `pending[0]` is input sample `base`.
    pending: Vec<f32>,
    base: u64,
    /// Input samples pushed so far.
    pushed: u64,
    /// The next output sample.
    next: u64,
}

/// The windowed sinc, in input samples from its centre.
#[derive(Clone, Copy, Debug)]
struct Kernel {
    /// Cycles per input sample.
    cutoff: f64,
    half_width: f64,
}

impl Kernel {
    fn at(self, x: f64) -> f32 {
        let x = x.abs();
        if x >= self.half_width {
            return 0.0;
        }
        let ratio = x / self.half_width;
        let window = bessel_i0(KAISER_BETA * (1.0 - ratio * ratio).sqrt()) / bessel_i0(KAISER_BETA);
        #[allow(clippy::cast_possible_truncation)] // The kernel's values are well inside f32.
        let value = (2.0 * self.cutoff * sinc(2.0 * self.cutoff * x) * window) as f32;
        value
    }
}

impl Resampler {
    /// A converter from `from` Hz (above zero) to 48 kHz.
    pub(crate) fn new(from: u32) -> Self {
        let from = u64::from(from.max(1));
        let to = u64::from(SAMPLE_RATE);
        let common = gcd(from, to);
        let (up, down) = (to / common, from / common);
        // The cutoff in cycles per input sample: the lower of the two Nyquist frequencies.
        #[allow(clippy::cast_precision_loss)] // Rates are far below 2^52.
        let cutoff = 0.5 * ROLLOFF * (to as f64 / from as f64).min(1.0);
        let kernel = Kernel { cutoff, half_width: ZERO_CROSSINGS / (2.0 * cutoff) };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A few hundred input samples.
        let reach = kernel.half_width.ceil() as u64;
        let mut resampler = Self {
            up,
            down,
            reach,
            kernel,
            phases: Vec::new(),
            window: Vec::new(),
            taps: Vec::new(),
            pending: Vec::new(),
            base: 0,
            pushed: 0,
            next: 0,
        };
        if up <= MAX_PHASES && !resampler.is_identity() {
            for phase in 0..up {
                resampler.fill_taps(phase);
                resampler.phases.extend_from_slice(&resampler.taps);
            }
        }
        resampler
    }

    /// Whether the input already runs at 48 kHz.
    pub(crate) fn is_identity(&self) -> bool {
        self.up == self.down
    }

    /// Output samples for `input_frames` of input: `ceil(input_frames · 48000 / from)`.
    pub(crate) fn output_len(&self, input_frames: u64) -> u64 {
        (input_frames * self.up).div_ceil(self.down)
    }

    /// Adds input and appends every output sample it completes to `out`.
    pub(crate) fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.is_identity() {
            out.extend_from_slice(input);
            return;
        }
        self.pending.extend_from_slice(input);
        self.pushed += input.len() as u64;
        self.run(self.pushed, out);
    }

    /// Appends the outputs the input's end still owes: the tail is read as silence past the last input sample.
    pub(crate) fn finish(&mut self, out: &mut Vec<f32>) {
        if self.is_identity() {
            return;
        }
        let total = self.output_len(self.pushed);
        while self.next < total {
            out.push(self.at(self.next));
            self.next += 1;
        }
        self.pending.clear();
    }

    /// Outputs while the kernel's right edge is inside the first `available` input samples.
    fn run(&mut self, available: u64, out: &mut Vec<f32>) {
        loop {
            let centre = self.next * self.down / self.up;
            if centre + self.reach >= available {
                break;
            }
            out.push(self.at(self.next));
            self.next += 1;
        }
        // Drop input no later output reaches, in batches so the copy is amortised.
        let keep_from = (self.next * self.down / self.up).saturating_sub(self.reach);
        let drop = usize::try_from(keep_from.saturating_sub(self.base)).unwrap_or(0);
        if drop > 4_096 && drop <= self.pending.len() {
            self.pending.drain(..drop);
            self.base += drop as u64;
        }
    }

    /// The taps of `phase` (an output `phase / up` of the way past an input sample) into `self.taps`.
    fn fill_taps(&mut self, phase: u64) {
        #[allow(clippy::cast_precision_loss)] // A fraction of `up`, and offsets of a few hundred.
        let (fraction, lead) = (phase as f64 / self.up as f64, (self.reach - 1) as f64);
        self.taps.clear();
        for k in 0..2 * self.reach {
            #[allow(clippy::cast_precision_loss)] // As above.
            let x = k as f64 - lead - fraction;
            self.taps.push(self.kernel.at(x));
        }
    }

    /// Output sample `n`: the taps of its phase over the input window around its position.
    fn at(&mut self, n: u64) -> f32 {
        let position = n * self.down;
        let (centre, phase) = (position / self.up, position % self.up);
        let count = usize::try_from(2 * self.reach).unwrap_or(0);
        // The window starts `reach - 1` before the centre; it's read straight from `pending` when it's all there.
        let start = (centre + 1)
            .checked_sub(self.reach + self.base)
            .and_then(|s| usize::try_from(s).ok())
            .filter(|s| s + count <= self.pending.len());
        if start.is_none() {
            self.window.clear();
            for k in 0..2 * self.reach {
                let sample = (centre + 1 + k)
                    .checked_sub(self.reach + self.base)
                    .and_then(|i| self.pending.get(usize::try_from(i).ok()?))
                    .copied()
                    .unwrap_or(0.0);
                self.window.push(sample);
            }
        }
        if self.phases.is_empty() {
            self.fill_taps(phase);
        }
        let window = start.map_or(self.window.as_slice(), |s| &self.pending[s..s + count]);
        let taps = if self.phases.is_empty() {
            self.taps.as_slice()
        } else {
            let first = usize::try_from(phase).unwrap_or(0) * count;
            &self.phases[first..first + count]
        };
        dot(taps, window)
    }
}

/// A dot product in eight lanes, so it vectorises.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    let ((chunks_a, rest_a), (chunks_b, rest_b)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    let tail: f32 = rest_a.iter().zip(rest_b).map(|(x, y)| x * y).sum();
    for (x, y) in chunks_a.iter().zip(chunks_b) {
        for ((lane, x), y) in lanes.iter_mut().zip(x).zip(y) {
            *lane += x * y;
        }
    }
    lanes.iter().sum::<f32>() + tail
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) }
}

/// The modified Bessel function of the first kind, order zero (the Kaiser window's shape).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, half) = (1.0, 1.0, x / 2.0);
    for k in 1..64 {
        let k = f64::from(k);
        term *= (half / k) * (half / k);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Converts a whole channel at once.
pub(crate) fn resample(input: &[f32], from: u32) -> Vec<f32> {
    let mut resampler = Resampler::new(from);
    let mut out = Vec::with_capacity(usize::try_from(resampler.output_len(input.len() as u64)).unwrap_or(0));
    resampler.push(input, &mut out);
    resampler.finish(&mut out);
    out
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // Test signals of a few seconds.
mod tests {
    use super::*;

    fn sine(rate: u32, frequency: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (0.5 * (std::f64::consts::TAU * frequency * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    /// The error against the ideal 48 kHz sine, in dB below the signal, away from the edges where the input starts
    /// and stops abruptly.
    fn error_db(out: &[f32], frequency: f64) -> f64 {
        let ideal = sine(SAMPLE_RATE, frequency, out.len());
        let middle = 1_000..out.len() - 1_000;
        let error: f64 = middle.clone().map(|i| f64::from(out[i] - ideal[i]).powi(2)).sum();
        let signal: f64 = middle.map(|i| f64::from(ideal[i]).powi(2)).sum();
        10.0 * (error / signal).log10()
    }

    #[test]
    fn every_common_rate_converts_to_48k_far_below_minus_60_db() {
        let mut worst = f64::NEG_INFINITY;
        for rate in [8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 88_200, 96_000, 176_400, 192_000] {
            // Up to 40 % of the rate: inside every rate's pass band (the cutoff is 47.5 %).
            for frequency in [100.0, 1_000.0, 0.4 * f64::from(rate.min(SAMPLE_RATE))] {
                let input = sine(rate, frequency, rate as usize);
                let out = resample(&input, rate);
                assert_eq!(out.len() as u64, Resampler::new(rate).output_len(input.len() as u64));
                assert_eq!(out.len(), 48_000, "{rate} Hz: a second");
                let db = error_db(&out, frequency);
                worst = worst.max(db);
                assert!(db < -85.0, "{rate} Hz, {frequency} Hz tone: {db:.1} dB");
            }
        }
        println!("resample: worst error {worst:.1} dB over 10 rates and 3 tones");
    }

    #[test]
    fn a_high_tone_from_44_1k_keeps_its_level() {
        // 18 kHz is inside the pass band from 44.1 kHz (the cutoff is 20.9 kHz).
        let out = resample(&sine(44_100, 18_000.0, 44_100), 44_100);
        let db = error_db(&out, 18_000.0);
        assert!(db < -60.0, "18 kHz: {db:.1} dB");
    }

    #[test]
    fn content_above_the_new_nyquist_is_removed_when_downsampling() {
        // 30 kHz from 96 kHz can't exist at 48 kHz; it must not fold back down as 18 kHz.
        let out = resample(&sine(96_000, 30_000.0, 96_000), 96_000);
        let middle = &out[1_000..out.len() - 1_000];
        let rms = (middle.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / middle.len() as f64).sqrt();
        let db = 20.0 * (rms / (0.5 / 2f64.sqrt())).log10();
        assert!(db < -80.0, "30 kHz folded back at {db:.1} dB");
    }

    #[test]
    fn chunked_input_gives_the_same_samples_as_one_block() {
        let input = sine(44_100, 440.0, 50_000);
        let whole = resample(&input, 44_100);
        let mut resampler = Resampler::new(44_100);
        let mut chunked = Vec::new();
        for chunk in input.chunks(777) {
            resampler.push(chunk, &mut chunked);
        }
        resampler.finish(&mut chunked);
        assert_eq!(whole.len(), chunked.len());
        assert!(whole.iter().zip(&chunked).all(|(a, b)| a.to_bits() == b.to_bits()), "bit-identical");
    }

    #[test]
    fn an_odd_rate_without_a_phase_table_converts_as_well() {
        // 44,099 Hz reduces to 16,000 phases: past the table, so the taps are computed per output.
        let rate = 44_099;
        assert_eq!(Resampler::new(rate).phases.len(), 0, "no table");
        let out = resample(&sine(rate, 1_000.0, rate as usize), rate);
        let db = error_db(&out, 1_000.0);
        assert!(db < -85.0, "{db:.1} dB");
    }

    #[test]
    fn input_at_48k_passes_through_untouched() {
        let input = sine(48_000, 440.0, 1_000);
        assert_eq!(resample(&input, 48_000), input);
    }
}
