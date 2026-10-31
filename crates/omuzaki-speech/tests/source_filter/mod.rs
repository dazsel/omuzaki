//! A source-filter synthetic speaker, shared by the parity and prosody tests: a glottal pulse train through vowel
//! formants, noise bursts for consonants and a falling intonation per phrase. Everything is generated; no recording.
#![allow(dead_code, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::f64::consts::PI;

use omuzaki_speech::parity::RATE;
use omuzaki_speech::synth::Rng;

/// A voice reading the line.
#[derive(Clone, Copy, Debug)]
pub struct Voice {
    /// Starting pitch, Hz.
    pub f0: f64,
    /// Formant scale (vocal tract length).
    pub formants: f64,
    /// Syllable length scale: 1 is the reference's tempo.
    pub tempo: f64,
    /// Extra pause before the second and the third phrase, s.
    pub pause: [f64; 2],
    /// Take-to-take variation: syllable lengths (share) and pitch (cents).
    pub jitter_len: f64,
    pub jitter_cents: f64,
    pub seed: u64,
}

pub const SPEAKER: Voice =
    Voice { f0: 200.0, formants: 1.0, tempo: 1.0, pause: [0.0; 2], jitter_len: 0.06, jitter_cents: 15.0, seed: 1 };

/// Vowel formants (F1, F2, F3), Hz: a, e, i, o, u.
const VOWELS: [[f64; 3]; 5] = [
    [750.0, 1250.0, 2600.0],
    [500.0, 1850.0, 2600.0],
    [300.0, 2250.0, 3000.0],
    [500.0, 900.0, 2500.0],
    [320.0, 800.0, 2400.0],
];

/// The line: three phrases of syllables, each a vowel index.
const LINE: [&[usize]; 3] = [&[0, 2, 1, 3, 0], &[4, 1, 0, 2], &[3, 0, 1, 2, 0, 4]];

/// A two-pole resonator at `hz` with bandwidth `bw`.
struct Resonator {
    a1: f64,
    a2: f64,
    gain: f64,
    y1: f64,
    y2: f64,
}

impl Resonator {
    fn new(hz: f64, bw: f64) -> Self {
        let r = (-PI * bw / f64::from(RATE)).exp();
        let theta = 2.0 * PI * hz / f64::from(RATE);
        Self { a1: 2.0 * r * theta.cos(), a2: -r * r, gain: 1.0 - r, y1: 0.0, y2: 0.0 }
    }

    fn step(&mut self, x: f64) -> f64 {
        let y = self.gain * x + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

pub fn read(v: Voice) -> Vec<f32> {
    let fs = f64::from(RATE);
    let mut rng = Rng::new(v.seed);
    let mut jitter = |amount: f64| f64::from(rng.next_signed()) * amount;
    let mut out = vec![0.0f32; (0.3 * fs) as usize];
    let mut phase = 0.0f64;
    for (p, phrase) in LINE.iter().enumerate() {
        let syllables = phrase.len();
        for (s, &vowel) in phrase.iter().enumerate() {
            // Consonant: a short noise burst.
            let burst = ((0.045 * v.tempo * (1.0 + jitter(v.jitter_len))) * fs) as usize;
            for _ in 0..burst {
                out.push((jitter(0.05)) as f32);
            }
            // Vowel: pulses through three formants, pitch falling over the phrase and dropping at its end.
            let len = ((0.16 * v.tempo * (1.0 + jitter(v.jitter_len))) * fs) as usize;
            let cents = jitter(v.jitter_cents);
            let mut filters: Vec<Resonator> = VOWELS[vowel]
                .iter()
                .zip([80.0, 100.0, 140.0])
                .map(|(f, bw)| Resonator::new(f * v.formants, bw))
                .collect();
            for n in 0..len {
                let t = (s as f64 + n as f64 / len as f64) / syllables as f64;
                let fall = if s + 1 == syllables { 1.0 - 0.18 * (n as f64 / len as f64) } else { 1.0 };
                let f0 = v.f0 * (1.0 - 0.15 * t) * fall * 2f64.powf(cents / 1200.0);
                phase += f0 / fs;
                let pulse: f64 = (1..=30)
                    .take_while(|k| f64::from(*k) * f0 < fs / 2.0)
                    .map(|k| (2.0 * PI * f64::from(k) * phase).sin() / f64::from(k).powf(1.2))
                    .sum();
                let envelope = (n as f64 / 160.0).min(1.0) * ((len - n) as f64 / 160.0).min(1.0);
                let y: f64 = filters.iter_mut().map(|r| r.step(pulse)).sum();
                out.push((0.3 * y * envelope) as f32);
            }
        }
        let gap = match p {
            0 => 0.35 + v.pause[0],
            1 => 0.3 + v.pause[1],
            _ => 0.3,
        };
        out.extend(std::iter::repeat_n(0.0, (gap * fs) as usize));
    }
    out
}
