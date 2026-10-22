//! The remote encoder to its acceptance (#1224; `docs/design/supervisor.md` §15.5, record.md D11): the `fake-worker`
//! binary as a supervised encoder worker over shared frame slots and a packet channel. Packets and their side data
//! cross byte for byte; `kill -9` mid-take restarts it without ending the take, emptying the ring or losing a frame;
//! a hung worker surfaces as an encoder failure and is replaced; the take's part rule follows the new extradata.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_core::time::{Seconds, SystemClock};
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream::encode_remote::{EncoderPeer, RemoteEncoders, same_part};
use omuzaki_stream::replay::ring::ChunkKind;
use omuzaki_stream::replay::runtime::ReplayCore;
use omuzaki_stream::replay::settings::{Backing, ReplaySettings};
use omuzaki_stream::watchdog::launch::process::{ProcessLauncher, WorkerCommand};
use omuzaki_stream::watchdog::{NoHandoff, RestartReason, Supervisor, SupervisorConfig, WorkerKind, WorkerSpec};
use omuzaki_stream_api::encoder::{AlphaKind, CodecId, EncodeConfig, EncoderFactory, Frame, VideoSpec};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::packet::{Packet, SideKind};
use omuzaki_stream_api::time::Ticks;

const FAKE_WORKER: &str = env!("CARGO_BIN_EXE_fake-worker");
const SIZE: UVec2 = UVec2::new(16, 8);
const MIB: u64 = 1024 * 1024;

fn config() -> EncodeConfig {
    EncodeConfig::new(VideoSpec { size: SIZE, fps: (30, 1), alpha: AlphaKind::None }, CodecId::H264).unwrap()
}

struct Rig {
    sup: Arc<Supervisor>,
    factory: Arc<RemoteEncoders>,
}

/// A supervised fake encoder worker with `args`, and the factory over it.
fn rig(args: &[&str]) -> Rig {
    let peer = Arc::new(EncoderPeer::new(config(), 4).unwrap());
    let mut all = vec!["--role", "encoder"];
    all.extend_from_slice(args);
    let launcher = ProcessLauncher::new(WorkerCommand::new(FAKE_WORKER, all), WorkerKind::Encoder, peer.clone());
    let mut spec = WorkerSpec::for_encoder("encoder", 30.0, 4);
    spec.hung_after = Seconds(0.5);
    let sup = Supervisor::start(
        vec![(spec, Box::new(launcher))],
        Arc::new(NoHandoff),
        Arc::new(SystemClock::new()),
        SupervisorConfig::default(),
    )
    .unwrap();
    let sup = Arc::new(sup);
    let id = sup.ids()[0];
    let factory = Arc::new(RemoteEncoders::new(Arc::clone(&sup), id, peer));
    Rig { sup, factory }
}

