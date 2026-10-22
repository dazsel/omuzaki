//! What every test of the fakes shares: the registered manifests and a run with test doubles.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use omuzaki_ai_api::Progress;
use omuzaki_connect_api::{
    ConnectError, Connector, ConnectorConfig, ConnectorManifest, Request, RunCx, RunEnv, RunOutput, Target,
};
use omuzaki_connect_mock::{FakePace, MemoryInputs, RecordingMeter};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_http_api::CancelFlag;
use omuzaki_http_mock::FakeHttp;

/// The registered manifest of `id`.
pub fn manifest(id: &str) -> ConnectorManifest {
    omuzaki_connect_mock::link();
    let reg = omuzaki_connect_api::registered().find(|r| r.id == id).expect("registered");
    reg.parse().unwrap()
}

/// Progress that turns cancelled after `after` stage reports or waits (`u32::MAX`: never).
pub struct CancelAfter {
    pub after: u32,
    pub seen: AtomicU32,
    pub cancelled: AtomicBool,
}

impl CancelAfter {
    pub fn new(after: u32) -> Self {
        Self { after, seen: AtomicU32::new(0), cancelled: AtomicBool::new(after == 0) }
    }

    /// Counts one wait; cancels once `after` are done.
    pub fn tick(&self) {
        if self.seen.fetch_add(1, Ordering::SeqCst) + 1 >= self.after {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

impl Progress for CancelAfter {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// A pace that advances the clock and ticks the progress: "cancels after the n-th wait".
pub struct TickingPace<'a> {
    pub pace: FakePace,
    pub progress: &'a CancelAfter,
}

impl omuzaki_connect_api::Pace for TickingPace<'_> {
    fn wait(&self, seconds: f64, progress: &dyn Progress) {
        self.pace.wait(seconds, progress);
        self.progress.tick();
    }
}

/// What one run used and gave.
pub struct Ran {
    pub result: Result<RunOutput, ConnectError>,
    pub meter: RecordingMeter,
    pub clock: Arc<FakeClock>,
    pub waits: u32,
}

/// Runs `c` on `req` under `m` with `cap`, cancelling after `cancel_after` waits.
pub fn run_with(c: &dyn Connector, m: &ConnectorManifest, req: &Request, cap: u64, cancel_after: u32) -> Ran {
    let target = Target::from_manifest(m, &ConnectorConfig { enabled: true, ..ConnectorConfig::default() }).unwrap();
    let http = FakeHttp::new();
    let inputs = MemoryInputs::new();
    let progress = CancelAfter::new(cancel_after);
    let meter = RecordingMeter::new();
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let pace = TickingPace { pace: FakePace::new(Arc::clone(&clock)), progress: &progress };
    let env = RunEnv { progress: &progress, cancel: Arc::new(CancelFlag::new()), meter: &meter };
    let cx = RunCx::new(&target, &http, &inputs, &env, &pace, &*clock, cap);
    let result = c.run(m, req, &cx);
    assert!(http.calls().is_empty() && http.unmatched().is_empty(), "a fake never touches a transport");
    let waits = pace.pace.waits.load(Ordering::SeqCst);
    Ran { result, meter, clock, waits }
}

/// Runs with no cap and no cancel.
pub fn run(c: &dyn Connector, m: &ConnectorManifest, req: &Request) -> Ran {
    run_with(c, m, req, u64::MAX, u32::MAX)
}
