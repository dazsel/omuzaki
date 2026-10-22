//! The engine's small signal pieces: the 3:1 speech decimator, the edge resampler and the meters. All are `Copy` state
//! or fixed buffers made on the control thread; nothing here allocates once made.

use omuzaki_audio_api::node::{BlockMut, SAMPLE_RATE};
use omuzaki_audio_api::report::Meters;

/// Taps of the 48 → 16 kHz decimator: odd, so its group delay is a whole number of samples (64, about 1.33 ms).
pub const DECIMATOR_TAPS: usize = 129;
/// The decimator's cutoff in Hz: halfway between the passband edge (7 kHz) and the stopband (9 kHz).
const DECIMATOR_CUTOFF_HZ: f64 = 8_000.0;
/// The Kaiser window's beta for about 70 dB of stopband attenuation.
const KAISER_BETA: f64 = 7.0;
/// The RMS meter's time constant (design section 6).
const RMS_SECONDS: f64 = 0.3;

/// The modified Bessel function of the first kind, order 0, by its series (for the Kaiser window).
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

/// A linear-phase Kaiser-windowed sinc low-pass, normalised to unity gain at DC.
fn decimator_coefficients() -> [f32; DECIMATOR_TAPS] {
    let mut h = [0.0f64; DECIMATOR_TAPS];
    #[allow(clippy::cast_precision_loss)] // Tap indices: tiny.
    let centre = (DECIMATOR_TAPS - 1) as f64 / 2.0;
    let fc = DECIMATOR_CUTOFF_HZ / f64::from(SAMPLE_RATE);
    let denom = bessel_i0(KAISER_BETA);
    for (n, value) in h.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let t = n as f64 - centre;
        let sinc =
            if t == 0.0 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * t).sin() / (std::f64::consts::PI * t) };
        let r = t / centre;
        *value = sinc * bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / denom;
    }
    let sum: f64 = h.iter().sum();
    #[allow(clippy::cast_possible_truncation)] // Filter taps below 1 in magnitude.
    h.map(|v| (v / sum) as f32)
}

/// The fixed 3:1 decimator of the 16 kHz taps: a 129-tap Kaiser low-pass, evaluated only at every third input.
#[derive(Clone, Debug)]
pub struct Decimator {
    coefficients: [f32; DECIMATOR_TAPS],
    history: [f32; DECIMATOR_TAPS],
    at: usize,
    phase: u8,
}

impl Default for Decimator {
    fn default() -> Self {
        Self::new()
    }
}

impl Decimator {
    /// A decimator with an empty (silent) history.
    #[must_use]
    pub fn new() -> Self {
        Self { coefficients: decimator_coefficients(), history: [0.0; DECIMATOR_TAPS], at: 0, phase: 0 }
    }

    /// The decimator's delay, in 48 kHz samples.
    #[must_use]
    pub const fn delay() -> u32 {
        // (129 - 1) / 2: fits u32 trivially.
        64
    }

    /// Takes one 48 kHz sample; returns a 16 kHz sample on every third.
    pub fn push(&mut self, sample: f32) -> Option<f32> {
        self.history[self.at] = sample;
        self.at = (self.at + 1) % DECIMATOR_TAPS;
        self.phase = (self.phase + 1) % 3;
        if self.phase != 0 {
            return None;
        }
        // `at` is the oldest sample now; the filter is symmetric, so the direction of the sum does not matter.
        let (newer, older) = self.history.split_at(self.at);
        let mut acc = 0.0f32;
        for (h, x) in self.coefficients.iter().zip(older.iter().chain(newer)) {
            acc += h * x;
        }
        Some(acc)
    }
}

/// Linear-interpolating rate conversion of one channel to 48 kHz, for a device or feed at another rate. Keeps one
/// sample of history and the fractional position across calls.
#[derive(Clone, Copy, Debug)]
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Position of the next output between `last` (0) and the next input (1).
    position: f64,
    last: f32,
}

impl Resampler {
    /// A converter from `from_rate` Hz to 48 kHz.
    #[must_use]
    pub fn new(from_rate: u32) -> Self {
        Self { step: f64::from(from_rate) / f64::from(SAMPLE_RATE), position: 0.0, last: 0.0 }
    }

    /// Whether the input already runs at 48 kHz.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        (self.step - 1.0).abs() < f64::EPSILON
    }

    /// Converts `input`, calling `out` with each 48 kHz sample.
    pub fn process(&mut self, input: &[f32], mut out: impl FnMut(f32)) {
        if self.is_identity() {
            input.iter().copied().for_each(out);
            return;
        }
        for &next in input {
            while self.position < 1.0 {
                #[allow(clippy::cast_possible_truncation)] // A fraction in [0, 1).
                let t = self.position as f32;
                out(self.last + (next - self.last) * t);
                self.position += self.step;
            }
            self.position -= 1.0;
            self.last = next;
        }
    }
}

