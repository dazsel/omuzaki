//! The disk guard on a real take (#850): a take refuses to start on a disk about to fill, warns when it is low, and
//! stops cleanly before it fills, with its files and manifest readable. The free space is scripted.

// Integration-test helpers may panic: a panic is a clear test failure. The test reads its own temporary folder.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::disallowed_methods, clippy::cast_possible_wrap)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream::record::disk::{DiskGuard, DiskState, GuardPlan, estimate_rate};
use omuzaki_stream::record::runtime::RecordPlan;
use omuzaki_stream::record::settings::RecordFormat;
use omuzaki_stream::record::take::{StopCause, TakeReport};
use omuzaki_stream::service::{Program, PushOutcome, StreamService};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::time::Ticks;

const SIZE: UVec2 = UVec2::new(64, 36);
const FRAME_NS: i64 = 1_000_000_000 / 30;
const MIN_FREE: u64 = 1 << 20;

fn picture() -> Image {
    let mut rgba8 = Vec::new();
    for _ in 0..SIZE.x * SIZE.y {
        rgba8.extend_from_slice(&[120, 60, 30, 160]);
    }
    Image { size: SIZE, alpha: AlphaEncoding::Premultiplied, rgba8 }
}

/// A disk whose free space the test sets, asked at every frame.
fn scripted(free: &Arc<AtomicU64>) -> GuardPlan {
    let free = Arc::clone(free);
    GuardPlan {
        guard: DiskGuard { min_free: MIN_FREE, ..DiskGuard::default() },
        free: Arc::new(move |_| Ok(free.load(Ordering::SeqCst))),
        every: Duration::ZERO,
    }
}

/// The free space that leaves `seconds` of room for a PNG take of the test's size.
fn room(seconds: u64) -> u64 {
    MIN_FREE + seconds * estimate_rate(RecordFormat::PngSequence, SIZE, (30, 1))
}

fn plan(dir: &std::path::Path, guard: GuardPlan) -> RecordPlan {
    RecordPlan {
        dir: dir.to_owned(),
        stem: "take".into(),
        format: RecordFormat::PngSequence,
        tracks: Vec::new(),
        hold: None,
        guard: Some(guard),
    }
}

fn service() -> StreamService {
    let service = StreamService::new().unwrap();
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    service
}

/// Pushes frame `n` and waits (with a deadline, never a fixed sleep) until the pump has fanned it out.
fn push(service: &StreamService, n: u64) {
    assert_eq!(service.push(&picture(), Ticks(n as i64 * FRAME_NS)), PushOutcome::Queued);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while service.counters().pumped < n + 1 {
        assert!(std::time::Instant::now() < deadline, "the pump fanned out {:?}", service.counters());
        std::thread::yield_now();
    }
}

#[test]
fn a_take_refuses_to_start_on_a_disk_about_to_fill() {
    let dir = tempfile::tempdir().unwrap();
    let service = service();
    let free = Arc::new(AtomicU64::new(room(5)));
    let refused = service.record_start(plan(dir.path(), scripted(&free))).unwrap_err();
    let StreamError::Unavailable { what, hint } = refused else { panic!("{refused:?}") };
    assert_eq!(what, "room on the recordings disk");
    assert!(hint.contains("record.min_free_mb") && hint.contains("1 MiB"), "{hint}");
    assert!(!service.record_status().recording);
    assert!(!dir.path().join("take").exists(), "nothing was written");
    // Under the minimum itself is refused too.
    free.store(MIN_FREE / 2, Ordering::SeqCst);
    assert!(service.record_start(plan(dir.path(), scripted(&free))).is_err());
}

#[test]
fn a_take_warns_when_low_and_stops_cleanly_before_the_disk_fills() {
    let dir = tempfile::tempdir().unwrap();
    let service = service();
    let free = Arc::new(AtomicU64::new(1 << 40));
    service.record_start(plan(dir.path(), scripted(&free))).unwrap();
    let status = service.record_status();
    assert!(matches!(status.disk, Some(DiskState::Ok { .. })), "{status:?}");

    for n in 0..5 {
        push(&service, n);
    }
    free.store(room(100), Ordering::SeqCst);
    push(&service, 5);
    let status = service.record_status();
    let Some(DiskState::Low { seconds_left }) = status.disk else { panic!("{status:?}") };
    assert!((seconds_left - 100.0).abs() < 1.0, "{seconds_left}");
    assert!(status.recording && status.ended.is_none());

    // The disk fills: the next frame ends the take before it is written.
    free.store(room(5), Ordering::SeqCst);
    push(&service, 6);
    let status = service.record_status();
    assert_eq!((status.recording, status.ended, status.disk), (false, Some(StopCause::DiskLow), Some(DiskState::Stop)));
    push(&service, 7);

    let report: TakeReport = service.record_stop().unwrap();
    assert_eq!((report.stop_cause, report.frames, report.dropped_frames), (StopCause::DiskLow, 6, 0));
    let manifest: TakeReport = serde_json::from_slice(&std::fs::read(&report.manifest).unwrap()).unwrap();
    assert_eq!(manifest, report);
    let frames = std::fs::read_dir(&report.video).unwrap().count();
    assert_eq!(frames, 6, "every frame before the stop is on disk");
}

#[test]
fn a_take_that_ended_on_its_own_makes_way_for_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let service = service();
    let free = Arc::new(AtomicU64::new(1 << 40));
    service.record_start(plan(dir.path(), scripted(&free))).unwrap();
    push(&service, 0);
    free.store(room(1), Ordering::SeqCst);
    push(&service, 1);
    assert_eq!(service.record_status().ended, Some(StopCause::DiskLow));
    assert!(dir.path().join("take.take.json").exists(), "its manifest is on disk");

    free.store(1 << 40, Ordering::SeqCst);
    let mut next = plan(dir.path(), scripted(&free));
    next.stem = "next".into();
    service.record_start(next).unwrap();
    assert!(service.record_status().recording);
    assert_eq!(service.record_stop().unwrap().stop_cause, StopCause::Requested);
}

#[test]
fn a_disk_that_cannot_be_asked_never_stops_a_take() {
    let dir = tempfile::tempdir().unwrap();
    let service = service();
    let guard = GuardPlan {
        guard: DiskGuard::default(),
        free: Arc::new(|_| Err(std::io::Error::other("no answer"))),
        every: Duration::ZERO,
    };
    service.record_start(plan(dir.path(), guard)).unwrap();
    push(&service, 0);
    let status = service.record_status();
    assert_eq!((status.recording, status.disk), (true, None));
    assert_eq!(service.record_stop().unwrap().frames, 1);
}
