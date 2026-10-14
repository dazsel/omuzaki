//! Cost of one audio-callback block through each ring policy: what the audio thread pays per tap per block.

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

criterion_group!(benches, push_block);
criterion_main!(benches);
