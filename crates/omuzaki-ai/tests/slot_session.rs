//! The session `Registry::open` returns follows its slot: a new selection is swapped in without blocking `run`, a
//! crash falls back down the chain, and `reset` reaches whatever runs next. A wrapper around the `fake` runtime
//! (`slot-probe`) adds a load delay and records loads and resets; the models are the identity, as the slot's smoke
//! test requires, and differ in their `load_ms` and `crash_after` lines.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{
    LoadError, ModelEntry, ModelRef, ModelRegistry, ModelSource, ModelStatus, NoProgress, Probe, Requester, RunError,
    Runtime, RuntimeId, Session, SessionInfo, SessionOptions, Signature, SlotRef, TensorBuf, TensorRef, Version,
    register_runtime,
};
use omuzaki_ai_mock::{FakeRuntime, MemFetcher};
use serde::Deserialize;

const MODELS: &str = "echo-fake";
/// What one `run` may take, however long a load does.
const RUN_BUDGET: Duration = Duration::from_millis(5);
/// How long a test waits for the loader thread before it calls the swap lost.
const PATIENCE: Duration = Duration::from_secs(20);

/// Everything the probe runtime saw, for all tests in this binary; each test looks up only its own models (their
/// digests differ).
static LOG: Mutex<Vec<Event>> = Mutex::new(Vec::new());

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Loaded(ModelRef),
    Reset(ModelRef),
}

fn count(event: &Event) -> usize {
    LOG.lock().unwrap().iter().filter(|e| *e == event).count()
}

#[derive(Deserialize)]
struct Extras {
    #[serde(default)]
    load_ms: u64,
}

/// The `fake` runtime, slowed by the model file's `load_ms`, logging loads and resets.
#[derive(Default)]
struct ProbeRuntime;

struct Probed {
    inner: Box<dyn Session>,
    model: ModelRef,
}

impl Session for Probed {
    fn signature(&self) -> &Signature {
        self.inner.signature()
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.inner.run(inputs, outputs)
    }

    fn reset(&mut self) {
        LOG.lock().unwrap().push(Event::Reset(self.model.clone()));
        self.inner.reset();
    }

    fn info(&self) -> SessionInfo {
        self.inner.info()
    }
}

impl Runtime for ProbeRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::new("slot-probe").unwrap()
    }

    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        FakeRuntime.probe(source)
    }

    fn load(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        io: &BTreeMap<String, String>,
        options: &SessionOptions,
    ) -> Result<Box<dyn Session>, LoadError> {
        let text = std::fs::read_to_string(&source.files[0].path).map_err(|e| LoadError::Format(e.to_string()))?;
        let extras: Extras = toml::from_str(&text).map_err(|e| LoadError::Format(e.to_string()))?;
        std::thread::sleep(Duration::from_millis(extras.load_ms));
        let inner = FakeRuntime.load(source, slot, io, options)?;
        LOG.lock().unwrap().push(Event::Loaded(source.model.clone()));
        Ok(Box::new(Probed { inner, model: source.model.clone() }))
    }
}

register_runtime!(ProbeRuntime);

fn echo_slot() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

/// An identity model file; `tag` makes its digest unique, `load_ms` its load slow, `crash_after` its runs fail.
fn model_file(tag: &str, load_ms: u64, crash_after: Option<u64>) -> String {
    let crash = crash_after.map(|n| format!("crash_after = {n}\n")).unwrap_or_default();
    format!(
        "# {tag}\nscale = 1.0\nbias = 0.0\nload_ms = {load_ms}\n{crash}\n[[inputs]]\nname = \"x\"\ndtype = \"f32\"\n\
         shape = [\"any\"]\n\n[[outputs]]\nname = \"y\"\ndtype = \"f32\"\nshape = [\"any\"]\n"
    )
}

fn url(version: &str) -> String {
    format!("https://models.invalid/{MODELS}/{version}/echo.fake.toml")
}

