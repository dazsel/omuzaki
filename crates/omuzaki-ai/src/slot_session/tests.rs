//! Unit tests for what the integration tests can't reach: a registry whose loader thread couldn't be started.

use std::time::{Duration, Instant};

use omuzaki_ai_api::{ModelRegistry, NoProgress, RunError, SlotRef, TensorBuf, TensorRef};
use omuzaki_ai_mock::fixtures_fetcher;

use super::loader::FAIL_SPAWN;
use crate::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};

fn echo_slot() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

#[test]
fn without_a_loader_sessions_keep_their_answer_until_their_model_goes_and_reset_starts_it() {
    let dir = tempfile::tempdir().unwrap();
    let files = std::sync::Arc::new(VerifiedCache::new(dir.path().join(CACHE_DIR), fixtures_fetcher()));
    let registry = Registry::new(dir.path(), files, "1.0.0".parse().unwrap(), RegistrySettings::default()).unwrap();
    let echo = registry.models(Some(&echo_slot())).remove(0).model;
    registry.install(&echo, &NoProgress).unwrap();

    FAIL_SPAWN.with(|fail| fail.set(true));
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    let frame = [0.5f32; 4];
    let mut run = |session: &mut dyn omuzaki_ai_api::Session| session.run(&[TensorRef::f32(&frame, &[4])], &mut out);
    assert_eq!(run(&mut *session), Ok(()), "the model answers without a loader");

    // Its own model goes: it stops at once, loader or not, and the health meter says why nothing replaces it.
    registry.remove(&echo).unwrap();
    assert_eq!(run(&mut *session), Err(RunError::Unavailable));
    let status = registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap();
    assert_eq!(status.active, None);
    registry.install(&echo, &NoProgress).unwrap();
    let status = registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap();
    let fallback = status.fallback.unwrap();
    assert!(fallback.contains("couldn't start the thread"), "{fallback}");
    for _ in 0..20 {
        assert_eq!(run(&mut *session), Err(RunError::Unavailable), "nothing has confirmed the model again");
        std::thread::sleep(Duration::from_millis(1));
    }
    FAIL_SPAWN.with(|fail| fail.set(false));

    session.reset(); // cold: starts the loader, which confirms the reinstalled model
    let deadline = Instant::now() + Duration::from_secs(20);
    while run(&mut *session).is_err() {
        assert!(Instant::now() < deadline, "the started loader never confirmed");
        std::thread::sleep(Duration::from_millis(2));
    }
    let status = registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap();
    assert_eq!((status.active, status.fallback), (Some(echo), None));
}
