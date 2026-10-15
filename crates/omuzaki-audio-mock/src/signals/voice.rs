//! A synthetic voice: a Rosenberg glottal pulse train through four formant resonators.
//!
//! The source is the classic Rosenberg pulse (a raised-cosine opening and a quarter-cosine closing), differentiated to
//! model the lips' radiation, with a little period jitter and aspiration noise. The vocal tract is four Klatt
//! resonators in series, whose frequencies and bandwidths follow the vowel trajectory. It is not speech, but it has the
//! things the audio tests need: a voiced harmonic spectrum with formants, a pitch that moves, and onsets.

use std::f64::consts::{PI, TAU};

use omuzaki_core::rng::Rng;

use super::math::{cos, exp};
use super::{SAMPLE_RATE, normalise, samples_in};

/// Formant centre frequencies and bandwidths of one vowel, in Hz.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FormantFrequencies {
    /// F1 to F4.
    pub centre: [f64; 4],
    /// The bandwidth of each.
    pub bandwidth: [f64; 4],
}

impl FormantFrequencies {
    fn lerp(self, other: Self, t: f64) -> Self {
        let mix = |a: [f64; 4], b: [f64; 4]| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
        Self { centre: mix(self.centre, other.centre), bandwidth: mix(self.bandwidth, other.bandwidth) }
    }
}

/// The vowels the voice can say: average adult male formants after Peterson and Barney (1952).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Vowel {
    /// As in "father".
    A,
    /// As in "bed".
    E,
    /// As in "beet".
    I,
    /// As in "law".
    O,
    /// As in "boot".
    U,
}

impl Vowel {
    /// The formants of the vowel.
    #[must_use]
    pub fn formants(self) -> FormantFrequencies {
        let centre = match self {
            Self::A => [730.0, 1090.0, 2440.0, 3400.0],
            Self::E => [530.0, 1840.0, 2480.0, 3500.0],
            Self::I => [270.0, 2290.0, 3010.0, 3500.0],
            Self::O => [570.0, 840.0, 2410.0, 3400.0],
            Self::U => [300.0, 870.0, 2240.0, 3400.0],
        };
        FormantFrequencies { centre, bandwidth: [60.0, 90.0, 120.0, 130.0] }
    }
}

/// A value that moves in straight lines between points `(seconds, value)`, and holds the first and last value outside
/// them.
#[derive(Clone, Debug, PartialEq)]
pub struct Trajectory {
    points: Vec<(f64, f64)>,
}

impl Trajectory {
    /// A value that never changes.
    #[must_use]
    pub fn constant(value: f64) -> Self {
        Self { points: vec![(0.0, value)] }
    }

    /// A trajectory through `points`, which are sorted by time here. Empty `points` give a constant 0.
    #[must_use]
    pub fn new(mut points: Vec<(f64, f64)>) -> Self {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        if points.is_empty() {
            points.push((0.0, 0.0));
        }
        Self { points }
    }

    /// The value at `seconds`.
    #[must_use]
    pub fn at(&self, seconds: f64) -> f64 {
        let after = self.points.partition_point(|p| p.0 <= seconds);
        match (after.checked_sub(1).map(|i| self.points[i]), self.points.get(after)) {
            (Some(before), Some(next)) if next.0 > before.0 => {
                before.1 + (next.1 - before.1) * (seconds - before.0) / (next.0 - before.0)
            }
            (Some(before), _) => before.1,
            (None, Some(next)) => next.1,
            (None, None) => 0.0,
        }
    }
}

/// What to say, and how.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceSpec {
    /// The seed of the jitter and the aspiration noise.
    pub seed: u64,
    /// How long the voice runs, in seconds.
    pub seconds: f64,
    /// The fundamental frequency in Hz over time.
    pub f0: Trajectory,
    /// The vowel at given times, in any order; the formants move in straight lines between them in time order.
    pub vowels: Vec<(f64, Vowel)>,
    /// Random variation of each glottal period, as a fraction of it (0.005 is a steady voice).
    pub jitter: f64,
    /// Aspiration noise relative to the glottal pulses (0.02 is a slightly breathy voice).
    pub breathiness: f32,
    /// The peak of the result.
    pub peak: f32,
}

