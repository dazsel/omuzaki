//! Cost of one audio-callback block through each ring policy: what the audio thread pays per tap per block; and of
//! one feed pull (`feed/pull_256`), against the bare linear pull it replaced (`feed/bare_pull_256`, the same
//! interpolation with no playhead and no fade), so the playhead's cost on the real-time path is a number.

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_audio_api::ring::ring;
use omuzaki_audio_api::tap::DropPolicy;
use std::hint::black_box;

fn push_block(c: &mut Criterion) {
    let block = vec![0.25f32; 256];
    let mut sink = vec![0.0f32; 256];
    for (name, policy) in [("drop_newest", DropPolicy::DropNewest), ("overwrite", DropPolicy::Overwrite)] {
        let (mut tx, mut rx) = ring(48_000, policy, 0);
        c.bench_function(&format!("ring/push_256_then_read/{name}"), |b| {
            b.iter(|| {
                black_box(tx.push(black_box(&block)));
                black_box(rx.read(&mut sink));
            });
        });
        let (mut tx, _rx) = ring(48_000, policy, 0);
        c.bench_function(&format!("ring/push_256_only/{name}"), |b| b.iter(|| black_box(tx.push(black_box(&block)))));
    }
}

/// The pull the engine ran before the playhead (#1248): ring pops and linear interpolation only.
fn bare_pull(rx: &mut omuzaki_audio_api::ring::Consumer, state: &mut (f32, f32, f64), out: &mut [f32], step: f64) {
    for sample in out.iter_mut() {
        while state.2 >= 1.0 {
            state.2 -= 1.0;
            let mut frame = [0.0f32];
            if rx.pop_into(&mut frame) < 1 {
                frame[0] = 0.0;
            }
            state.0 = state.1;
            state.1 = frame[0];
        }
        #[allow(clippy::cast_possible_truncation)]
        let t = state.2 as f32;
        *sample = state.0 + (state.1 - state.0) * t;
        state.2 += step;
    }
}

fn feed_pull(c: &mut Criterion) {
    let block = vec![0.25f32; 256];
    for rate in [48_000u32, 24_000] {
        let (producer, consumer) = ring(96_000, DropPolicy::DropNewest, 0);
        let (mut writer, mut source) = omuzaki_audio_api::feed::feed_channel(producer, consumer, rate, false);
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let mut at = 0u64;
        c.bench_function(&format!("feed/pull_256/{rate}"), |b| {
            b.iter(|| {
                writer.push(black_box(&block));
                black_box(source.pull(&mut left, &mut right, 256, at));
                at += 256;
            });
        });
        let (mut tx, mut rx) = ring(96_000, DropPolicy::DropNewest, 0);
        let mut state = (0.0f32, 0.0f32, 1.0f64);
        let step = f64::from(rate) / 48_000.0;
        c.bench_function(&format!("feed/bare_pull_256/{rate}"), |b| {
            b.iter(|| {
                tx.push(black_box(&block));
                bare_pull(&mut rx, &mut state, &mut left, step);
                black_box(&left);
            });
        });
    }
}

criterion_group!(benches, push_block, feed_pull);
criterion_main!(benches);
