//! The replay buffer (`docs/design/record.md` D3 to D5; #1017, #1018, #1019): the rolling buffer's rules, the disk
//! segment store, saves from the single-threaded core on fake time, the threaded output, and a real VP9 alpha clip
//! when libvpx loads.

// Integration-test helpers may panic: a panic is a clear test failure. The tests read their own temporary folders and
// cast small known values.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp
)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use omuzaki_audio_api::ring::TapRead;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream::record::runtime::{AudioTrack, TrackStart};
use omuzaki_stream::replay::ring::{Caps, Charge, ChunkKind, Payload, Push, RollingBuffer};
use omuzaki_stream::replay::runtime::{
    ClipTarget, ReplayBuffer, ReplayCore, ReplayError, ReplayPlan, SaveReport, Stored, write_clip,
};
use omuzaki_stream::replay::segment::SegmentStore;
use omuzaki_stream::replay::settings::{Backing, ReplaySettings};
use omuzaki_stream::testing::encoder::{FakeEncoderConfig, FakeEncoderFactory};
use omuzaki_stream::testing::muxer::{MemoryMuxerFactory, read_test_container};
use omuzaki_stream::timeline::AudioSource;
use omuzaki_stream_api::encoder::{AlphaKind, CodecId, EncodeConfig, Frame, VideoSpec};
use omuzaki_stream_api::mux::Container;
use omuzaki_stream_api::output::{Output, OutputConfig};
use omuzaki_stream_api::packet::{ByteMeter, Packet, StreamId};
use omuzaki_stream_api::time::Ticks;
use proptest::prelude::*;

const MIB: u64 = 1 << 20;
const SIZE: UVec2 = UVec2::new(64, 36);

/// A payload of `n` bytes for the ring's own tests.
#[derive(Clone, Debug)]
struct Blob(Arc<Charge>);

impl Payload for Blob {
    fn len(&self) -> u64 {
        self.0.bytes()
    }
}

fn push(ring: &mut RollingBuffer<Blob>, kind: ChunkKind, pts: f64, duration: f64, len: u64) -> Push {
    ring.push_with(kind, Seconds(pts), Seconds(duration), len, |c| Ok::<_, ()>(Blob(Arc::new(c)))).unwrap()
}

const fn video(keyframe: bool) -> ChunkKind {
    ChunkKind::Video { keyframe }
}

/// Pushes `seconds` of 30 fps video with a keyframe every `gop` frames, `key` and `delta` bytes, checking `each`
/// after every push.
fn fill(
    ring: &mut RollingBuffer<Blob>,
    seconds: u32,
    gop: u32,
    key: u64,
    delta: u64,
    each: impl Fn(&RollingBuffer<Blob>),
) {
    for f in 0..seconds * 30 {
        let k = f % gop == 0;
        push(ring, video(k), f64::from(f) / 30.0, 1.0 / 30.0, if k { key } else { delta });
        each(ring);
    }
}

#[test]
fn the_ring_keeps_the_span_and_starts_on_a_keyframe() {
    let mut ring = RollingBuffer::new(Caps { max_span: Seconds(60.0), max_bytes: 512 * MIB }).unwrap();
    fill(&mut ring, 100, 30, 60_000, 6_000, |_| {});
    let stats = ring.stats();
    assert!((60.0..=61.0).contains(&stats.span.0), "{stats:?}");
    let sel = ring.select(Seconds(60.0));
    assert_eq!(sel.chunks[0].kind, video(true));
    assert!(sel.early_by.0 < 1.0 && sel.short_by.0 == 0.0, "{:?} {:?}", sel.early_by, sel.short_by);
    let ten = ring.select(Seconds(10.0));
    assert!((ten.start.0 - 90.0).abs() < 1e-9, "{:?}", ten.start);
    assert_eq!(ring.meter().get(), stats.bytes);
}