impl VoiceSpec {
    /// A steady voice: `/a/` at 120 Hz for `seconds`, peak 0.5.
    #[must_use]
    pub fn steady(seed: u64, seconds: f64) -> Self {
        Self {
            seed,
            seconds,
            f0: Trajectory::constant(120.0),
            vowels: vec![(0.0, Vowel::A)],
            jitter: 0.005,
            breathiness: 0.02,
            peak: 0.5,
        }
    }

    fn formants_at(&self, seconds: f64) -> FormantFrequencies {
        let after = self.vowels.partition_point(|v| v.0 <= seconds);
        let at = |i: usize| self.vowels.get(i).map_or_else(|| Vowel::A.formants(), |v| v.1.formants());
        match (after.checked_sub(1), self.vowels.get(after)) {
            (Some(i), Some(next)) if next.0 > self.vowels[i].0 => {
                at(i).lerp(next.1.formants(), (seconds - self.vowels[i].0) / (next.0 - self.vowels[i].0))
            }
            (Some(i), _) => at(i),
            (None, Some(next)) => next.1.formants(),
            (None, None) => Vowel::A.formants(),
        }
    }
}

/// How often the resonator coefficients follow the trajectory, in samples: fine enough that a vowel glide has no
/// audible steps, coarse enough not to spend the run on `exp` and `cos`.
const COEFFICIENT_INTERVAL: usize = 16;

/// The open and closing fractions of the Rosenberg pulse.
const OPEN: f64 = 0.4;
const CLOSING: f64 = 0.16;

/// One Klatt resonator: unity gain at DC, so the formants shape the spectrum without changing the level much.
#[derive(Copy, Clone, Default)]
struct Resonator {
    a: f64,
    b: f64,
    c: f64,
    y1: f64,
    y2: f64,
}

impl Resonator {
    fn tune(&mut self, centre: f64, bandwidth: f64) {
        let rate = f64::from(SAMPLE_RATE);
        self.c = -exp(-TAU * bandwidth / rate);
        self.b = 2.0 * exp(-PI * bandwidth / rate) * cos(TAU * centre / rate);
        self.a = 1.0 - self.b - self.c;
    }

