//! `fx.denoise` (#2587) on the voice: how much noise it takes out of a noisy voice, that it leaves a clean voice alone,
//! that the microphone mouth reads the cleaned voice at least as well as the noisy one, and its voice probability. It
//! lives here because the voice line and the lip-sync evaluation are `omuzaki-track-voice`'s; the node's own
//! real-time rules are tested beside it, in `omuzaki-audio-fx/tests/denoise.rs`.
//!
//! The set is licence-clean and made in code: the formant synthesiser's line of vowels, stops and nasals
//! (`omuzaki-track-voice::synth`, the lip-sync evaluation's own line) under three kinds of room noise: a fan (pink
//! noise), mains hum (60 Hz and its harmonics, with a little hiss) and a keyboard (short clicks several times a
//! second), each at 0, 5 and 10 dB SNR over the voice. Quality is the scale-invariant signal-to-distortion ratio
//! (SI-SDR) against the clean line, after the node's 960-sample delay.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::f64::consts::PI;

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{BlockMut, Node, NodeConfig, NodeKind, SAMPLE_RATE};
use omuzaki_audio_fx::denoise::{Denoise, DenoiseKind, FRAME, LATENCY};
use omuzaki_audio_mock::signals::{clap, pink_noise, white_noise};
use omuzaki_core::rng::Rng;
use omuzaki_face_api::expression::ExpressionSet;
use omuzaki_track_voice::eval::{LipSyncScore, frames_of, mic_mouth, per_hop, score};
use omuzaki_track_voice::synth::{Sound, utterance};
use omuzaki_track_voice::{LipSyncConfig, PhoneSpan, Viseme};

const RATE: u32 = SAMPLE_RATE;

/// The lip-sync evaluation's line, twice over (about 9 s), so the network has settled for most of it.
fn line() -> Vec<Sound> {
    use Sound::{Fricative, Nasal, Pause, Stop, Vowel};
    use Viseme::{Aa, Ee, Ih, Oh, Ou};
    let lips = |seconds| Stop { lips: true, seconds };
    let tongue = |seconds| Stop { lips: false, seconds };
    let once = [
        Pause(0.3),
        Nasal(0.08),
        Vowel(Aa, 0.22),
        Pause(0.15),
        lips(0.07),
        Vowel(Aa, 0.18),
        lips(0.08),
        Vowel(Aa, 0.2),
        Pause(0.3),
        Nasal(0.08),
        Vowel(Ih, 0.15),
        Vowel(Oh, 0.2),
        Nasal(0.09),
        Vowel(Ee, 0.25),
        Pause(0.25),
        lips(0.07),
        Vowel(Ou, 0.18),
        lips(0.08),
        Vowel(Oh, 0.2),
        Fricative(0.12),
        Vowel(Aa, 0.2),
        Pause(0.3),
        Vowel(Aa, 0.15),
        lips(0.1),
        Vowel(Aa, 0.2),
        Pause(0.2),
        tongue(0.07),
        Vowel(Oh, 0.16),
        Nasal(0.08),
        Vowel(Aa, 0.2),
        Pause(0.2),
        lips(0.07),
        Vowel(Ih, 0.16),
        Nasal(0.08),
        Vowel(Aa, 0.22),
        Pause(0.4),
    ];
    once.iter().chain(once.iter()).copied().collect()
}

#[derive(Copy, Clone, Debug)]
enum NoiseKind {
    Fan,
    Hum,
    Keyboard,
}

/// `len` samples of `kind`, at unit power.
fn noise(kind: NoiseKind, len: usize) -> Vec<f32> {
    let fs = f64::from(RATE);
    let mut out = match kind {
        NoiseKind::Fan => pink_noise(len, 11, 1.0),
        NoiseKind::Hum => {
            let hiss = white_noise(len, 12, 0.05);
            (0..len)
                .map(|n| {
                    let t = n as f64 / fs;
                    let hum: f64 = [(60.0, 1.0), (120.0, 0.5), (180.0, 0.35), (300.0, 0.2)]
                        .iter()
                        .map(|(f, a)| a * (2.0 * PI * f * t).sin())
                        .sum();
                    hum as f32 + hiss[n]
                })
                .collect()
        }
        NoiseKind::Keyboard => {
            // Keystrokes about seven times a second, each a 5 ms click, at varied strength, over a faint room.
            let mut out = white_noise(len, 13, 0.01);
            let mut rng = Rng::new(14);
            let mut at = 0usize;
            while at < len {
                at += (fs * f64::from(rng.range_f32(0.06, 0.25))) as usize;
                let click = clap(480, rng.range_f32(0.0, 1e6) as u64, 8.0, rng.range_f32(0.4, 1.0));
                for (i, c) in click.iter().enumerate() {
                    if let Some(s) = out.get_mut(at + i) {
                        *s += c;
                    }
                }
            }
            out
        }
    };
    let power = out.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / len as f64;
    let scale = (1.0 / power).sqrt() as f32;
    for s in &mut out {
        *s *= scale;
    }
    out
}

