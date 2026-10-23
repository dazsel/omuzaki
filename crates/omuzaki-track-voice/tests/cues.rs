//! The voice cues of #919 (PF-08): syllable onsets found at precision and recall of at least 0.9 within ±30 ms on a
//! synthetic voice with marked syllable onsets, every frame stamped exactly `clock.time_of(sample) - capture_offset`
//! through a real tap ring with a gap and a mute, and a mute giving zero cues.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::sync::Arc;

use omuzaki_audio_api::ring::{TapFlags, ring};
use omuzaki_audio_api::tap::DropPolicy;
use omuzaki_audio_api::{ClockSnapshot, EngineClock, TapInfo};
use omuzaki_audio_mock::signals::{SAMPLE_RATE, Trajectory, VoiceSpec, Vowel, pink_noise, voice};
use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use omuzaki_track_voice::{CueExtractor, CueFrame};

/// Onsets match within this, seconds.
const TOLERANCE: f64 = 0.030;

/// A clock that is not at sample 0, not at time 0 and not at exactly 48 kHz: what a real device's loop publishes.
fn clock() -> Arc<EngineClock> {
    Arc::new(EngineClock::new(ClockSnapshot {
        sample: 1_000_000,
        time: Seconds(12.5),
        seconds_per_sample: 1.0 / 47_999.3,
    }))
}

fn info(first_sample: u64) -> TapInfo {
    TapInfo { first_sample, rate: SAMPLE_RATE, capture_offset: Seconds(0.0371), honours_mute: true }
}

/// Speech-like syllables: each one a vowel at its own pitch and level rising in 8 ms from a consonant dip (`dip` of the
/// syllable's level: 0.03 is a stop, 30 dB down; 0.25 a nasal or a liquid, 12 dB down), with pauses of silence every few syllables, over a room at about -50 dBFS. Returns the samples and each syllable's
/// first sample.
fn syllables(seed: u64, count: usize, dip: f32) -> (Vec<f32>, Vec<usize>) {
    let mut rng = Rng::new(seed);
    let rate = f64::from(SAMPLE_RATE);
    let vowels = [Vowel::A, Vowel::E, Vowel::I, Vowel::O, Vowel::U];
    let (mut t, mut plan, mut f0, mut formants) = (0.4, Vec::new(), Vec::new(), Vec::new());
    for i in 0..count {
        let length = f64::from(rng.range_f32(0.12, 0.28));
        let level = rng.range_f32(0.5, 1.0);
        let pitch = f64::from(rng.range_f32(95.0, 230.0));
        f0.push((t, pitch));
        f0.push((t + length, pitch * f64::from(rng.range_f32(0.85, 1.1))));
        formants.push((t + 0.02, vowels[(rng.next_u64() % 5) as usize]));
        plan.push((t, length, level));
        let pause = i % 5 == 4;
        t += length + if pause { f64::from(rng.range_f32(0.3, 0.6)) } else { f64::from(rng.range_f32(0.04, 0.12)) };
    }
    let seconds = t + 0.5;
    let spec = VoiceSpec {
        seed,
        seconds,
        f0: Trajectory::new(f0),
        vowels: formants,
        jitter: 0.01,
        breathiness: 0.03,
        peak: 0.5,
    };
    let mut samples = voice(&spec);
    // The envelope: 8 ms raised-cosine attacks from wherever the last syllable left off, 30 ms releases into the dip,
    // silence in pauses and before the first syllable.
    let mut envelope = vec![0.0f32; samples.len()];
    let mut floor = 0.0f32;
    for (i, &(start, length, level)) in plan.iter().enumerate() {
        let dip = if i % 5 == 4 { 0.0 } else { dip * level };
        let (from, to) = ((start * rate) as usize, ((start + length) * rate) as usize);
        let next = plan.get(i + 1).map_or(samples.len(), |p| (p.0 * rate) as usize);
        for (n, e) in envelope.iter_mut().enumerate().take(next).skip(from) {
            *e = if n < to {
                let rise = ((n - from) as f32 / SAMPLE_RATE as f32 / 0.008).min(1.0);
                floor + (level - floor) * (0.5 - 0.5 * (std::f32::consts::PI * rise).cos())
            } else {
                let fall = ((n - to) as f32 / SAMPLE_RATE as f32 / 0.030).min(1.0);
                dip + (level - dip) * (1.0 - fall)
            };
        }
        floor = dip;
    }
    let room = pink_noise(samples.len(), seed ^ 0x5eed, 0.004);
    for ((s, e), r) in samples.iter_mut().zip(&envelope).zip(&room) {
        *s = *s * e + r;
    }
    let onsets = plan.iter().map(|p| (p.0 * rate) as usize).collect();
    (samples, onsets)
}