    fn step(&mut self, x: f64) -> f64 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// The Rosenberg pulse at `phase` in `[0, 1)`.
fn rosenberg(phase: f64) -> f64 {
    if phase < OPEN {
        0.5 * (1.0 - cos(PI * phase / OPEN))
    } else if phase < OPEN + CLOSING {
        cos(0.5 * PI * (phase - OPEN) / CLOSING)
    } else {
        0.0
    }
}

/// Renders `spec`.
#[must_use]
pub fn voice(spec: &VoiceSpec) -> Vec<f32> {
    let len = samples_in(spec.seconds);
    // The vowels may be given in any order; the formants follow them in time order.
    let mut sorted = spec.clone();
    sorted.vowels.sort_by(|a, b| a.0.total_cmp(&b.0));
    let spec = &sorted;
    let rate = f64::from(SAMPLE_RATE);
    let mut rng = Rng::new(spec.seed);
    let mut resonators = [Resonator::default(); 4];
    let (mut phase, mut previous_pulse) = (0.0f64, 0.0f64);
    let mut period_scale = 1.0f64;
    let mut out = Vec::with_capacity(len);
    for n in 0..len {
        let t = n as f64 / rate;
        if n % COEFFICIENT_INTERVAL == 0 {
            let formants = spec.formants_at(t);
            for (i, resonator) in resonators.iter_mut().enumerate() {
                resonator.tune(formants.centre[i], formants.bandwidth[i]);
            }
        }
        phase += spec.f0.at(t).max(1.0) / rate * period_scale;
        if phase >= 1.0 {
            phase -= 1.0;
            // A new glottal period: draw its length error now, so the jitter is per period and not per sample.
            period_scale = 1.0 + spec.jitter * f64::from(rng.range_f32(-1.0, 1.0));
        }
        let pulse = rosenberg(phase);
        // The derivative of the flow is what leaves the lips.
        let mut x = (pulse - previous_pulse) * 20.0;
        previous_pulse = pulse;
        x += f64::from(spec.breathiness) * f64::from(rng.range_f32(-1.0, 1.0));
        for resonator in &mut resonators {
            x = resonator.step(x);
        }
        // An `f32` signal from `f64` arithmetic.
        #[allow(clippy::cast_possible_truncation)]
        out.push(x as f32);
    }
    normalise(&mut out, spec.peak);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The power of `x` at `hz`, by Goertzel's recurrence.
    fn power_at(x: &[f32], hz: f64) -> f64 {
        let w = TAU * hz / f64::from(SAMPLE_RATE);
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s = f64::from(v) + coeff * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        s1 * s1 + s2 * s2 - coeff * s1 * s2
    }

    /// The lag in `lo..hi` with the highest autocorrelation: the pitch period.
    fn period(x: &[f32], lo: usize, hi: usize) -> usize {
        (lo..hi)
            .max_by(|&a, &b| {
                let r = |lag: usize| x.iter().zip(&x[lag..]).map(|(p, q)| f64::from(p * q)).sum::<f64>();
                r(a).total_cmp(&r(b))
            })
            .unwrap_or(lo)
    }

    #[test]
    fn the_same_seed_gives_the_same_voice() {
        let spec = VoiceSpec::steady(11, 0.2);
        let a = voice(&spec);
        assert_eq!(a, voice(&spec));
        assert_ne!(a, voice(&VoiceSpec::steady(12, 0.2)));
        assert!((super::super::peak(&a) - 0.5).abs() < 1e-6);
        assert!(a.iter().all(|s| s.is_finite()));
        assert_eq!(a.len(), 9600);
    }

    #[test]
    fn the_vowel_puts_energy_at_its_first_formant() {
        // At 120 Hz the harmonic nearest /a/'s F1 (730 Hz) is the 6th, 720 Hz; nearest /i/'s F1 (270 Hz) is the 2nd,
        // 240 Hz.
        let say = |vowel| {
            let mut spec = VoiceSpec::steady(1, 0.5);
            spec.vowels = vec![(0.0, vowel)];
            voice(&spec)[4800..].to_vec()
        };
        let (a, i) = (say(Vowel::A), say(Vowel::I));
        assert!(power_at(&a, 720.0) > 3.0 * power_at(&a, 240.0), "/a/ is strong near 720 Hz");
        assert!(power_at(&i, 240.0) > 3.0 * power_at(&i, 720.0), "/i/ is strong near 240 Hz");
    }

    #[test]
    fn vowels_may_be_given_in_any_order() {
        let mut ordered = VoiceSpec::steady(3, 0.4);
        ordered.vowels = vec![(0.0, Vowel::A), (0.2, Vowel::I)];
        let mut shuffled = ordered.clone();
        shuffled.vowels.reverse();
        assert_eq!(voice(&ordered), voice(&shuffled));
    }

    #[test]
    fn the_pitch_follows_its_trajectory() {
        let mut spec = VoiceSpec::steady(2, 1.0);
        spec.f0 = Trajectory::new(vec![(0.0, 100.0), (1.0, 200.0)]);
        spec.jitter = 0.0;
        let v = voice(&spec);
        let start = period(&v[..2400], 150, 600);
        let end = period(&v[v.len() - 2400..], 150, 600);
        // 100 Hz is 480 samples, 200 Hz is 240; the windows sit at about 105 and 195 Hz.
        assert!(start.abs_diff(457) < 25, "start period {start}");
        assert!(end.abs_diff(246) < 25, "end period {end}");
    }

    #[test]
    fn trajectories_hold_their_ends_and_interpolate() {
        let t = Trajectory::new(vec![(1.0, 10.0), (0.0, 0.0), (2.0, 30.0)]);
        assert_eq!([t.at(-1.0), t.at(0.5), t.at(1.5), t.at(9.0)], [0.0, 5.0, 20.0, 30.0]);
        assert_eq!(Trajectory::constant(7.0).at(3.0), 7.0);
    }
}