#[test]
fn the_byte_cap_binds_and_a_short_buffer_says_so() {
    let mut ring = RollingBuffer::new(Caps { max_span: Seconds(60.0), max_bytes: 64 * MIB }).unwrap();
    // 2.34 MB a second: a 600 kB keyframe and 29 deltas of 60 kB.
    fill(&mut ring, 120, 30, 600_000, 60_000, |r| assert!(r.meter().get() <= 64 * MIB));
    let stats = ring.stats();
    assert!((27.5..=29.0).contains(&stats.span.0), "{stats:?}");
    let sel = ring.select(Seconds(60.0));
    assert!((sel.short_by.0 - 31.0).abs() < 1.5, "{:?}", sel.short_by);
}

#[test]
fn a_group_over_the_cap_is_refused_and_a_delta_first_is_skipped() {
    let mut ring = RollingBuffer::new(Caps { max_span: Seconds(10.0), max_bytes: MIB }).unwrap();
    assert_eq!(push(&mut ring, video(false), 0.0, 0.033, 10), Push::Skipped);
    assert_eq!(push(&mut ring, ChunkKind::Audio { track: 0 }, 0.0, 0.033, 10), Push::Skipped);
    assert_eq!(push(&mut ring, video(true), 0.0, 0.033, 5 * MIB), Push::Refused);
    assert!(ring.is_empty());
    assert_eq!(ring.stats().refused, 1);
    // A group that grows past the cap is dropped whole, and the buffer waits for the next keyframe.
    assert!(matches!(push(&mut ring, video(true), 1.0, 0.033, MIB / 2), Push::Kept { .. }));
    assert_eq!(push(&mut ring, video(false), 1.033, 0.033, MIB / 2 + 1), Push::Refused);
    assert!(ring.is_empty() && ring.meter().get() == 0);
    assert_eq!(push(&mut ring, video(false), 1.066, 0.033, 10), Push::Skipped);
    assert!(matches!(push(&mut ring, video(true), 2.0, 0.033, 10), Push::Kept { .. }));
}

#[test]
fn bytes_a_save_holds_keep_counting_until_it_lets_go() {
    let meter = ByteMeter::new();
    let mut ring =
        RollingBuffer::with_meter(Caps { max_span: Seconds(300.0), max_bytes: 4 * MIB }, meter.clone()).unwrap();
    // 1 MiB groups of one keyframe each.
    for s in 0..3 {
        push(&mut ring, video(true), f64::from(s), 1.0, MIB);
    }
    let pinned = ring.select(Seconds(300.0));
    assert_eq!(pinned.chunks.len(), 3);
    // Three more groups: the ring evicts the pinned ones, but their bytes still count, so the cap holds.
    for s in 3..6 {
        let pushed = push(&mut ring, video(true), f64::from(s), 1.0, MIB);
        assert!(meter.get() <= 4 * MIB, "{pushed:?}");
    }
    assert!(ring.stats().bytes <= MIB, "{:?}", ring.stats());
    drop(pinned);
    assert_eq!(meter.get(), ring.stats().bytes);
    for s in 6..9 {
        push(&mut ring, video(true), f64::from(s), 1.0, MIB);
    }
    assert_eq!(ring.stats().bytes, 4 * MIB);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn the_ring_invariants_hold(
        gop in 1u32..60,
        sizes in proptest::collection::vec((1u64..400_000, 0u8..4), 50..600),
        cap_mib in 1u64..8,
        length in 1.0f64..20.0,
    ) {
        let cap = cap_mib * MIB;
        let mut ring = RollingBuffer::new(Caps { max_span: Seconds(10.0), max_bytes: cap }).unwrap();
        let mut binding = false;
        for (f, (size, extra)) in sizes.iter().enumerate() {
            let t = f as f64 / 30.0;
            let key = (f as u32).is_multiple_of(gop);
            binding |= push(&mut ring, video(key), t, 1.0 / 30.0, *size) == Push::Refused;
            for i in 0..*extra {
                let kind = if i == 0 { ChunkKind::Input } else { ChunkKind::Audio { track: i } };
                push(&mut ring, kind, t + 0.001, 0.01, size / 7 + 1);
            }
            prop_assert!(ring.meter().get() <= cap);
            let stats = ring.stats();
            prop_assert_eq!(stats.bytes, ring.meter().get());
            binding |= stats.groups_evicted > 0 && stats.span.0 < 10.0;
            let sel = ring.select(Seconds(length));
            if let Some(first) = sel.chunks.iter().find(|c| matches!(c.kind, ChunkKind::Video { .. })) {
                prop_assert_eq!(first.kind, video(true));
            }
            for c in sel.chunks.iter().filter(|c| matches!(c.kind, ChunkKind::Video { .. })) {
                prop_assert!(c.pts.0 >= sel.start.0);
            }
            if !binding && !sel.chunks.is_empty() && length <= stats.span.0 {
                prop_assert!(sel.actual.0 + 1e-9 >= length);
            }
        }
    }
}