/// A delay of a node's latency, run in its place while it is disabled, so switching a node off never shifts its bus
/// in time. Its history is allocated on the control thread; processing allocates nothing.
#[derive(Clone, Debug)]
pub struct HoldDelay {
    /// One history per channel; `history[c][position]` is the oldest sample, the next to leave.
    history: Vec<Vec<f32>>,
    position: usize,
}

impl HoldDelay {
    /// A delay of `latency` samples on `channels` channels, holding silence.
    #[must_use]
    pub fn new(latency: u32, channels: usize) -> Self {
        let length = usize::try_from(latency).unwrap_or(0);
        Self { history: vec![vec![0.0; length]; channels], position: 0 }
    }

    /// Replaces each sample of `block` with the one `latency` samples before it.
    pub fn process(&mut self, block: &mut BlockMut<'_>) {
        let length = self.history.first().map_or(0, Vec::len);
        if length == 0 {
            return;
        }
        let start = self.position;
        let mut channel = 0;
        let history = &mut self.history;
        block.for_each_channel(|samples| {
            if let Some(history) = history.get_mut(channel) {
                let mut at = start;
                for sample in samples.iter_mut() {
                    std::mem::swap(sample, &mut history[at]);
                    at = (at + 1) % length;
                }
            }
            channel += 1;
        });
        self.position = (start + block.frames()) % length;
    }
}

/// Peak (per block) and RMS (300 ms exponential) of a signal.
#[derive(Clone, Copy, Debug)]
pub struct MeterState {
    mean_square: f64,
    alpha: f64,
}

impl Default for MeterState {
    fn default() -> Self {
        Self { mean_square: 0.0, alpha: (-1.0 / (RMS_SECONDS * f64::from(SAMPLE_RATE))).exp() }
    }
}

fn to_db(linear: f64) -> f32 {
    if linear <= 0.0 {
        return f32::NEG_INFINITY;
    }
    #[allow(clippy::cast_possible_truncation)] // dBFS values: small.
    {
        (20.0 * linear.log10()) as f32
    }
}

impl MeterState {
    /// Measures one block of up to two channels (the louder of each pair counts) and returns the reading.
    pub fn measure(&mut self, channels: &[&[f32]]) -> Meters {
        let frames = channels.first().map_or(0, |c| c.len());
        let mut peak = 0.0f32;
        for n in 0..frames {
            let x = channels.iter().map(|c| c[n].abs()).fold(0.0f32, f32::max);
            peak = peak.max(x);
            self.mean_square = self.alpha * self.mean_square + (1.0 - self.alpha) * f64::from(x) * f64::from(x);
        }
        Meters { peak_db: to_db(f64::from(peak)), rms_db: to_db(self.mean_square.sqrt()), ..Meters::SILENT }
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
mod tests {
    use super::*;

    /// The decimator's gain at `hz`, measured with a sine after the filter settles.
    fn gain_at(hz: f64) -> f64 {
        let mut d = Decimator::new();
        let mut out = Vec::new();
        for n in 0..48_000u32 {
            let x = (2.0 * std::f64::consts::PI * hz * f64::from(n) / 48_000.0).sin();
            #[allow(clippy::cast_possible_truncation)]
            if let Some(y) = d.push(x as f32) {
                out.push(f64::from(y));
            }
        }
        let tail = &out[out.len() / 2..];
        #[allow(clippy::cast_precision_loss)]
        let rms = (tail.iter().map(|v| v * v).sum::<f64>() / tail.len() as f64).sqrt();
        rms * std::f64::consts::SQRT_2
    }

    #[test]
    fn the_decimator_meets_its_mask() {
        for hz in [100.0, 1_000.0, 4_000.0, 7_000.0] {
            let g = gain_at(hz);
            assert!((g - 1.0).abs() < 0.03, "{hz} Hz passes at {g:.4}");
        }
        for hz in [9_000.0, 12_000.0, 20_000.0] {
            let db = 20.0 * gain_at(hz).log10();
            assert!(db < -65.0, "{hz} Hz is down {db:.1} dB");
        }
    }

    #[test]
    fn the_resampler_keeps_a_tone_and_the_length() {
        let mut r = Resampler::new(44_100);
        let input: Vec<f32> = (0..44_100u32).map(|n| (f64::from(n) * 0.01).sin() as f32).collect();
        let mut out = Vec::new();
        r.process(&input, |s| out.push(s));
        assert!((out.len() as i64 - 48_000).abs() <= 1, "{}", out.len());
        let mut same = Resampler::new(48_000);
        let mut copy = Vec::new();
        same.process(&input, |s| copy.push(s));
        assert_eq!(copy, input);
    }

    #[test]
    fn meters_read_a_full_scale_sine() {
        let mut m = MeterState::default();
        let sine: Vec<f32> = (0..144_000u32).map(|n| (f64::from(n) * 0.1).sin() as f32).collect();
        let reading = m.measure(&[&sine]);
        assert!(reading.peak_db.abs() < 0.01);
        assert!((reading.rms_db + 3.01).abs() < 0.1, "{}", reading.rms_db);
        assert_eq!(MeterState::default().measure(&[&[0.0; 16]]).peak_db, f32::NEG_INFINITY);
    }
}
