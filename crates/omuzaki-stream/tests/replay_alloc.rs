//! The replay core allocates nothing once warm (#1019): a counting allocator around `push_frame` (the ring push and the
//! audio alignment) on a warm, memory-backed 60 s buffer, with an encoder that hands out shared packet bytes, so every
//! allocation counted is the core's own.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::cast_possible_wrap)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use omuzaki_audio_api::ring::TapRead;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_stream::record::runtime::{AudioTrack, TrackStart};
use omuzaki_stream::replay::runtime::ReplayCore;
use omuzaki_stream::replay::settings::{Backing, ReplaySettings};
use omuzaki_stream::timeline::AudioSource;
use omuzaki_stream_api::encoder::{
    AlphaKind, CodecId, EncodeConfig, Encoder, EncoderFactory, EncoderInfo, Frame, StreamSpec, VideoSpec,
};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::packet::{Packet, StreamId};
use omuzaki_stream_api::time::{Ticks, Timebase};

/// Counts allocations on the measuring thread while it measures.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

fn measuring() -> bool {
    MEASURING.try_with(Cell::get).unwrap_or(false)
}

// SAFETY: every call forwards to the system allocator unchanged; the counter is a relaxed atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if measuring() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract for `alloc` is passed on as is.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if measuring() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract for `realloc` is passed on as is.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const SIZE: UVec2 = UVec2::new(1920, 1080);

/// An encoder whose packets share two prepared buffers (60 kB keyframes every 30 frames, 6 kB deltas), as a real
/// encoder's output arrives already made: it allocates nothing per frame.
struct Shared {
    key: Bytes,
    delta: Bytes,
    ready: Option<u64>,
    spec: StreamSpec,
}

impl Encoder for Shared {
    fn spec(&self) -> StreamSpec {
        self.spec.clone()
    }

    fn send(&mut self, frame: &Frame<'_>) -> Result<(), StreamError> {
        self.ready = Some(frame.index);
        Ok(())
    }

    fn receive(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        if let Some(index) = self.ready.take() {
            let keyframe = index % 30 == 0;
            out.push(Packet {
                stream: StreamId(0),
                pts: index as i64,
                dts: index as i64,
                duration: 1,
                keyframe,
                data: if keyframe { self.key.clone() } else { self.delta.clone() },
                side_data: Vec::new(),
            });
        }
        Ok(())
    }

    fn request_keyframe(&mut self) {}

    fn flush(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        self.receive(out)
    }
}

struct SharedFactory;

impl EncoderFactory for SharedFactory {
    fn probe(&self) -> Vec<EncoderInfo> {
        Vec::new()
    }

    fn create(&self, cfg: &EncodeConfig) -> Result<Box<dyn Encoder>, StreamError> {
        let spec = StreamSpec::video(cfg.codec, "program", Timebase { num: 1, den: 30 }, cfg.video)?;
        let (key, delta) = (Bytes::from(vec![7u8; 60_000]), Bytes::from(vec![9u8; 6_000]));
        // Share each buffer once now, so the per-frame clones only count references.
        drop((key.clone(), delta.clone()));
        Ok(Box::new(Shared { key, delta, ready: None, spec }))
    }
}

/// A microphone that has `available` samples by now.
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

#[test]
fn a_warm_memory_buffer_allocates_nothing_per_frame() {
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
    let mut core =
        ReplayCore::new(settings, encode, &std::env::temp_dir(), vec![mic], Arc::new(SharedFactory)).unwrap();
    let step = |core: &mut ReplayCore, i: u64| {
        available.store((i + 1) * 1600, Ordering::Relaxed);
        core.push_frame(&Frame::new(SIZE, i, Ticks(i as i64 * 33_333_333))).unwrap();
    };
    // Warm: past a full 60 s ring, so groups are being evicted and their allocations reused.
    let warm = 30 * 70;
    for i in 0..warm {
        step(&mut core, i);
    }
    let before = core.bytes();
    MEASURING.with(|m| m.set(true));
    for i in warm..warm + 30 * 20 {
        step(&mut core, i);
    }
    MEASURING.with(|m| m.set(false));
    let counted = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(counted, 0, "allocations in 20 s of warm push_frame");
    let buffered = core.stats();
    assert!(core.bytes() <= before + 60_000, "the meter holds steady: {before} then {}", core.bytes());
    let clip = core.select(Seconds(60.0));
    assert!(clip.chunks.len() > 30 * 60, "the ring still holds a minute: {buffered:?}");
}
