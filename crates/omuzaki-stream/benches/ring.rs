//! The rolling buffer (#1017): one push into a full 60 s buffer, and selecting a 60 s clip from it (1,800 video chunks
//! and 6,000 audio chunks).

// A benchmark's setup may panic: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss)]

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::Seconds;
use omuzaki_stream::replay::ring::{Caps, Charge, ChunkKind, Payload, RollingBuffer};

#[derive(Clone)]
struct Blob(Arc<Charge>);

impl Payload for Blob {
    fn len(&self) -> u64 {
        self.0.bytes()
    }
}

fn push(ring: &mut RollingBuffer<Blob>, kind: ChunkKind, pts: f64, duration: f64, len: u64) {
    ring.push_with(kind, Seconds(pts), Seconds(duration), len, |c| Ok::<_, ()>(Blob(Arc::new(c)))).unwrap();
}

/// Pushes frame `f` of a 30 fps, 1 s GOP stream (80 kB keyframes, 8 kB deltas) and its audio: 6,000 audio chunks per
/// 60 s, as four 48 kHz tracks in 1,920-sample (40 ms) chunks give.
fn frame(ring: &mut RollingBuffer<Blob>, f: u64) {
    let t = f as f64 / 30.0;
    let key = f.is_multiple_of(30);
    push(ring, ChunkKind::Video { keyframe: key }, t, 1.0 / 30.0, if key { 80_000 } else { 8_000 });
    // 25 chunks a second per track, 40 ms each.
    if (f + 1) * 25 / 30 > f * 25 / 30 {
        for track in 0..4 {
            push(ring, ChunkKind::Audio { track }, t, 0.04, 7_680);
        }
    }
}

fn bench(c: &mut Criterion) {
    let mut ring = RollingBuffer::new(Caps { max_span: Seconds(61.0), max_bytes: 1 << 30 }).unwrap();
    let mut f = 0;
    while f < 30 * 70 {
        frame(&mut ring, f);
        f += 1;
    }
    c.bench_function("ring push, warm", |b| {
        b.iter(|| {
            frame(&mut ring, f);
            f += 1;
        });
    });
    c.bench_function("ring select 60 s", |b| b.iter(|| black_box(ring.select(Seconds(60.0)))));
}

criterion_group!(benches, bench);
criterion_main!(benches);
