//! The analyser's and the cue extractor's cost per second of audio, the numbers held against a CPU share: one second
//! of a vowel at 48 kHz (100 hops) per iteration.

#![allow(missing_docs, clippy::expect_used)]

use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_audio_api::{ClockSnapshot, EngineClock, TapInfo};
use omuzaki_core::time::Seconds;
use omuzaki_track_voice::synth::{self, Vowel};
use omuzaki_track_voice::{CueExtractor, Viseme, VoiceAnalyzer, VowelProfile};

fn analyze(c: &mut Criterion) {
    let samples = synth::vowel(Vowel::of(Viseme::Aa, 140.0, 1.0, 0.4), 48_000, 1.0, 0.002);
    let mut analyzer = VoiceAnalyzer::new(48_000, VowelProfile::default()).expect("a supported rate");
    c.bench_function("one_second_at_48k", |b| {
        b.iter(|| {
            let mut frames = 0;
            analyzer.push(&samples, |_| frames += 1);
            frames
        });
    });
}

fn cues(c: &mut Criterion) {
    let samples = synth::vowel(Vowel::of(Viseme::Aa, 140.0, 1.0, 0.4), 48_000, 1.0, 0.002);
    let info = TapInfo { first_sample: 0, rate: 48_000, capture_offset: Seconds(0.0), honours_mute: true };
    let clock = Arc::new(EngineClock::new(ClockSnapshot::nominal(48_000)));
    let mut extractor = CueExtractor::new(&info, clock).expect("a supported rate");
    let mut at = 0u64;
    c.bench_function("cues_one_second_at_48k", |b| {
        b.iter(|| {
            let mut frames = 0;
            extractor.push(at, false, &samples, |_| frames += 1);
            at += 48_000;
            frames
        });
    });
}

criterion_group!(benches, analyze, cues);
criterion_main!(benches);