fn manifest(version: &str, bytes: &str) -> (String, String) {
    let pin = common::pin(&[&url(version)], bytes.as_bytes());
    let text = format!(
        "id = \"{MODELS}\"\nversion = \"{version}\"\nname = \"Echo (probe)\"\nslot = \"test_echo@1\"\n\
         runtime = \"slot-probe\"\nmin_app_version = \"1.0.0\"\nproviders = [\"cpu\"]\n\n[[files]]\n\
         name = \"echo.fake.toml\"\nsha256 = \"{}\"\nsize = {}\nurls = [\"{}\"]\n\n[card]\n\
         register = \"omuzaki-ai tests\"\n\n[card.licence]\nspdx = \"MIT OR Apache-2.0\"\n",
        pin.sha256,
        pin.size,
        url(version)
    );
    (MODELS.to_owned(), text)
}

/// A data folder with the models of one test, opened as often as the test wants.
struct Rig {
    dir: tempfile::TempDir,
    net: Arc<MemFetcher>,
    models: Vec<(String, String)>,
}

impl Rig {
    /// `files` are `(version, model file)`; the file contents make the digests, so tests tag them apart.
    fn new(files: &[(&str, String)]) -> Self {
        let mut net = MemFetcher::new();
        let mut models = Vec::new();
        for (version, bytes) in files {
            net = net.serving(url(version), bytes.as_str());
            models.push(manifest(version, bytes));
        }
        Self { dir: tempfile::tempdir().unwrap(), net: Arc::new(net), models }
    }

    fn open(&self) -> Registry {
        struct Shared(Arc<MemFetcher>);
        impl omuzaki_ai_api::Fetcher for Shared {
            fn open(&self, url: &str, from: u64) -> Result<omuzaki_ai_api::Body, omuzaki_ai_api::FetchError> {
                self.0.open(url, from)
            }
        }
        let files = Arc::new(VerifiedCache::new(self.dir.path().join(CACHE_DIR), Shared(self.net.clone())));
        let app: Version = "1.0.0".parse().unwrap();
        Registry::with_models(self.dir.path(), files, app, RegistrySettings::default(), self.models.clone()).unwrap()
    }
}

fn entry(registry: &Registry, version: &str) -> ModelEntry {
    let version: Version = version.parse().unwrap();
    registry.models(Some(&echo_slot())).into_iter().find(|e| e.model.version == version).unwrap()
}

/// Installs `versions` in order: the last one becomes the slot's shipped default.
fn install(registry: &Registry, versions: &[&str]) -> Vec<ModelRef> {
    versions
        .iter()
        .map(|v| {
            let model = entry(registry, v).model;
            registry.install(&model, &NoProgress).unwrap();
            model
        })
        .collect()
}

/// One `run` of the identity, timed.
fn timed_run(session: &mut dyn Session, out: &mut [TensorBuf]) -> (Result<(), RunError>, Duration) {
    let frame = [0.5f32; 4];
    let start = Instant::now();
    let result = session.run(&[TensorRef::f32(&frame, &[4])], out);
    (result, start.elapsed())
}

/// Runs until `done`, asserting that no run takes longer than [`RUN_BUDGET`]. Returns the results seen.
fn run_until(
    session: &mut dyn Session,
    out: &mut [TensorBuf],
    mut done: impl FnMut(&dyn Session) -> bool,
) -> Vec<Result<(), RunError>> {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    while !done(session) {
        assert!(Instant::now() < deadline, "the loader never finished; saw {seen:?}");
        let (result, took) = timed_run(session, out);
        assert!(took < RUN_BUDGET, "a run took {took:?}, longer than a run may");
        seen.push(result);
        std::thread::sleep(Duration::from_millis(2));
    }
    seen
}