fn packet(n: usize, key: bool) -> Packet {
    Packet {
        stream: StreamId(0),
        pts: 0,
        dts: 0,
        duration: 1,
        keyframe: key,
        data: Bytes::from(vec![7u8; n]),
        side_data: Vec::new(),
    }
}

fn folder_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir).unwrap().filter_map(Result::ok).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()
}

#[test]
fn the_segment_store_sweeps_orphans_and_frees_a_file_with_its_last_slice() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["omuzaki-replay-1.seg", "omuzaki-replay-2.seg", "a.tmp", "notes.txt"] {
        std::fs::write(dir.path().join(name), b"left over").unwrap();
    }
    let store = SegmentStore::open(dir.path()).unwrap();
    let names: Vec<String> =
        std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names, ["notes.txt"]);

    let meter = ByteMeter::new();
    let p = packet(1000, true);
    let len = SegmentStore::record_len(&p);
    let a = store.put(&p, Charge::new(&meter, len), true).unwrap();
    let b = store
        .put(&packet(500, false), Charge::new(&meter, SegmentStore::record_len(&packet(500, false))), false)
        .unwrap();
    assert_eq!(a.path(), b.path(), "one file per group");
    assert_eq!(a.read().unwrap(), p);
    assert_eq!(meter.get(), folder_bytes(dir.path()) - 9, "the meter counts the file's bytes");
    let held = a.clone();
    drop((a, b));
    store.close();
    assert!(held.path().exists(), "a save's clone keeps the file");
    let path = held.path().to_owned();
    drop(held);
    assert!(!path.exists());
    assert_eq!(meter.get(), 0);
}

/// An audio source that has `available` samples of 0.25 so far, mono.
struct Paced {
    available: Arc<AtomicU64>,
    pos: u64,
}

impl AudioSource for Paced {
    fn read(&mut self, out: &mut [f32]) -> TapRead {
        let n = (self.available.load(Ordering::Relaxed).saturating_sub(self.pos) as usize).min(out.len());
        out[..n].fill(0.25);
        let run = TapRead { len: n, start_sample: self.pos, ..TapRead::default() };
        self.pos += n as u64;
        run
    }
}

fn fake(key_bytes: usize, delta_bytes: usize, fail_at_frame: Option<u64>) -> Arc<FakeEncoderFactory> {
    Arc::new(FakeEncoderFactory(FakeEncoderConfig {
        size: SIZE,
        fps: 30,
        gop: 30,
        key_bytes,
        delta_bytes,
        fail_at_frame,
        delay: None,
        side_data_every: Some(1),
    }))
}

fn h264() -> EncodeConfig {
    EncodeConfig::new(VideoSpec { size: SIZE, fps: (30, 1), alpha: AlphaKind::None }, CodecId::H264).unwrap()
}

fn mic(available: &Arc<AtomicU64>) -> AudioTrack {
    AudioTrack {
        name: "mic".into(),
        source: Box::new(Paced { available: Arc::clone(available), pos: 0 }),
        rate: 48_000,
        channels: 1,
        start: TrackStart::First,
    }
}

/// A core over the fake encoder with one paced mic track, and the counter that paces it.
fn core(settings: ReplaySettings, cache: &Path, factory: Arc<FakeEncoderFactory>) -> (ReplayCore, Arc<AtomicU64>) {
    let available = Arc::new(AtomicU64::new(0));
    let core = ReplayCore::new(settings, h264(), cache, vec![mic(&available)], factory).unwrap();
    (core, available)
}