/// The voice's power over its voiced samples (pauses don't count, so an SNR means what a listener hears).
fn speech_power(clean: &[f32], truth: &[PhoneSpan]) -> f64 {
    let fs = f64::from(RATE);
    let (mut sum, mut n) = (0.0, 0usize);
    for span in truth.iter().filter(|s| s.phone != omuzaki_track_voice::Phone::Silence) {
        for s in &clean[(span.start_s * fs) as usize..((span.end_s * fs) as usize).min(clean.len())] {
            sum += f64::from(*s).powi(2);
            n += 1;
        }
    }
    sum / n.max(1) as f64
}

/// The scale-invariant SDR of `estimate` against `reference`, in dB.
fn si_sdr(reference: &[f32], estimate: &[f32]) -> f64 {
    let dot: f64 = reference.iter().zip(estimate).map(|(r, e)| f64::from(*r) * f64::from(*e)).sum();
    let energy: f64 = reference.iter().map(|r| f64::from(*r).powi(2)).sum();
    let alpha = dot / energy;
    let (mut target, mut error) = (0.0, 0.0);
    for (r, e) in reference.iter().zip(estimate) {
        let t = alpha * f64::from(*r);
        target += t * t;
        error += (f64::from(*e) - t).powi(2);
    }
    10.0 * (target / error.max(1e-30)).log10()
}

/// `x` through a fresh `fx.denoise` at `params`, with the node's delay taken out (the tail is padded with silence).
fn denoised(x: &[f32], params: &[(&str, f32)]) -> Vec<f32> {
    let mut node = DenoiseKind.create(&NodeConfig::default(), Channels::Mono).unwrap();
    let info = DenoiseKind.info();
    for (name, value) in params {
        node.set_param(info.param_index(name).unwrap(), *value);
    }
    node.reset();
    let mut out = x.to_vec();
    out.extend(std::iter::repeat_n(0.0, LATENCY));
    for (i, chunk) in out.chunks_mut(256).enumerate() {
        node.process(&mut BlockMut::mono(chunk, (i * 256) as u64));
    }
    out[LATENCY..].to_vec()
}

/// Leaves out the first second, while the network learns the room.
const SETTLE: usize = RATE as usize;

fn lipsync(pcm: &[f32], truth: &[PhoneSpan]) -> LipSyncScore {
    let set = ExpressionSet::starter();
    let end = truth.last().map_or(0.0, |s| s.end_s);
    let frames = frames_of(pcm, RATE).unwrap();
    let track = mic_mouth(&frames, &set, LipSyncConfig::default());
    score(truth, end, &per_hop(&track))
}

#[test]
fn it_takes_room_noise_out_of_a_voice() {
    let (clean, truth) = utterance(&line(), 140.0, RATE, 0.5);
    let speech = speech_power(&clean, &truth);
    let mut rows = Vec::new();
    for kind in [NoiseKind::Fan, NoiseKind::Hum, NoiseKind::Keyboard] {
        let n = noise(kind, clean.len());
        for snr_db in [0.0, 5.0, 10.0] {
            let gain = (speech / 10f64.powf(snr_db / 10.0)).sqrt() as f32;
            let noisy: Vec<f32> = clean.iter().zip(&n).map(|(c, n)| c + gain * n).collect();
            let out = denoised(&noisy, &[]);
            let before = si_sdr(&clean[SETTLE..], &noisy[SETTLE..]);
            let after = si_sdr(&clean[SETTLE..], &out[SETTLE..]);
            rows.push((kind, snr_db, before, after));
        }
    }
    println!("| noise | SNR in | SI-SDR before | after | gain |\n|---|---|---|---|---|");
    for (kind, snr, before, after) in &rows {
        println!("| {kind:?} | {snr:.0} dB | {before:.1} dB | {after:.1} dB | {:+.1} dB |", after - before);
    }
    let mean = rows.iter().map(|(.., b, a)| a - b).sum::<f64>() / rows.len() as f64;
    println!("mean gain {mean:+.1} dB");
    for (kind, snr, before, after) in &rows {
        assert!(after - before >= 3.0, "{kind:?} at {snr} dB: {before:.1} -> {after:.1}");
    }
    assert!(mean >= 6.0, "mean SI-SDR gain {mean:.1} dB");
}

