//! The replay core (#1019): `push_frame` at 1080p30 packet sizes (the fake encoder's 60 kB keyframes and 6 kB deltas,
//! with side data like VP9 alpha) plus one 48 kHz mono track, into a warm memory-backed 60 s buffer.

// A benchmark's setup may panic: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::cast_possible_wrap)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_audio_api::ring::TapRead;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_stream::record::runtime::{AudioTrack, TrackStart};
use omuzaki_stream::replay::runtime::ReplayCore;
use omuzaki_stream::replay::settings::{Backing, ReplaySettings};
use omuzaki_stream::testing::encoder::{FakeEncoderConfig, FakeEncoderFactory};
use omuzaki_stream::timeline::AudioSource;
use omuzaki_stream_api::encoder::{AlphaKind, CodecId, EncodeConfig, Frame, VideoSpec};
use omuzaki_stream_api::time::Ticks;

const SIZE: UVec2 = UVec2::new(1920, 1080);

struct Paced(Arc<AtomicU64>, u64);

impl AudioSource for Paced {
    fn read(&mut self, out: &mut [f32]) -> TapRead {
        let n = usize::try_from(self.0.load(Ordering::Relaxed).saturating_sub(self.1)).unwrap().min(out.len());
        out[..n].fill(0.1);
        let run = TapRead { len: n, start_sample: self.1, ..TapRead::default() };
        self.1 += n as u64;
        run
    }
}

fn bench(c: &mut Criterion) {
    let cache = std::env::temp_dir();
    let factory = Arc::new(FakeEncoderFactory(FakeEncoderConfig {
        size: SIZE,
        fps: 30,
        gop: 30,
        key_bytes: 60_000,
        delta_bytes: 6_000,
        fail_at_frame: None,
        delay: None,
        side_data_every: Some(1),
    }));
    let encode =
        EncodeConfig::new(VideoSpec { size: SIZE, fps: (30, 1), alpha: AlphaKind::None }, CodecId::H264).unwrap();
    let available = Arc::new(AtomicU64::new(0));
    let mic = AudioTrack {
        name: "mic".into(),
        source: Box::new(Paced(Arc::clone(&available), 0)),
        rate: 48_000,
        channels: 1,
        start: TrackStart::First,
    };
    let settings = ReplaySettings::new(Seconds(60.0), 1 << 30, Backing::Memory);
    let mut core = ReplayCore::new(settings, encode, &cache, vec![mic], factory).unwrap();
    let mut i = 0u64;
    let mut step = |core: &mut ReplayCore| {
        available.store((i + 1) * 1600, Ordering::Relaxed);
        core.push_frame(&Frame::new(SIZE, i, Ticks(i as i64 * 33_333_333))).unwrap();
        i += 1;
    };
    for _ in 0..30 * 70 {
        step(&mut core);
    }
    c.bench_function("replay push_frame 1080p30, warm", |b| b.iter(|| step(&mut core)));
}

criterion_group!(benches, bench);
criterion_main!(benches);
