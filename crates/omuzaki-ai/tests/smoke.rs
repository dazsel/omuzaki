//! Every registered slot's smoke test runs on the mock runtime: given a fake session of the slot's signature, it makes
//! a report (passed or not) within seconds, as ADR 0016 rule 4 asks of the test a user model must pass. Covers the
//! slots linked into this binary; a slot crate's own tests cover its slot the same way.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use std::path::PathBuf;
use std::time::{Duration, Instant};

use omuzaki_ai_api::{
    EvalEnv, EvalError, EvalLevel, FetchError, FileSource, PinnedFile, Progress, Session, SlotRef, slots,
};
use omuzaki_ai_mock::FakeSession;

/// How long a smoke test may take.
const SMOKE_BUDGET: Duration = Duration::from_secs(5);

/// No fixtures and no shipped models: a smoke test must say what it needs rather than hang or panic.
struct NoEnv;

impl FileSource for NoEnv {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        Err(FetchError::Network { url: pin.urls.join(" "), reason: "no fixtures in this test".into() })
    }
}

impl EvalEnv for NoEnv {
    fn files(&self) -> &dyn FileSource {
        self
    }

    fn shipped(&self, slot: &SlotRef) -> Result<Box<dyn Session>, EvalError> {
        Err(EvalError::Needs(slot.clone()))
    }
}

#[test]
fn every_slot_smoke_tests_a_mock_session_within_seconds() {
    let mut tested = Vec::new();
    for slot in slots() {
        let info = slot.info();
        let mut session = FakeSession::affine(slot.signature(), 1.0, 0.0);
        let start = Instant::now();
        let result = slot.evaluate(&mut session, &NoEnv, EvalLevel::Smoke);
        let took = start.elapsed();
        assert!(took < SMOKE_BUDGET, "{}'s smoke test took {took:?}", info.slot);
        match result {
            Ok(report) => assert_eq!(report.level, EvalLevel::Smoke, "{}", info.slot),
            // A slot whose test needs fixtures or a shipped model says so; any other error is a broken test.
            Err(EvalError::Needs(_) | EvalError::Fixture(_)) => {}
            Err(e) => panic!("{}'s smoke test failed on a mock session: {e}", info.slot),
        }
        tested.push(info.slot.to_string());
    }
    assert!(tested.len() >= 2, "the mock's slots should be registered, found {tested:?}");
}

#[test]
fn the_mock_slots_pass_their_smoke_test_with_the_identity() {
    for slot in slots().filter(|s| s.info().slot.to_string().starts_with("test_")) {
        let mut session = FakeSession::affine(slot.signature(), 1.0, 0.0);
        let report = slot.evaluate(&mut session, &NoEnv, EvalLevel::Smoke).unwrap();
        assert!(report.passed(), "{}: {report:?}", slot.info().slot);
    }
}