#[test]
fn a_clean_voice_comes_through_nearly_untouched() {
    let (clean, _) = utterance(&line(), 140.0, RATE, 0.5);
    let out = denoised(&clean, &[]);
    let sdr = si_sdr(&clean[SETTLE..], &out[SETTLE..]);
    println!("clean voice SI-SDR through fx.denoise: {sdr:.1} dB");
    assert!(sdr >= 10.0, "{sdr:.1} dB");
}

#[test]
fn the_microphone_mouth_reads_the_cleaned_voice_as_well_as_the_noisy_one() {
    let (clean, truth) = utterance(&line(), 140.0, RATE, 0.5);
    let speech = speech_power(&clean, &truth);
    let reference = lipsync(&clean, &truth);
    println!("clean:    {reference:?}");
    for kind in [NoiseKind::Fan, NoiseKind::Keyboard] {
        let n = noise(kind, clean.len());
        let gain = (speech / 10f64.powf(5.0 / 10.0)).sqrt() as f32;
        let noisy: Vec<f32> = clean.iter().zip(&n).map(|(c, n)| c + gain * n).collect();
        let before = lipsync(&noisy, &truth);
        let after = lipsync(&denoised(&noisy, &[("gate", 1.0)]), &truth);
        println!("{kind:?} 5 dB noisy:    {before:?}\n{kind:?} 5 dB denoised: {after:?}");
        assert!(after.vowel_accuracy >= before.vowel_accuracy - 0.02, "{kind:?}: {before:?} -> {after:?}");
        // A click landing on a closure can shut the noisy mouth by luck, so the floor is the clean voice's own recall
        // where that is lower.
        let closures = before.closure_recall.min(reference.closure_recall);
        assert!(after.closure_recall >= closures - 0.05, "{kind:?}: {before:?} -> {after:?}");
        assert!(after.silence >= before.silence - 0.02, "{kind:?}: {before:?} -> {after:?}");
    }
}

#[test]
fn the_gate_takes_the_residue_between_words_down() {
    let (clean, truth) = utterance(&line(), 140.0, RATE, 0.5);
    let speech = speech_power(&clean, &truth);
    let n = noise(NoiseKind::Fan, clean.len());
    let gain = (speech / 10f64.powf(10.0 / 10.0)).sqrt() as f32;
    let noisy: Vec<f32> = clean.iter().zip(&n).map(|(c, n)| c + gain * n).collect();
    let fs = f64::from(RATE);
    // The pauses, less 100 ms either side.
    let pauses: Vec<(usize, usize)> = truth
        .iter()
        .filter(|s| s.phone == omuzaki_track_voice::Phone::Silence && s.end_s - s.start_s >= 0.3)
        .map(|s| (((s.start_s + 0.1) * fs) as usize, ((s.end_s - 0.1) * fs) as usize))
        .filter(|(a, _)| *a > SETTLE)
        .collect();
    let level = |x: &[f32]| {
        let parts: Vec<f32> = pauses.iter().flat_map(|(a, b)| x[*a..*b].iter().copied()).collect();
        let rms = (parts.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / parts.len().max(1) as f64).sqrt();
        20.0 * rms.max(1e-30).log10()
    };
    let (raw, plain, gated) =
        (level(&noisy), level(&denoised(&noisy, &[])), level(&denoised(&noisy, &[("gate", 1.0)])));
    println!("noise between words: {raw:.1} dBFS raw, {plain:.1} denoised, {gated:.1} with the gate");
    assert!(plain < raw - 10.0, "{raw} {plain}");
    assert!(gated < plain - 6.0, "{plain} {gated}");
}

#[test]
fn it_reports_the_voice_probability_and_its_reduction() {
    let mut node = Denoise::new();
    let (clean, _) = utterance(&line(), 140.0, RATE, 0.5);
    let n = noise(NoiseKind::Fan, clean.len());
    let noisy: Vec<f32> = clean.iter().zip(&n).map(|(c, n)| c + 0.05 * n).collect();
    let mut max_vad = 0.0f32;
    let mut max_reduction = 0.0f32;
    for (i, chunk) in noisy.chunks(FRAME).enumerate() {
        let mut b = chunk.to_vec();
        node.process(&mut BlockMut::mono(&mut b, (i * FRAME) as u64));
        max_vad = max_vad.max(node.voice_probability());
        max_reduction = max_reduction.max(node.report().gain_reduction_db);
    }
    assert!(max_vad > 0.5, "{max_vad}");
    assert!(max_reduction > 3.0, "{max_reduction}");
}
