//! Measured voice parity (`voice.compare`): the aligner and the measures on a synthetic speaker, and the loop it is
//! for. A source-filter synthesiser (a glottal pulse train through vowel formants, noise bursts for consonants, a
//! falling intonation per phrase) reads one line as a "reference speaker", then three more takes with the natural
//! variation between takes, which set the floor. Then "ours" starts as a different voice (higher, slower, a longer
//! pause, other formants) and is fixed one step at a time by what the `worst` list says, toward the floor. Everything
//! is generated here: no recording, no reference that isn't the project's own.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::f64::consts::PI;

use omuzaki_speech::parity::ops::score;
use omuzaki_speech::parity::{HOP, RATE, analyse, compare, overlay};
use omuzaki_speech::synth::Rng;
use serde_json::Value;

/// A voice reading the line.
#[derive(Clone, Copy, Debug)]
struct Voice {
    /// Starting pitch, Hz.
    f0: f64,
    /// Formant scale (vocal tract length).
    formants: f64,
    /// Syllable length scale: 1 is the reference's tempo.
    tempo: f64,
    /// Extra pause before the second and the third phrase, s.
    pause: [f64; 2],
    /// Take-to-take variation: syllable lengths (share) and pitch (cents).
    jitter_len: f64,
    jitter_cents: f64,
    seed: u64,
}

const SPEAKER: Voice =
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

fn read(v: Voice) -> Vec<f32> {
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

fn metric(answer: &Value, name: &str) -> (f64, f64, bool) {
    let m = answer["metrics"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap();
    (m["value"].as_f64().unwrap(), m["target"].as_f64().unwrap(), m["pass"].as_bool().unwrap())
}

#[test]
fn the_aligner_finds_a_slower_reading_and_a_late_phrase() {
    let reference = analyse(&read(SPEAKER));
    let slow = analyse(&read(Voice { tempo: 1.2, pause: [0.25, 0.0], ..SPEAKER }));
    let p = compare(&slow, &reference).unwrap();
    let m = &p.measures;
    println!("{m:?}\n{:?}", p.segments);
    // About 1.2 times as long (the pauses don't stretch, the extra pause adds).
    assert!(m.duration_ratio > 1.12 && m.duration_ratio < 1.3, "{m:?}");
    // The second phrase starts later by the first phrase's stretch plus the pause.
    assert_eq!(p.segments.len(), 3, "{:?}", p.segments);
    let late = p.segments[1].offset_ms[0];
    assert!(late > 300.0 && late < 600.0, "{late}");
    assert!(m.f0_bias_cents.abs() < 20.0, "{m:?}");
}

#[test]
fn the_overlay_has_a_column_per_10_ms_of_the_reference() {
    let r = read(SPEAKER);
    let reference = analyse(&r);
    let p = compare(&analyse(&read(Voice { seed: 2, ..SPEAKER })), &reference).unwrap();
    let (first, last) = reference.speech_span().unwrap();
    let image = overlay(&p);
    assert_eq!(image.width as usize, last - first);
    assert_eq!(image.rgb.len(), (image.width * image.height * 3) as usize);
    assert_eq!(HOP, 160);
}

#[test]
fn a_copy_of_the_reference_is_refused() {
    let r = read(SPEAKER);
    let e = score(&r, &r, &[]).unwrap_err();
    assert!(e.message.contains("near-copy"), "{}", e.message);
}

/// The loop the op is for: measure, fix what the worst list names, measure again, until every measure is within the
/// floor. Every failing measure is named in the worst list with what to change.
#[test]
fn ours_is_fixed_step_by_step_toward_the_floor() {
    let reference = read(SPEAKER);
    let takes: Vec<Vec<f32>> = (2..5).map(|seed| read(Voice { seed, ..SPEAKER })).collect();
    let takes: Vec<&[f32]> = takes.iter().map(Vec::as_slice).collect();
    let steps = [
        (
            "a different voice: higher, slower, a longer pause, other formants",
            Voice { f0: 240.0, formants: 1.12, tempo: 1.18, pause: [0.25, 0.0], seed: 7, ..SPEAKER },
        ),
        (
            "pitch shifted down as the hint says",
            Voice { f0: 200.0, formants: 1.12, tempo: 1.18, pause: [0.25, 0.0], seed: 7, ..SPEAKER },
        ),
        ("speed and the pause fixed", Voice { formants: 1.12, seed: 7, ..SPEAKER }),
        ("a voice with the reference speaker's formants", Voice { seed: 7, ..SPEAKER }),
        (
            "the pause before phrase 3 shortened by the 50 ms the hint names",
            Voice { pause: [0.0, -0.05], seed: 7, ..SPEAKER },
        ),
    ];
    println!("| step | mcd dB | f0 bias cents | duration % | onset ms | done | worst |");
    println!("|---|---|---|---|---|---|---|");
    let mut answers = Vec::new();
    for (what, voice) in steps {
        let scored = score(&read(voice), &reference, &takes).unwrap();
        let answer = scored.answer(what, &[]);
        let worst = answer["worst"].get(0).map_or(String::from("none"), |w| {
            format!(
                "{} {}: {}",
                w["scope"].as_str().unwrap(),
                w["metric"].as_str().unwrap(),
                w["hint"].as_str().unwrap()
            )
        });
        println!(
            "| {what} | {:.2} | {:.0} | {:.1} | {:.0} | {} | {worst} |",
            metric(&answer, "mcd_db").0,
            metric(&answer, "f0_bias_cents").0,
            metric(&answer, "duration_error_pct").0,
            metric(&answer, "phrase_onset_ms").0,
            answer["done"]
        );
        answers.push(answer);
    }
    println!("floor: {}", answers[0]["floor"]);
    // Step 1: the pitch, the speed and the onset all fail, and the worst list says how far to shift the pitch.
    let first = &answers[0];
    assert!(!metric(first, "f0_bias_cents").2 && !metric(first, "duration_error_pct").2);
    let hints: Vec<&str> = first["worst"].as_array().unwrap().iter().filter_map(|w| w["hint"].as_str()).collect();
    assert!(hints.iter().any(|h| h.contains("semitones")), "{hints:?}");
    // Every step is closer than the one before on what it fixed.
    assert!(metric(&answers[1], "f0_bias_cents").0 < metric(first, "f0_bias_cents").0 / 4.0);
    assert!(metric(&answers[2], "duration_error_pct").0 < metric(&answers[1], "duration_error_pct").0 / 4.0);
    assert!(metric(&answers[3], "mcd_db").0 < metric(&answers[2], "mcd_db").0 / 2.0);
    // Step 4 is close but not there: the worst list names the late phrase, and step 5 follows it. What is left is
    // the rhythm inside the phrases, a little wider than the speaker's own other takes, and the answer says so: not
    // done until that is fixed too (it needs per-word timing, which this synthetic voice doesn't have).
    let last = &answers[4];
    let failing: Vec<&str> = last["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["pass"] == false)
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(metric(last, "phrase_onset_ms").2 && metric(last, "duration_error_pct").2, "{failing:?}");
    assert!(failing.len() <= 1, "{failing:?}");
    assert_eq!(last["done"], failing.is_empty());
    for name in failing {
        assert!(last["worst"].as_array().unwrap().iter().any(|w| w["metric"] == name), "{}", last["worst"]);
    }
}
