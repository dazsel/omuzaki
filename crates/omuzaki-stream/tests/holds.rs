//! Update holds (#1105, #1386; `docs/design/updates.md` §7): a take holds `Recording` and the replay buffer holds
//! `Capturing` while they run, so the app never restarts into an update mid-take; stopping, failing or dropping either
//! one releases its hold.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_stream::record::runtime::{RecordPlan, Recorder};
use omuzaki_stream::record::settings::RecordFormat;
use omuzaki_stream::replay::runtime::{ClipTarget, ReplayBuffer, ReplayPlan};
use omuzaki_stream::replay::settings::{Backing, ReplaySettings};
use omuzaki_stream::service::{Program, StreamService};
use omuzaki_stream::testing::encoder::{FakeEncoderConfig, FakeEncoderFactory};
use omuzaki_stream::testing::muxer::MemoryMuxerFactory;
use omuzaki_stream_api::encoder::CodecId;
use omuzaki_stream_api::mux::Container;
use omuzaki_stream_api::output::{Output, OutputConfig};
use omuzaki_update_api::hold::HoldReason;
use omuzaki_update_api::updater::Updater;
use omuzaki_update_mock::updater::MockUpdater;

const SIZE: UVec2 = UVec2::new(64, 36);

fn take(dir: &std::path::Path, updater: &MockUpdater) -> RecordPlan {
    RecordPlan {
        dir: dir.to_owned(),
        stem: "take".into(),
        format: RecordFormat::PngSequence,
        tracks: Vec::new(),
        hold: Some(updater.hold(HoldReason::Recording)),
        guard: None,
    }
}

#[test]
fn a_take_holds_recording_until_it_stops_or_is_dropped() {
    let (dir, updater) = (tempfile::tempdir().unwrap(), MockUpdater::new());
    let service = StreamService::new().unwrap();
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    service.record_start(take(dir.path(), &updater)).unwrap();
    assert_eq!(updater.holds(), [HoldReason::Recording]);
    service.record_stop().unwrap();
    assert_eq!(updater.holds(), []);

    // Dropped without being stopped, or never started.
    let recorder = Recorder::new(take(&dir.path().join("dropped"), &updater));
    assert_eq!(updater.holds(), [HoldReason::Recording]);
    drop(recorder);
    assert_eq!(updater.holds(), []);
}

fn replay(dir: &std::path::Path, updater: &MockUpdater, codec: CodecId) -> ReplayBuffer {
    let encoders = Arc::new(FakeEncoderFactory(FakeEncoderConfig {
        size: SIZE,
        fps: 30,
        gop: 30,
        key_bytes: 1000,
        delta_bytes: 100,
        fail_at_frame: None,
        delay: None,
        side_data_every: None,
    }));
    ReplayBuffer::new(ReplayPlan {
        settings: ReplaySettings::new(Seconds(10.0), 64 << 20, Backing::Memory),
        target: ClipTarget {
            dir: dir.to_owned(),
            container: Container::Mkv,
            extension: "omz",
            muxers: Arc::new(MemoryMuxerFactory),
        },
        cache: dir.join("cache"),
        codec,
        encoders,
        tracks: Vec::new(),
        hold: Some(updater.hold(HoldReason::Capturing)),
    })
}

#[test]
fn the_replay_buffer_holds_capturing_while_it_runs() {
    let (dir, updater) = (tempfile::tempdir().unwrap(), MockUpdater::new());
    let cfg = OutputConfig::new("replay", SIZE, (30, 1)).unwrap();
    let mut buffer = replay(dir.path(), &updater, CodecId::H264);
    buffer.start(&cfg).unwrap();
    assert_eq!(updater.holds(), [HoldReason::Capturing]);
    buffer.stop();
    assert_eq!(updater.holds(), []);

    let mut dropped = replay(dir.path(), &updater, CodecId::H264);
    dropped.start(&cfg).unwrap();
    drop(dropped);
    assert_eq!(updater.holds(), [], "dropping a running buffer releases its hold");

    // An encoder that won't open fails the start, and the hold goes with it.
    let mut refused = replay(dir.path(), &updater, CodecId::Vp9Alpha);
    assert!(refused.start(&cfg).is_err());
    assert_eq!(updater.holds(), []);
}