/// Precision and recall of `found` against `marked`, each found time used for at most one mark.
fn score(found: &[f64], marked: &[f64]) -> (f64, f64, Vec<f64>) {
    let mut used = vec![false; found.len()];
    let mut errors = Vec::new();
    for &m in marked {
        let best = found
            .iter()
            .enumerate()
            .filter(|(i, f)| !used[*i] && (*f - m).abs() <= TOLERANCE)
            .min_by(|a, b| (a.1 - m).abs().total_cmp(&(b.1 - m).abs()));
        if let Some((i, f)) = best {
            used[i] = true;
            errors.push(f - m);
        }
    }
    let hits = errors.len() as f64;
    (hits / found.len().max(1) as f64, hits / marked.len().max(1) as f64, errors)
}

/// Runs `samples` through the extractor in 10.7 ms blocks (not a multiple of the hop) and returns its frames.
fn extract(samples: &[f32], first: u64) -> Vec<CueFrame> {
    let mut cues = CueExtractor::new(&info(first), clock()).unwrap();
    let mut frames = Vec::new();
    for (k, block) in samples.chunks(513).enumerate() {
        cues.push(first + (k * 513) as u64, false, block, |f| frames.push(*f));
    }
    frames
}

/// The onset times in `frames`: each frame whose count moved carries the newest onset's time.
fn onset_times(frames: &[CueFrame]) -> Vec<f64> {
    let mut seen = 0;
    let mut times = Vec::new();
    for f in frames {
        if f.onsets != seen {
            assert_eq!(f.onsets, seen + 1, "at most one onset per 10 ms hop");
            times.push(f.onset_at.unwrap().0);
            seen = f.onsets;
        }
    }
    times
}

#[test]
fn syllable_onsets_have_precision_and_recall_of_at_least_0_9_within_30_ms() {
    let first = 1_234_567u64;
    let clock = clock();
    let (mut all_found, mut all_marked, mut all_errors) = (0usize, 0usize, Vec::new());
    for (seed, dip) in [(1, 0.03), (5, 0.03), (10, 0.1), (3, 0.25), (4, 0.25), (7, 0.25), (8, 0.25), (9, 0.25)] {
        let (samples, marks) = syllables(seed, 60, dip);
        let frames = extract(&samples, first);
        let found = onset_times(&frames);
        let marked: Vec<f64> =
            marks.iter().map(|&m| clock.time_of(first + m as u64).0 - info(first).capture_offset.0).collect();
        let (precision, recall, errors) = score(&found, &marked);
        eprintln!(
            "seed {seed}, dip {dip}: {} marked, {} found, precision {precision:.3}, recall {recall:.3}",
            marked.len(),
            found.len()
        );
        assert!(precision >= 0.9 && recall >= 0.9, "seed {seed}: precision {precision}, recall {recall}");
        all_found += found.len();
        all_marked += marked.len();
        all_errors.extend(errors);
    }
    let (precision, recall) = (all_errors.len() as f64 / all_found as f64, all_errors.len() as f64 / all_marked as f64);
    assert!(precision >= 0.95 && recall >= 0.95, "overall: precision {precision}, recall {recall}");
    all_errors.sort_by(f64::total_cmp);
    let median = all_errors[all_errors.len() / 2];
    eprintln!(
        "all: {all_marked} marked, {all_found} found, {} matched; timing error median {:.1} ms, from {:.1} to {:.1} ms",
        all_errors.len(),
        median * 1e3,
        all_errors[0] * 1e3,
        all_errors[all_errors.len() - 1] * 1e3
    );
}

