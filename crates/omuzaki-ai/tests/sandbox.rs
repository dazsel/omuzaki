//! The sandboxed worker (#1153), with this test binary standing in for `omuzaki-model-worker` (`common::rig`): a fake
//! model runs there as it does in process, a crash or a stall kills the worker and is `Crashed`, the memory cap holds,
//! and through the registry a crashing user model fails its slot, never the caller. Offline.
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::rig::{PORTS_X_Y, Rig, add, echo, manifest, reason, worker_settings};
use omuzaki_ai::sandbox::{self, DEFAULT_MEMORY_MIB, LoadSpec, Model, SandboxedSession};
use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{
    LoadError, ModelRef, ModelRegistry, ModelStatus, NoProgress, Requester, RunError, RuntimeId, Session,
    SessionOptions, TensorBuf, TensorRef,
};
use omuzaki_ai_mock::{FakeSession, MemFetcher, echo_signature, fake_model, fixtures_fetcher};

/// Writes a `fake` model with `extra` keys and the echo ports, and returns its path.
fn fake_file(dir: &tempfile::TempDir, scale: f32, extra: &str) -> PathBuf {
    let path = dir.path().join("m.fake.toml");
    std::fs::write(&path, format!("scale = {scale:?}\nbias = 0.5\n{extra}\n{PORTS_X_Y}")).unwrap();
    path
}

/// Loads the fake model at `path` straight into a worker capped at `memory_mib`, with `stall` per run.
fn load(path: PathBuf, memory_mib: u64, stall: Duration) -> Result<SandboxedSession, LoadError> {
    let (model, runtime) = (fake_model(), RuntimeId::new("fake").unwrap());
    let files = vec![("m.fake.toml".to_owned(), path)];
    let slot = echo_signature();
    let spec = LoadSpec { slot: &slot, io: &BTreeMap::new(), options: SessionOptions::default(), stall };
    let natives = VerifiedCache::new(tempfile::tempdir().unwrap().keep().join(CACHE_DIR), MemFetcher::new());
    sandbox::load(
        &worker_settings(memory_mib),
        &Model { model: &model, runtime: &runtime, files, stages: &[] },
        spec,
        &natives,
    )
}

fn run(session: &mut dyn Session, x: &[f32]) -> Result<Vec<f32>, RunError> {
    let mut out = TensorBuf::for_outputs(session.signature());
    session.run(&[TensorRef::f32(x, &[x.len()])], &mut out)?;
    Ok(out[0].as_f32().unwrap().to_vec())
}

const LONG: Duration = Duration::from_secs(30);

#[test]
fn a_fake_model_answers_in_the_worker_exactly_as_in_process() {
    let dir = tempfile::tempdir().unwrap();
    let mut sandboxed = load(fake_file(&dir, 2.0, ""), DEFAULT_MEMORY_MIB, LONG).unwrap();
    let mut local = FakeSession::affine(echo_signature(), 2.0, 0.5);
    assert!(sandboxed.info().sandboxed);
    for x in [
        vec![0.0f32, 1.5, -3.25],
        vec![f32::MAX, f32::MIN_POSITIVE],
        (0..1000u16).map(|i| f32::from(i) * 0.1).collect(),
    ] {
        assert_eq!(run(&mut sandboxed, &x).unwrap(), run(&mut local, &x).unwrap());
    }
    sandboxed.reset();
    assert_eq!(run(&mut sandboxed, &[1.0]).unwrap(), [2.5], "a reset leaves it working");
    // The registry's own checks come first, as in every session: a wrong input never reaches the worker.
    let mut out = TensorBuf::for_outputs(sandboxed.signature());
    assert!(matches!(sandboxed.run(&[], &mut out), Err(RunError::Mismatch { .. })));
    assert!(sandboxed.worker_running());
}

#[test]
fn a_crash_is_crashed_and_the_worker_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = load(fake_file(&dir, 1.0, "crash_after = 1"), DEFAULT_MEMORY_MIB, LONG).unwrap();
    assert!(run(&mut session, &[1.0]).is_ok());
    assert_eq!(run(&mut session, &[1.0]), Err(RunError::Crashed));
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.worker_running() {
        assert!(Instant::now() < deadline, "the worker outlived its crash");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(run(&mut session, &[1.0]), Err(RunError::Crashed), "a dead worker stays dead");
}

#[test]
fn a_run_past_the_deadline_is_crashed_within_50_ms_and_the_worker_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let stall = Duration::from_millis(100);
    let mut session = load(fake_file(&dir, 1.0, "stall_ms = 5000"), DEFAULT_MEMORY_MIB, stall).unwrap();
    let start = Instant::now();
    assert_eq!(run(&mut session, &[1.0]), Err(RunError::Crashed));
    let took = start.elapsed();
    assert!(took < stall + Duration::from_millis(50), "the stall took {took:?} to be called");
    assert!(!session.worker_running());
}

#[test]
#[cfg(target_os = "linux")]
fn a_model_over_the_memory_cap_is_killed_and_one_under_it_loads() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load(fake_file(&dir, 1.0, "grab_mb = 64"), 1024, LONG).is_ok(), "64 MiB fits under 1 GiB");
    let Err(error) = load(fake_file(&dir, 1.0, "grab_mb = 4096"), 1024, LONG) else { panic!("4 GiB loaded") };
    assert!(error.to_string().contains("crashed loading"), "{error}");
}

