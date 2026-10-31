//! Prosody reshaping (#2683), measured with `voice.compare`'s analysis on the synthetic source-filter speaker.
//!
//! - **Formants kept:** the speaker pitched up 4 semitones by TD-PSOLA is measured against the same speaker *read*
//!   4 semitones higher (the truth: same vocal tract, higher voice). The declared-rate shift (resampling) is measured
//!   the same way, and its formants move with the pitch. Formants are read from an LPC envelope (order 14, every
//!   voiced 25 ms frame) and held to the unshifted speaker's; MCD can't witness this, because at these pitches the harmonics are wider apart than the low
//!   mel bands and the cepstrum follows the pitch as much as the vocal tract.
//! - **Range and tempo** do what they say, and leave the formants where they were.
//! - **Cost** is a small share of real time.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#![allow(clippy::many_single_char_names)] // LPC and its envelope in the textbook's names

use std::f64::consts::PI;
use std::time::Instant;

use omuzaki_speech::parity::{RATE, analyse, compare};
use omuzaki_speech::prosody::{Shape, pitch_track, reshape};
use omuzaki_speech::slots::tts::resample;
use source_filter::{SPEAKER, Voice, read};

mod source_filter;

const UP: f64 = 4.0;

fn ratio() -> f64 {
    2f64.powf(UP / 12.0)
}