#[test]
fn every_frame_is_stamped_exactly_through_a_ring_with_a_gap_and_a_mute() {
    let first = 5_000_000u64;
    let clock = clock();
    let info = info(first);
    let (samples, _) = syllables(9, 10, 0.03);
    let (mut producer, mut consumer) = ring(1 << 20, DropPolicy::Overwrite, first);
    let mut cues = CueExtractor::new(&info, Arc::clone(&clock)).unwrap();
    let mut frames = Vec::new();
    let mut buf = vec![0.0f32; 1024];
    let mut drain = |consumer: &mut omuzaki_audio_api::TapReader, frames: &mut Vec<CueFrame>| {
        loop {
            let run = consumer.read(&mut buf);
            if run.len == 0 {
                break;
            }
            cues.push(run.start_sample, run.muted, &buf[..run.len], |f| frames.push(*f));
        }
    };
    let third = samples.len() / 3;
    producer.push(&samples[..third]);
    drain(&mut consumer, &mut frames);
    // The device stalls for 1234 samples, then the person mutes for the middle third.
    producer.push_gap(1234);
    producer.set_flags(TapFlags { muted: true, private: false });
    producer.push(&vec![0.0; third]);
    drain(&mut consumer, &mut frames);
    producer.set_flags(TapFlags::default());
    producer.push(&samples[2 * third..]);
    drain(&mut consumer, &mut frames);

    assert!(frames.len() > 250, "{}", frames.len());
    for f in &frames {
        // Bit for bit: the stamp is the clock's time of the frame's sample, less the capture offset.
        assert_eq!(f.at.0.to_bits(), (clock.time_of(f.sample).0 - 0.0371).to_bits(), "{f:?}");
    }
    // Hops are 480 samples on the engine clock, and the gap moves them along without a frame for it.
    let steps: Vec<u64> = frames.windows(2).map(|w| w[1].sample - w[0].sample).collect();
    assert_eq!(frames[0].sample, first + 479);
    assert!(steps.iter().all(|&s| s == 480 || s == 480 + 1234), "{steps:?}");
    assert_eq!(steps.iter().filter(|&&s| s == 480 + 1234).count(), 1);

    let muted: Vec<&CueFrame> = frames.iter().filter(|f| f.muted).collect();
    assert!(muted.len() >= third / 480 - 1, "{}", muted.len());
    assert!(muted.iter().all(|f| f.loudness == 0.0), "zero loudness while muted");
    let at_mute = muted[0].onsets;
    assert!(muted.iter().all(|f| f.onsets == at_mute), "no onset while muted");
    let after = frames.iter().rev().find(|f| !f.muted).unwrap();
    assert!(after.onsets > at_mute, "the speech after the unmute has onsets");
    assert!(frames.iter().any(|f| !f.muted && f.loudness > 0.5), "and loudness");
}

#[test]
fn the_mouth_analyser_counts_the_same_syllables() {
    // The analyser reports an onset on a voiced 10 ms frame, so its times are frame ends: within 60 ms of the mark.
    use omuzaki_track_voice::{VoiceAnalyzer, VowelProfile};
    let (mut found, mut marked, mut matched) = (0usize, 0usize, 0usize);
    for (seed, dip) in [(1, 0.03), (10, 0.1), (3, 0.25)] {
        let (samples, marks) = syllables(seed, 60, dip);
        let mut analyzer = VoiceAnalyzer::new(SAMPLE_RATE, VowelProfile::default()).unwrap();
        let (mut at, mut seen, mut times) = (0usize, 0u32, Vec::new());
        for block in samples.chunks(480) {
            at += block.len();
            analyzer.push(block, |f| {
                if f.onsets != seen {
                    seen = f.onsets;
                    times.push(at as f64 / f64::from(SAMPLE_RATE));
                }
            });
        }
        let marks: Vec<f64> = marks.iter().map(|&m| m as f64 / f64::from(SAMPLE_RATE) + 0.030).collect();
        let (_, _, errors) = score(&times, &marks);
        eprintln!(
            "analyser, seed {seed}, dip {dip}: {} marked, {} found, {} matched",
            marks.len(),
            times.len(),
            errors.len()
        );
        (found, marked, matched) = (found + times.len(), marked + marks.len(), matched + errors.len());
    }
    let (precision, recall) = (matched as f64 / found as f64, matched as f64 / marked as f64);
    assert!(precision >= 0.9 && recall >= 0.9, "precision {precision}, recall {recall}");
}