/// Pushes frames `from..to` at 30 fps, the mic 1,600 samples ahead of each.
fn run(core: &mut ReplayCore, available: &AtomicU64, frames: std::ops::Range<u64>) {
    for i in frames {
        available.store((i + 1) * 1600, Ordering::Relaxed);
        let frame = Frame::new(SIZE, i, Ticks(i as i64 * 33_333_333));
        core.push_frame(&frame).unwrap();
    }
}

fn memory_target(dir: &Path) -> ClipTarget {
    ClipTarget {
        dir: dir.to_owned(),
        container: Container::Mkv,
        extension: "omz",
        muxers: Arc::new(MemoryMuxerFactory),
    }
}

fn save(core: &ReplayCore, dir: &Path, length: f64) -> (SaveReport, Vec<Packet>) {
    let sel = core.select(Seconds(length));
    let report = write_clip(&sel, &core.spec(), &core.tracks(), &memory_target(dir)).unwrap();
    let packets = read_test_container(&report.path).unwrap();
    (report, packets)
}

fn frame_index(p: &Packet) -> u64 {
    u64::from_le_bytes(p.data[..8].try_into().unwrap())
}

fn wav_samples(path: &Path) -> Vec<f32> {
    hound::WavReader::open(path).unwrap().into_samples::<f32>().map(Result::unwrap).collect()
}

