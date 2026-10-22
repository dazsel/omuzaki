//! The analyser's cost per second of audio, the number held against a CPU share: one second of a vowel at 48 kHz (100
//! hops) per iteration.

#![allow(missing_docs, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_track_voice::synth::{self, Vowel};
use omuzaki_track_voice::{Viseme, VoiceAnalyzer, VowelProfile};

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

criterion_group!(benches, analyze);
criterion_main!(benches);
