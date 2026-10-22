//! The live outputs' update hold (#1106; `docs/design/updates.md` §7): while any live output runs the app holds one
//! `Streaming` hold, released when the last one stops or is dropped; a failed start holds nothing.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream::live_hold::LiveHolds;
use omuzaki_stream::service::{Program, PushOutcome, StreamService};
use omuzaki_stream_api::encoder::Frame;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{
    AlphaNeed, Lanes, Output, OutputConfig, OutputInfo, OutputState, OutputStatus, SubmitReport,
};
use omuzaki_stream_api::time::Ticks;
use omuzaki_update_api::hold::HoldReason;
use omuzaki_update_api::updater::Updater;
use omuzaki_update_mock::updater::MockUpdater;

const SIZE: UVec2 = UVec2::new(16, 9);

/// An output that counts what it is given; `refuse` makes its start fail.
#[derive(Default)]
struct Counting {
    refuse: bool,
    frames: Arc<AtomicU64>,
    state: Option<OutputState>,
}

impl Output for Counting {
    fn info(&self) -> OutputInfo {
        OutputInfo::new("counting", AlphaNeed::Straight, Lanes { gpu: None, cpu: true })
    }

    fn start(&mut self, _cfg: &OutputConfig) -> Result<(), StreamError> {
        if self.refuse {
            return Err(StreamError::Unavailable { what: "the device".into(), hint: "plug it in".into() });
        }
        self.state = Some(OutputState::Running);
        Ok(())
    }

    fn submit(&mut self, _frame: &Frame<'_>) -> Result<SubmitReport, StreamError> {
        self.frames.fetch_add(1, Ordering::SeqCst);
        Ok(SubmitReport::new(0.25, 0.0, 0))
    }

    fn status(&self) -> OutputStatus {
        let mut status = OutputStatus::default();
        status.state = self.state.unwrap_or(OutputState::Idle);
        status.frames = self.frames.load(Ordering::SeqCst);
        status
    }

    fn stop(&mut self) {
        self.state = Some(OutputState::Stopped);
    }
}

fn cfg() -> OutputConfig {
    OutputConfig::new("live", SIZE, (30, 1)).unwrap()
}

#[test]
fn the_first_live_output_holds_streaming_and_the_last_to_stop_releases_it() {
    let (updater, holds) = (MockUpdater::new(), LiveHolds::new());
    let mut first = holds.wrap(Counting::default(), Some(updater.hold(HoldReason::Streaming)));
    let mut second = holds.wrap(Counting::default(), Some(updater.hold(HoldReason::Streaming)));
    first.start(&cfg()).unwrap();
    second.start(&cfg()).unwrap();
    assert_eq!(updater.holds(), [HoldReason::Streaming], "one hold for all the live outputs");
    assert_eq!((holds.running(), holds.held()), (2, true));
    first.stop();
    assert_eq!(updater.holds(), [HoldReason::Streaming], "the second still runs");
    second.stop();
    assert_eq!(updater.holds(), []);
    assert_eq!((holds.running(), holds.held()), (0, false));
    second.stop();
    assert_eq!(holds.running(), 0, "a second stop counts nothing");
}

#[test]
fn a_failed_start_holds_nothing_and_a_dropped_output_counts_down() {
    let (updater, holds) = (MockUpdater::new(), LiveHolds::new());
    let mut refused =
        holds.wrap(Counting { refuse: true, ..Counting::default() }, Some(updater.hold(HoldReason::Streaming)));
    assert!(refused.start(&cfg()).is_err());
    assert_eq!((updater.holds(), holds.running()), (vec![], 0));

    let mut live = holds.wrap(Counting::default(), Some(updater.hold(HoldReason::Streaming)));
    live.start(&cfg()).unwrap();
    assert_eq!(updater.holds(), [HoldReason::Streaming]);
    drop(live);
    assert_eq!((updater.holds(), holds.running()), (vec![], 0));

    // Without an updater every call just forwards.
    let mut bare = holds.wrap(Counting::default(), None);
    bare.start(&cfg()).unwrap();
    assert_eq!((holds.running(), holds.held()), (1, false));
}

#[test]
fn a_held_output_forwards_its_frames_and_status_through_the_service() {
    let updater = MockUpdater::new();
    let service = StreamService::new().unwrap();
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    let frames = Arc::new(AtomicU64::new(0));
    let output = Counting { frames: Arc::clone(&frames), ..Counting::default() };
    let held = service.live_holds().wrap(output, Some(updater.hold(HoldReason::Streaming)));
    assert_eq!(held.info().kind, "counting");
    let id = service.start(Box::new(held), cfg()).unwrap();
    assert_eq!(updater.holds(), [HoldReason::Streaming]);

    let picture =
        Image { size: SIZE, alpha: AlphaEncoding::Premultiplied, rgba8: vec![0; (SIZE.x * SIZE.y * 4) as usize] };
    for n in 0..3 {
        assert_eq!(service.push(&picture, Ticks(n * 1_000_000_000 / 30)), PushOutcome::Queued);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while service.counters().pumped < n.unsigned_abs() + 1 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    assert_eq!(frames.load(Ordering::SeqCst), 3);
    let status = service.outputs().into_iter().find(|o| o.id == id).unwrap();
    assert_eq!((status.frames, status.kind.as_str()), (3, "counting"));
    service.stop(id).unwrap();
    assert_eq!(updater.holds(), []);
}