/// The spread of the pitch, cents.
fn spread(pcm: &[f32]) -> f64 {
    let v: Vec<f64> = pitch_track(pcm, RATE).into_iter().flatten().map(|f| 1200.0 * f.log2()).collect();
    let m = v.iter().sum::<f64>() / v.len() as f64;
    (v.iter().map(|c| (c - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

/// The mean first and second formant over the voiced frames, Hz, from an order-14 LPC envelope.
fn formants(pcm: &[f32]) -> (f64, f64) {
    const ORDER: usize = 14;
    let fs = f64::from(RATE);
    let (win, hop) = ((0.025 * fs) as usize, (0.01 * fs) as usize);
    let track = pitch_track(pcm, RATE);
    let (mut f1, mut f2, mut n) = (0.0, 0.0, 0usize);
    for (i, f0) in track.iter().enumerate() {
        let Some(start) = (i * hop).checked_sub(win / 2) else { continue };
        if f0.is_none() || start + win > pcm.len() {
            continue;
        }
        // Pre-emphasis and a Hamming window.
        let x: Vec<f64> = (0..win)
            .map(|k| {
                let prev = if start + k > 0 { f64::from(pcm[start + k - 1]) } else { 0.0 };
                let w = 0.54 - 0.46 * (2.0 * PI * k as f64 / (win - 1) as f64).cos();
                (f64::from(pcm[start + k]) - 0.97 * prev) * w
            })
            .collect();
        let r: Vec<f64> = (0..=ORDER).map(|lag| (lag..win).map(|k| x[k] * x[k - lag]).sum()).collect();
        if r[0] <= 1e-9 {
            continue;
        }
        // Levinson-Durbin.
        let mut a = vec![0.0; ORDER + 1];
        a[0] = 1.0;
        let mut err = r[0];
        for m in 1..=ORDER {
            let acc: f64 = (0..m).map(|j| a[j] * r[m - j]).sum();
            let k = -acc / err;
            let prev = a.clone();
            for j in 1..m {
                a[j] = prev[j] + k * prev[m - j];
            }
            a[m] = k;
            err *= 1.0 - k * k;
        }
        // The envelope 1/|A| on a 10 Hz grid; its first two peaks above 150 Hz.
        let env: Vec<f64> = (0..400)
            .map(|b| {
                let w = 2.0 * PI * (f64::from(b) * 10.0) / fs;
                let (re, im) = a.iter().enumerate().fold((0.0, 0.0), |(re, im), (j, c)| {
                    (re + c * (w * j as f64).cos(), im - c * (w * j as f64).sin())
                });
                1.0 / (re * re + im * im).sqrt()
            })
            .collect();
        let peaks: Vec<f64> = (16..399)
            .filter(|&b| env[b] > env[b - 1] && env[b] >= env[b + 1])
            .map(|b| b as f64 * 10.0)
            .take(2)
            .collect();
        if let [p1, p2] = peaks[..] {
            f1 += p1.ln();
            f2 += p2.ln();
            n += 1;
        }
    }
    ((f1 / n as f64).exp(), (f2 / n as f64).exp())
}

#[test]
fn a_psola_pitch_shift_keeps_the_formants_and_a_resampled_one_moves_them() {
    let voice = read(SPEAKER);
    let truth = analyse(&read(Voice { f0: SPEAKER.f0 * ratio(), ..SPEAKER }));
    let psola = reshape(&voice, RATE, &Shape { semitones: UP, ..Shape::IDENTITY });
    // The TTS host's way: synthesise at speed / r and play at rate × r (the declared rate), here resampled back to 16 kHz.
    let slower = reshape(&voice, RATE, &Shape { tempo: 1.0 / ratio(), ..Shape::IDENTITY });
    let declared = resample(&slower, (f64::from(RATE) * ratio()).round() as u32, RATE);
    let (t1, t2) = formants(&read(Voice { f0: SPEAKER.f0 * ratio(), ..SPEAKER }));
    let (o1, o2) = formants(&voice);
    let (p1, p2) = formants(&psola);
    let (r1, r2) = formants(&declared);
    let p = compare(&analyse(&psola), &truth).unwrap().measures;
    let r = compare(&analyse(&declared), &truth).unwrap().measures;
    println!("| voice | F1 Hz | F2 Hz | pitch bias c against the truth | duration |");
    println!("|---|---|---|---|---|");
    println!("| the speaker, unshifted | {o1:.0} | {o2:.0} | | |");
    println!("| truth: the speaker read +4 | {t1:.0} | {t2:.0} | 0 | 1 |");
    println!("| TD-PSOLA +4 | {p1:.0} | {p2:.0} | {:.0} | {:.3} |", p.f0_bias_cents, p.duration_ratio);
    println!("| declared rate +4 | {r1:.0} | {r2:.0} | {:.0} | {:.3} |", r.f0_bias_cents, r.duration_ratio);
    // Both land on the pitch and the length.
    assert!(p.f0_bias_cents.abs() < 25.0 && r.f0_bias_cents.abs() < 25.0, "{p:?}\n{r:?}");
    assert!((p.duration_ratio - 1.0).abs() < 0.03, "{p:?}");
    // The LPC reading itself leans toward the harmonics: the truth, the same vocal tract read higher, reads a few per
    // cent off the unshifted speaker. So the formants are held to the speaker's own: PSOLA within 4 %, while the
    // declared rate moves F2 up by more than 12 % (the pitch ratio is 26 %).
    for (ours, own) in [(p1, o1), (p2, o2)] {
        assert!((ours / own - 1.0).abs() < 0.04, "psola {ours} against the speaker's {own}");
    }
    assert!(r2 / o2 > 1.12 && r2 / t2 > 1.12, "declared {r2} against the speaker's {o2} and the truth's {t2}");
}

#[test]
fn range_scales_the_intonation_and_tempo_the_length() {
    let voice = read(SPEAKER);
    let base = spread(&voice);
    for range in [0.5, 1.5, 2.0] {
        let shaped = reshape(&voice, RATE, &Shape { range, ..Shape::IDENTITY });
        let got = spread(&shaped) / base;
        println!("range {range}: spread × {got:.2}");
        assert!((got - range).abs() < 0.15 * range, "range {range}: × {got}");
        let ((a1, a2), (b1, b2)) = (formants(&shaped), formants(&voice));
        assert!((a1 / b1 - 1.0).abs() < 0.04 && (a2 / b2 - 1.0).abs() < 0.04, "range {range} moved the formants");
    }
    let fast = reshape(&voice, RATE, &Shape { tempo: 1.25, ..Shape::IDENTITY });
    let m = compare(&analyse(&fast), &analyse(&voice)).unwrap().measures;
    println!("tempo 1.25: {m:?}");
    assert!((m.duration_ratio - 0.8).abs() < 0.03 && m.f0_bias_cents.abs() < 15.0, "{m:?}");
    let ((a1, a2), (b1, b2)) = (formants(&fast), formants(&voice));
    assert!((a1 / b1 - 1.0).abs() < 0.04 && (a2 / b2 - 1.0).abs() < 0.04, "tempo moved the formants");
}

#[test]
fn reshaping_costs_a_small_share_of_real_time() {
    let voice = read(SPEAKER);
    let at24 = resample(&voice, RATE, 24_000);
    let seconds = at24.len() as f64 / 24_000.0;
    let start = Instant::now();
    let rounds = 3;
    for _ in 0..rounds {
        std::hint::black_box(reshape(&at24, 24_000, &Shape { semitones: 2.0, range: 1.4, tempo: 1.1 }));
    }
    let share = start.elapsed().as_secs_f64() / f64::from(rounds) / seconds;
    println!(
        "{:.2} % of real time ({} build)",
        share * 100.0,
        if cfg!(debug_assertions) { "debug" } else { "release" }
    );
    // A ceiling that holds in a debug build on CI; release runs at a tenth of it or less.
    assert!(share < 0.5, "{share}");
}