impl Rig {
    fn pid(&self) -> u32 {
        let wait = Instant::now();
        loop {
            if let Some(pid) = self.sup.status()[0].pid {
                return pid;
            }
            assert!(wait.elapsed() < Duration::from_secs(5), "no encoder process");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn kill_9(&self) -> u32 {
        let pid = self.pid();
        let raw = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        rustix::process::kill_process(raw, rustix::process::Signal::KILL).unwrap();
        pid
    }
}

/// Frame `index`'s pixels: different for every frame.
fn image(index: u64) -> Image {
    let rgba8 = (0..SIZE.x * SIZE.y * 4).map(|i| (u64::from(i) * 7 + index * 13) as u8).collect();
    Image::new(SIZE, AlphaEncoding::Straight, rgba8).unwrap()
}

fn index_of(packet: &Packet) -> u64 {
    let side = &packet.side_data[0];
    u64::from_le_bytes(side.data.as_ref().try_into().unwrap())
}

#[test]
fn packets_and_side_data_cross_byte_for_byte() {
    let rig = rig(&["--extradata", "codec-private"]);
    let mut encoder = rig.factory.create(&config()).unwrap();
    assert_eq!(encoder.spec().extradata.as_ref(), b"codec-private");
    let mut packets = Vec::new();
    for index in 0..40_u64 {
        if index == 20 {
            encoder.request_keyframe();
        }
        let pixels = image(index);
        let frame = Frame::new(SIZE, index, Ticks(0)).with_cpu(&pixels);
        let started = Instant::now();
        loop {
            match encoder.send(&frame) {
                Ok(()) => break,
                Err(StreamError::Full) => encoder.receive(&mut packets).unwrap(),
                Err(e) => panic!("{e}"),
            }
            assert!(started.elapsed() < Duration::from_secs(2), "no slot came back");
            std::thread::sleep(Duration::from_millis(1));
        }
        encoder.receive(&mut packets).unwrap();
    }
    encoder.flush(&mut packets).unwrap();
    assert_eq!(packets.len(), 40);
    for (index, packet) in (0_u64..).zip(&packets) {
        assert_eq!(packet.pts, index as i64);
        assert_eq!(packet.data.as_ref(), image(index).rgba8.as_slice(), "frame {index}'s bytes");
        assert_eq!(packet.side_data.len(), 1);
        assert_eq!(packet.side_data[0].kind, SideKind::BlockAdditional { id: 1 });
        assert_eq!(index_of(packet), index, "side data byte for byte");
        assert_eq!(packet.keyframe, index == 0 || index == 20, "frame {index}");
    }
    assert!(matches!(encoder.send(&Frame::new(SIZE, 40, Ticks(0)).with_cpu(&image(40))), Err(StreamError::Stopped)));
}

/// Pushes frames `range` into `core` every `pace`, taking ready packets in between.
fn push(core: &mut ReplayCore, range: std::ops::Range<u64>, pace: Duration) {
    for index in range {
        let pixels = image(index);
        let frame = Frame::new(SIZE, index, Ticks(index as i64 * 33_333_333)).with_cpu(&pixels);
        core.push_frame(&frame).unwrap();
        std::thread::sleep(pace);
        core.drain().unwrap();
    }
}

/// The ring's video packets, in order, once `count` of them are in (or 3 s have passed).
fn video(core: &mut ReplayCore, count: usize) -> Vec<(u64, bool, Packet)> {
    let started = Instant::now();
    loop {
        core.drain().unwrap();
        let chunks = core.select(Seconds(1e6)).chunks;
        let packets: Vec<_> = chunks
            .into_iter()
            .filter_map(|chunk| match chunk.kind {
                ChunkKind::Video { keyframe } => Some((keyframe, chunk.data.packet().unwrap())),
                _ => None,
            })
            .map(|(keyframe, packet)| (index_of(&packet), keyframe, packet))
            .collect();
        if packets.len() >= count || started.elapsed() > Duration::from_secs(3) {
            return packets;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn kill_9_mid_take_restarts_with_a_keyframe_and_loses_no_frame() {
    let rig = rig(&[]);
    let cache = tempfile::tempdir().unwrap();
    let settings = ReplaySettings::new(Seconds(60.0), 64 * MIB, Backing::Memory);
    let mut core = ReplayCore::new(settings, config(), cache.path(), Vec::new(), rig.factory.clone()).unwrap();
    push(&mut core, 0..45, Duration::from_millis(3));
    let before = core.bytes();
    let killed = rig.kill_9();
    push(&mut core, 45..90, Duration::from_millis(3));
    let packets = video(&mut core, 90);
    assert_eq!(core.restarts(), 1, "encoder_restarts");
    assert!(core.bytes() >= before, "the ring was not emptied: {} then {}", before, core.bytes());
    assert_ne!(rig.pid(), killed);
    let indexes: Vec<u64> = packets.iter().map(|(i, _, _)| *i).collect();
    assert_eq!(indexes, (0..90).collect::<Vec<_>>(), "every frame once, in order ({} skipped)", core.skipped());
    for (index, _, packet) in &packets {
        assert_eq!(packet.data.as_ref(), image(*index).rgba8.as_slice(), "frame {index}");
    }
    let keyframes: Vec<u64> = packets.iter().filter(|(_, k, _)| *k).map(|(i, _, _)| *i).collect();
    assert_eq!(keyframes.len(), 2, "the take's first frame and the first after the restart: {keyframes:?}");
    let log = rig.sup.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].reason, RestartReason::crashed(None));
    eprintln!("restart after kill -9 at frame {}; requeued {}", keyframes[1], core.requeued());
}

#[test]
fn a_hung_worker_fails_the_encoder_and_is_replaced() {
    let rig = rig(&["--hang-after-ms", "300", "--first-generation-only"]);
    let cache = tempfile::tempdir().unwrap();
    let settings = ReplaySettings::new(Seconds(60.0), 64 * MIB, Backing::Memory);
    let mut core = ReplayCore::new(settings, config(), cache.path(), Vec::new(), rig.factory.clone()).unwrap();
    let first = rig.pid();
    // 1.5 s at 100 fps: the hang starts at 0.3 s and is found after 0.5 s of silence.
    push(&mut core, 0..150, Duration::from_millis(10));
    assert_ne!(rig.pid(), first);
    let log = rig.sup.log();
    assert!(matches!(log[0].reason, RestartReason::Hung { .. }), "{:?}", log[0].reason);
    // Every frame is in the ring once, or was skipped while the hung worker held every slot; none twice.
    let expected = 150 - core.skipped() as usize;
    let packets = video(&mut core, expected);
    let mut indexes: Vec<u64> = packets.iter().map(|(i, _, _)| *i).collect();
    let count = indexes.len();
    indexes.dedup();
    assert_eq!(indexes.len(), count, "a frame encoded twice");
    assert!(indexes.windows(2).all(|w| w[0] < w[1]), "out of order");
    assert_eq!(count as u64 + core.skipped(), 150, "every frame encoded or counted as skipped");
    assert_eq!(core.restarts(), 1);
    assert!(core.requeued() >= 1, "the frames the hung worker held were encoded by the next one");
    eprintln!("hang: {} skipped, {} requeued", core.skipped(), core.requeued());
}

/// The specs of the first two generations, the second after a `kill -9` of the first.
fn two_generations(args: &[&str]) -> (u32, bool) {
    let rig = rig(args);
    let first = rig.factory.create(&config()).unwrap();
    rig.kill_9();
    let second = rig.factory.create(&config()).unwrap();
    let (first, second) = (first.spec(), second.spec());
    let parts = if same_part(&first, &second) { 1 } else { 2 };
    (parts, first.extradata != second.extradata)
}

#[test]
fn the_take_continues_with_the_same_extradata_and_rolls_with_new() {
    let (parts, changed) = two_generations(&["--extradata", "same"]);
    assert_eq!((parts, changed), (1, false));
    let (parts, changed) = two_generations(&["--extradata", "x", "--extradata-per-generation"]);
    assert_eq!((parts, changed), (2, true));
}

#[test]
fn another_configuration_is_refused() {
    let rig = rig(&[]);
    let other =
        EncodeConfig::new(VideoSpec { size: UVec2::new(32, 32), fps: (30, 1), alpha: AlphaKind::None }, CodecId::H264)
            .unwrap();
    assert!(matches!(rig.factory.create(&other), Err(StreamError::Config(_))));
}
