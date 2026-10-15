//! Unit tests for what the integration tests can't reach: a session whose loader thread couldn't be started.

use std::time::{Duration, Instant};

use omuzaki_ai_api::{ModelRegistry, NoProgress, Requester, RunError, SlotRef, TensorBuf, TensorRef};
use omuzaki_ai_mock::fixtures_fetcher;

use super::loader::FAIL_SPAWN;
use crate::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};

fn echo_slot() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

#[test]
fn without_a_loader_the_gate_stays_shut_for_a_changed_selection_until_reset_restarts_it() {
    let dir = tempfile::tempdir().unwrap();
    let files = std::sync::Arc::new(VerifiedCache::new(dir.path().join(CACHE_DIR), fixtures_fetcher()));
    let registry = Registry::new(dir.path(), files, "1.0.0".parse().unwrap(), RegistrySettings::default()).unwrap();
    let echo = registry.models(Some(&echo_slot())).remove(0).model;
    registry.install(&echo, &NoProgress).unwrap();

    FAIL_SPAWN.with(|fail| fail.set(true));
    let mut session = registry.open(&echo_slot(), None).unwrap();
    FAIL_SPAWN.with(|fail| fail.set(false));
    let mut out = TensorBuf::for_outputs(session.signature());
    let frame = [0.5f32; 4];
    let mut run = |session: &mut dyn omuzaki_ai_api::Session| session.run(&[TensorRef::f32(&frame, &[4])], &mut out);
    assert_eq!(run(&mut *session), Ok(()), "nothing has changed, so the model answers");

    registry.select(&echo_slot(), Some(&echo), Requester::Person).unwrap();
    for _ in 0..50 {
        assert_eq!(run(&mut *session), Err(RunError::Unavailable), "nobody can say the old model is still right");
        std::thread::sleep(Duration::from_millis(1));
    }

    session.reset(); // starts the loader, which settles the change
    let deadline = Instant::now() + Duration::from_secs(20);
    while run(&mut *session).is_err() {
        assert!(Instant::now() < deadline, "the restarted loader never confirmed");
        std::thread::sleep(Duration::from_millis(2));
    }
}