#[test]
fn a_new_selection_is_swapped_in_without_blocking_any_run() {
    let rig = Rig::new(&[("1.0.0", model_file("swap-1", 0, None)), ("2.0.0", model_file("swap-2", 200, None))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    assert_eq!(session.info().model, v1);
    let mut out = TensorBuf::for_outputs(session.signature());
    let loads_before = count(&Event::Loaded(v2.clone()));

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let started = Instant::now();
    let during = run_until(&mut *session, &mut out, |s| {
        s.info().model == v2 || count(&Event::Loaded(v2.clone())) > loads_before
    });

    assert!(started.elapsed() >= Duration::from_millis(150), "the 200 ms load happened on another thread");
    assert!(!during.is_empty() && during.iter().all(Result::is_ok), "the old session answered meanwhile: {during:?}");
    // The load is done: the next two runs pick it up (after a pause, so the hand-over has certainly landed).
    std::thread::sleep(Duration::from_millis(50));
    for _ in 0..2 {
        let (result, took) = timed_run(&mut *session, &mut out);
        assert!(took < RUN_BUDGET, "{took:?}");
        assert!(result.is_ok() || result == Err(RunError::Unavailable));
    }
    assert_eq!(session.info().model, v2);
    assert_eq!(session.info().fallback_for, None, "the selection is what runs, nothing is stood in for");
}

#[test]
fn a_change_that_leaves_the_choice_alone_loads_nothing() {
    let rig = Rig::new(&[("1.0.0", model_file("keep-1", 0, None))]);
    let registry = rig.open();
    let v1 = install(&registry, &["1.0.0"]).remove(0);
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    let loaded = count(&Event::Loaded(v1.clone()));

    registry.select(&echo_slot(), None, Requester::Person).unwrap(); // bumps the generation, same chain
    for _ in 0..5 {
        let (result, took) = timed_run(&mut *session, &mut out);
        assert_eq!(result, Ok(()));
        assert!(took < RUN_BUDGET);
        std::thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(count(&Event::Loaded(v1)), loaded, "the same model isn't loaded again");
}

#[test]
fn a_crash_falls_back_to_the_shipped_default_and_says_why() {
    let rig = Rig::new(&[("1.0.0", model_file("crash-1", 0, None)), ("2.0.0", model_file("crash-2", 0, Some(3)))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let mut session = registry.open(&echo_slot(), None).unwrap();
    assert_eq!(session.info().model, v2);
    let mut out = TensorBuf::for_outputs(session.signature());

    for _ in 0..3 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }
    let (fourth, took) = timed_run(&mut *session, &mut out);
    assert_eq!(fourth, Err(RunError::Unavailable), "the crash is not passed on");
    assert!(took < RUN_BUDGET, "{took:?}");
    run_until(&mut *session, &mut out, |s| s.info().model == v1);

    let info = session.info();
    assert_eq!(info.fallback_for, Some(v2.clone()));
    assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()), "the default answers");
    let slot = registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap();
    assert_eq!(slot.active, Some(v1));
    assert!(slot.fallback.as_ref().is_some_and(|f| f.contains("can't run")), "{:?}", slot.fallback);
    let ModelStatus::Disabled { reason } = entry(&registry, "2.0.0").status else { panic!("v2 should be disabled") };
    assert!(reason.contains("stopped working") && reason.contains("crashed"), "{reason}");

    // Only for this app session: the state file doesn't remember it.
    drop(session);
    let reopened = rig.open();
    assert_eq!(entry(&reopened, "2.0.0").status, ModelStatus::Ready);
}

#[test]
fn with_nothing_to_fall_back_to_a_crash_leaves_the_slot_unavailable() {
    let rig = Rig::new(&[("1.0.0", model_file("alone-1", 0, Some(3)))]);
    let registry = rig.open();
    install(&registry, &["1.0.0"]);
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    for _ in 0..3 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }

    for _ in 0..20 {
        let (result, took) = timed_run(&mut *session, &mut out);
        assert_eq!(result, Err(RunError::Unavailable));
        assert!(took < RUN_BUDGET, "{took:?}");
        std::thread::sleep(Duration::from_millis(10));
    }

    let slot = registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap();
    assert_eq!(slot.active, None);
    assert!(!slot.info.without_model.is_empty(), "the health meter has the slot's own words");
    assert!(matches!(entry(&registry, "1.0.0").status, ModelStatus::Disabled { .. }));
}

#[test]
fn reset_reaches_the_current_session_and_the_one_swapped_in() {
    let rig = Rig::new(&[("1.0.0", model_file("reset-1", 0, None)), ("2.0.0", model_file("reset-2", 200, None))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());

    session.reset();
    assert_eq!(count(&Event::Reset(v1.clone())), 1, "forwarded to the session that runs");

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    assert_eq!(timed_run(&mut *session, &mut out).0, Ok(())); // starts the load
    session.reset(); // while v2 loads
    run_until(&mut *session, &mut out, |s| s.info().model == v2);

    assert_eq!(count(&Event::Reset(v2)), 1, "replayed on the session that was swapped in");
}
