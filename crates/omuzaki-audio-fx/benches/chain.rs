//! `voice.fx.cpu` (spec §10, #1159): the BR-24 chain with every effect on, pitch and formant included, over the
//! synthetic voice in 256-sample blocks, the audio device's block size. Each kind alone is measured beside it, so the
//! shifter's own share is stated.
//!
//! Criterion reports the time per second of audio; a second of audio is 1,000 ms of real time, so the CPU share of 8
//! cores is `time / 1 s / 8`. The budget harness's report of the same number is `examples/voice_fx_budget.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use omuzaki_audio_api::graph::NodeSpec;
use omuzaki_audio_fx::chain::Chain;
use omuzaki_audio_fx::presets::budget_chain_with_pitch;
use omuzaki_audio_mock::signals::{VoiceSpec, voice};

const BLOCK: usize = 256;

fn run(c: &mut Criterion, name: &str, chain: &[NodeSpec], audio: &[f32]) {
    let mut group = c.benchmark_group("voice.fx.cpu");
    group.throughput(Throughput::Elements(audio.len() as u64));
    let mut built = Chain::build(chain).unwrap();
    let mut buffer = audio.to_vec();
    group.bench_function(name, |b| {
        b.iter(|| {
            buffer.copy_from_slice(audio);
            for block in buffer.chunks_mut(BLOCK) {
                built.process(block);
            }
            black_box(&buffer);
        });
    });
    group.finish();
}

fn bench(c: &mut Criterion) {
    let audio = voice(&VoiceSpec::steady(1, 1.0));
    let chain = budget_chain_with_pitch();
    run(c, "chain (1 s of voice)", &chain, &audio);
    for node in &chain {
        run(c, &format!("{} alone (1 s of voice)", node.kind), std::slice::from_ref(node), &audio);
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