#[test]
fn a_save_holds_the_last_minute_from_a_keyframe_with_its_audio() {
    let (dir, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let settings = ReplaySettings::new(Seconds(60.0), 64 * MIB, Backing::Memory);
    let (mut core, available) = core(settings, cache.path(), fake(60_000, 6_000, None));
    run(&mut core, &available, 0..3000);
    let (report, packets) = save(&core, dir.path(), 60.0);
    assert_eq!(report.first_frame, 1200);
    assert!(packets[0].keyframe);
    assert_eq!(frame_index(&packets[0]), 1200);
    assert_eq!(frame_index(packets.last().unwrap()), 2999);
    assert!(packets.windows(2).all(|w| w[1].pts == w[0].pts + 1), "contiguous frames");
    assert!(packets.iter().all(|p| p.side_data.len() == 1), "the side data travels with every packet");
    assert_eq!((report.early_by_s, report.gaps), (0.0, 0));
    let mic = wav_samples(&report.tracks[0]);
    assert!((mic.len() as i64 - 48_000 * 60).abs() <= 1600, "{}", mic.len());
    assert!(mic.iter().all(|&s| s == 0.25));

    // Half a second later the clip still starts at the keyframe at or before frame 1215.
    run(&mut core, &available, 3000..3015);
    let (report, packets) = save(&core, dir.path(), 60.0);
    assert_eq!(frame_index(&packets[0]), 1200);
    assert!((report.early_by_s - 0.5).abs() < 1e-6, "{}", report.early_by_s);
    println!("clip of {} bytes written in {:.3} s", report.bytes, report.took_s);
}

#[test]
fn the_cap_binds_in_memory_and_on_disk_and_a_save_still_works() {
    for backing in [Backing::Memory, Backing::Disk] {
        let (dir, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let settings = ReplaySettings::new(Seconds(60.0), 64 * MIB, backing);
        let (mut core, available) = core(settings, cache.path(), fake(600_000, 60_000, None));
        // 10 simulated minutes at 2.34 MB/s of video, plus the mic.
        for i in 0..18_000 {
            run(&mut core, &available, i..i + 1);
            assert!(core.bytes() <= 64 * MIB);
            if backing == Backing::Disk && i % 900 == 0 {
                assert!(folder_bytes(cache.path()) <= 64 * MIB);
            }
        }
        // 2.34 MB/s of video and 192 kB/s of mic: the cap holds this many seconds, less up to one group.
        let holds = (64 * MIB) as f64 / 2_532_000.0;
        let buffered = core.stats().span.0;
        assert!((holds - 1.2..=holds).contains(&buffered), "{backing:?} {buffered} of {holds}");
        let (report, packets) = save(&core, dir.path(), 60.0);
        assert!((holds - 1.2..=holds).contains(&report.actual_s), "{backing:?} {report:?}");
        assert_eq!(report.requested_s, 60.0);
        assert!(packets[0].keyframe);
    }
}

#[test]
fn a_disk_buffer_dropped_without_stopping_leaves_orphans_the_next_start_removes() {
    let cache = tempfile::tempdir().unwrap();
    let settings = ReplaySettings::new(Seconds(10.0), 64 * MIB, Backing::Disk);
    let (mut core, available) = core(settings, cache.path(), fake(6_000, 600, None));
    run(&mut core, &available, 0..90);
    assert!(folder_bytes(cache.path()) > 0);
    std::mem::forget(core);
    let (_core, _) = self::core(settings, cache.path(), fake(6_000, 600, None));
    assert_eq!(folder_bytes(cache.path()), 0);
}

#[test]
fn an_encoder_failure_restarts_it_with_a_keyframe_and_the_clip_decodes_across_it() {
    let (dir, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let settings = ReplaySettings::new(Seconds(60.0), 64 * MIB, Backing::Memory);
    let (mut core, available) = core(settings, cache.path(), fake(6_000, 600, Some(500)));
    run(&mut core, &available, 0..900);
    assert_eq!(core.restarts(), 1);
    let (_, packets) = save(&core, dir.path(), 60.0);
    assert!(packets[0].keyframe);
    assert!(packets.windows(2).all(|w| w[1].dts > w[0].dts), "dts monotonic");
    assert_eq!(packets.len(), 900, "no frame lost");
    assert!(packets[500].keyframe, "a keyframe where the fresh encoder took over");
}

/// A threaded buffer over the fake encoder, started at 30 fps.
fn threaded(dir: &Path, cache: &Path) -> (ReplayBuffer, Arc<AtomicU64>) {
    let available = Arc::new(AtomicU64::new(0));
    let plan = ReplayPlan {
        settings: ReplaySettings::new(Seconds(60.0), 64 * MIB, Backing::Memory),
        target: memory_target(dir),
        cache: cache.to_owned(),
        codec: CodecId::H264,
        encoders: fake(60_000, 6_000, None),
        tracks: vec![mic(&available)],
        hold: None,
    };
    let mut buffer = ReplayBuffer::new(plan);
    buffer.start(&OutputConfig::new("replay", SIZE, (30, 1)).unwrap()).unwrap();
    (buffer, available)
}

fn image() -> Image {
    Image { size: SIZE, alpha: AlphaEncoding::Straight, rgba8: vec![0; (SIZE.x * SIZE.y * 4) as usize] }
}

/// Waits until the encoder thread has taken frame `index`, and says how long that took.
fn caught_up(handle: &omuzaki_stream::replay::runtime::ReplayHandle, index: u64) -> Duration {
    let started = Instant::now();
    while handle.status().frames <= index {
        assert!(started.elapsed() < Duration::from_secs(5), "the encoder thread stalled at frame {index}");
        std::thread::yield_now();
    }
    started.elapsed()
}

#[test]
fn the_threaded_buffer_saves_while_frames_keep_coming_and_a_third_save_is_busy() {
    let (dir, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut buffer, available) = threaded(dir.path(), cache.path());
    let handle = buffer.handle();
    assert_eq!(handle.save(None).unwrap().wait().unwrap_err(), ReplayError::Empty);
    let picture = image();
    let (mut tickets, mut longest_submit, mut longest_encode) = (Vec::new(), Duration::ZERO, Duration::ZERO);
    for i in 0..3000u64 {
        available.store((i + 1) * 1600, Ordering::Relaxed);
        let frame = Frame::new(SIZE, i, Ticks(i as i64 * 33_333_333)).with_cpu(&picture);
        let started = Instant::now();
        let submitted = buffer.submit(&frame).unwrap();
        let submit = started.elapsed();
        assert_eq!(submitted.dropped, 0, "frame {i} dropped");
        if i == 2000 || i == 2010 {
            tickets.push(handle.save(None).unwrap());
        }
        if i == 2011 && tickets.iter().all(|t| t.try_wait().is_none()) {
            // Both saves still outstanding: a third is refused.
            assert_eq!(handle.save(None).unwrap_err(), ReplayError::Busy);
        }
        // A real source sends the next frame a frame period later; here it comes as soon as this one is taken.
        let encode = caught_up(&handle, i);
        if (2000..2100).contains(&i) {
            longest_submit = longest_submit.max(submit);
            longest_encode = longest_encode.max(encode);
        }
    }
    let reports: Vec<SaveReport> = tickets.into_iter().map(|t| t.wait().unwrap()).collect();
    for report in &reports {
        let packets = read_test_container(&report.path).unwrap();
        assert!(packets[0].keyframe);
        assert!(packets.windows(2).all(|w| w[1].pts == w[0].pts + 1));
        assert!((59.0..=61.0).contains(&report.actual_s), "{report:?}");
    }
    let status = handle.status();
    buffer.stop();
    assert_eq!(status.dropped_frames, 0);
    assert_eq!(status.last_save.as_ref().map(|s| &s.path), Some(&reports[1].path));
    // Ceilings that hold on a loaded CI runner in a debug build: far under the 33 ms frame period.
    assert!(longest_submit < Duration::from_millis(10), "the longest submit during the saves took {longest_submit:?}");
    assert!(longest_encode < Duration::from_millis(33), "a frame waited {longest_encode:?} for the encoder thread");
    assert_eq!(handle.save(None).unwrap_err(), ReplayError::Disabled);
    println!(
        "during saves: longest submit {longest_submit:?}, longest wait for the encoder {longest_encode:?}; saves took {:.3} s and {:.3} s",
        reports[0].took_s, reports[1].took_s
    );
}

#[test]
fn a_vp9_alpha_clip_decodes_with_its_alpha() {
    if let Err(e) = omuzaki_stream::vp9::available() {
        eprintln!("skipped: {e}");
        return;
    }
    let (dir, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let size = UVec2::new(64, 36);
    let plan = ReplayPlan::vp9(
        ReplaySettings::new(Seconds(5.0), 64 * MIB, Backing::Memory),
        dir.path().to_owned(),
        cache.path().to_owned(),
        Vec::new(),
    );
    let mut buffer = ReplayBuffer::new(plan);
    buffer.start(&OutputConfig::new("replay", size, (30, 1)).unwrap()).unwrap();
    // A half-transparent picture: alpha 128 everywhere.
    let mut rgba8 = Vec::new();
    for _ in 0..size.x * size.y {
        rgba8.extend_from_slice(&[200, 100, 50, 128]);
    }
    let picture = Image { size, alpha: AlphaEncoding::Straight, rgba8 };
    for i in 0..90u64 {
        buffer.submit(&Frame::new(size, i, Ticks(i as i64 * 33_333_333)).with_cpu(&picture)).unwrap();
        std::thread::sleep(Duration::from_millis(3));
    }
    std::thread::sleep(Duration::from_millis(200));
    let report = buffer.handle().save(Some(Seconds(2.0))).unwrap().wait().unwrap();
    buffer.stop();
    let info = omuzaki_stream::sinks::webm::read(&std::fs::read(&report.path).unwrap()).unwrap();
    assert!(info.frames.len() >= 60, "{} frames", info.frames.len());
    assert!(info.frames[0].key);
    assert!(info.frames.iter().all(|f| f.alpha.is_some()), "every frame carries its alpha");
    let mut decoder = omuzaki_stream::vp9::Vp9Decoder::new().unwrap();
    let mut plane = omuzaki_stream::vp9::I420::default();
    decoder.decode(info.frames[0].alpha.as_deref().unwrap(), &mut plane).unwrap();
    let mean = plane.y.iter().map(|&v| f64::from(v)).sum::<f64>() / plane.y.len() as f64;
    assert!((mean - 128.0).abs() < 3.0, "alpha decodes to {mean}");
}

#[test]
fn a_stored_payload_reads_back_from_memory_and_disk() {
    let cache = tempfile::tempdir().unwrap();
    for backing in [Backing::Memory, Backing::Disk] {
        let settings = ReplaySettings::new(Seconds(10.0), 64 * MIB, backing);
        let (mut core, available) = core(settings, cache.path(), fake(1000, 100, None));
        run(&mut core, &available, 0..40);
        let sel = core.select(Seconds(10.0));
        let first: &Stored = &sel.chunks[0].data;
        let packet = first.packet().unwrap();
        assert_eq!((frame_index(&packet), packet.keyframe, first.len() > 1000), (0, true, true));
    }
}