#[test]
fn a_worker_that_cannot_start_is_a_load_error_that_says_so() {
    let mut settings = worker_settings(DEFAULT_MEMORY_MIB);
    settings.program = "/nonexistent/omuzaki-model-worker".into();
    let (model, runtime) = (fake_model(), RuntimeId::new("fake").unwrap());
    let probe = sandbox::probe(&settings, &Model { model: &model, runtime: &runtime, files: vec![], stages: &[] }, &{
        VerifiedCache::new(tempfile::tempdir().unwrap().keep(), MemFetcher::new())
    });
    let Err(error) = probe else { panic!("a probe without a worker") };
    assert!(error.to_string().contains("couldn't be started"), "{error}");
}

#[test]
fn adding_a_model_the_worker_refuses_or_that_crashes_in_its_smoke_test_disables_it() {
    let rig = Rig::new();
    let registry = rig.registry();
    for (extra, expected) in [
        ("refuse = \"custom op domain com.example\"\n", "refused: custom op domain com.example"),
        ("crash_after = 0\n", "its smoke test could not run"),
    ] {
        let file = rig.fake("m.fake.toml", "1.0", &format!("{extra}{PORTS_X_Y}"));
        let entry = add(&registry, &echo(), vec![file.clone()], Some(manifest(&echo(), &[&file], "")), Some("Pat"));
        assert!(reason(&entry).contains(expected), "{expected:?}: {entry:?}");
    }
}

#[test]
fn a_user_model_that_crashes_while_running_falls_back_to_the_shipped_model_and_says_why() {
    let rig = Rig::new();
    let files = Arc::new(VerifiedCache::new(rig.data.path().join(CACHE_DIR), fixtures_fetcher()));
    let registry =
        Registry::new(rig.data.path(), files, "1.0.0".parse().unwrap(), RegistrySettings::default()).unwrap();
    registry.set_sandbox(worker_settings(DEFAULT_MEMORY_MIB));
    let shipped = registry.models(Some(&echo())).remove(0).model;
    registry.install(&shipped, &NoProgress).unwrap();
    // Every load is a fresh worker: the smoke test's run passes, and so does the session's first.
    let file = rig.fake("mine.fake.toml", "1.0", &format!("crash_after = 1\n{PORTS_X_Y}"));
    let mine = add(&registry, &echo(), vec![file.clone()], Some(manifest(&echo(), &[&file], "")), Some("Pat")).model;
    registry.select(&echo(), Some(&mine), Requester::Person).unwrap();

    let mut session = registry.open(&echo(), None).unwrap();
    assert_eq!(run(&mut *session, &[0.5]), Ok(vec![0.5]));
    assert_eq!(run(&mut *session, &[0.5]), Err(RunError::Unavailable), "the slot session absorbs the crash");
    let deadline = Instant::now() + LONG;
    while run(&mut *session, &[0.5]).is_err() {
        assert!(Instant::now() < deadline, "the shipped model never took over");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(session.info().model, shipped);
    assert_eq!(session.info().fallback_for, Some(mine.clone()));
    let status = registry.slots().into_iter().find(|s| s.info.slot == echo()).unwrap();
    assert!(status.fallback.unwrap().contains("stopped working"));
    let entry = registry.models(None).into_iter().find(|e| e.model == mine).unwrap();
    assert!(matches!(entry.status, ModelStatus::Disabled { .. }), "{entry:?}");
}

/// The pipe's cost per run, for the PR: run with `--release --nocapture`.
#[test]
fn per_run_overhead_of_the_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = load(fake_file(&dir, 1.0, ""), DEFAULT_MEMORY_MIB, LONG).unwrap();
    let mut local = FakeSession::affine(echo_signature(), 1.0, 0.5);
    for (what, len) in [("640x480 RGB frame (f32)", 640 * 480 * 3), ("20 ms audio block at 48 kHz (f32)", 960)] {
        let x = vec![0.25f32; len];
        let mut out = TensorBuf::for_outputs(session.signature());
        let times = |s: &mut dyn Session, out: &mut [TensorBuf]| -> Vec<Duration> {
            // Enough to rank in release, where the numbers mean something; a smoke run in debug.
            (0..if cfg!(debug_assertions) { 10 } else { 200 })
                .map(|_| {
                    let start = Instant::now();
                    s.run(&[TensorRef::f32(&x, &[len])], out).unwrap();
                    start.elapsed()
                })
                .collect()
        };
        let (mut piped, mut direct) = (times(&mut session, &mut out), times(&mut local, &mut out));
        piped.sort();
        direct.sort();
        let at = |v: &[Duration], q: usize| v[v.len() * q / 100];
        println!(
            "{what}: sandboxed p50 {:?} p95 {:?}; in process p50 {:?} p95 {:?}",
            at(&piped, 50),
            at(&piped, 95),
            at(&direct, 50),
            at(&direct, 95)
        );
    }
}

#[test]
fn the_model_ref_of_a_sandboxed_session_is_the_one_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    let session = load(fake_file(&dir, 1.0, ""), DEFAULT_MEMORY_MIB, LONG).unwrap();
    let expected: ModelRef = fake_model();
    assert_eq!(session.info().model, expected);
}
