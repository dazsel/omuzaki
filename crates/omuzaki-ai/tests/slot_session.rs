//! The session `Registry::open` returns follows its slot: a new selection is swapped in without blocking `run` while
//! the old model keeps answering, a model that can no longer be used stops at once, a crash falls back down the chain,
//! and `reset` reaches whatever runs next. A wrapper around the `fake` runtime
//! (`slot-probe`) adds a load delay and records loads and resets; the models are the identity, as the slot's smoke
//! test requires, and differ in their `load_ms` and `crash_after` lines.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
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

/// What the probe runtime saw, one log per model (their digests differ between tests), so a session's `run` only
/// ever contends with the same test's other threads, never with other tests running in parallel.
static LOGS: Mutex<Vec<(ModelRef, Log)>> = Mutex::new(Vec::new());

type Log = Arc<Mutex<Vec<Event>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    /// A load began (before its delay).
    Loading,
    Loaded,
    /// `reset` reached the session, on this thread.
    Reset(ThreadId),
    /// A `run` reached the model.
    Ran,
    /// The session was dropped, on this thread.
    Dropped(ThreadId),
}

fn log_of(model: &ModelRef) -> Log {
    let mut logs = LOGS.lock().unwrap();
    if let Some((_, log)) = logs.iter().find(|(m, _)| m == model) {
        return log.clone();
    }
    let log = Log::default();
    logs.push((model.clone(), log.clone()));
    log
}

/// The threads `reset` ran on, for `model`.
fn resets(model: &ModelRef) -> Vec<ThreadId> {
    let log = log_of(model);
    let log = log.lock().unwrap();
    log.iter().filter_map(|e| if let Event::Reset(t) = e { Some(*t) } else { None }).collect()
}

fn count(model: &ModelRef, event: &Event) -> usize {
    log_of(model).lock().unwrap().iter().filter(|e| *e == event).count()
}

#[derive(Deserialize)]
struct Extras {
    #[serde(default)]
    load_ms: u64,
    /// Loads after this many earlier ones fail (the install's smoke test is the first).
    #[serde(default)]
    fail_load_after: Option<usize>,
    /// Loading panics when a slot session's loader thread does it (the install's smoke test, on the caller's thread,
    /// still works).
    #[serde(default)]
    panic_in_loader: bool,
    /// Resetting a session panics when a slot session's loader thread does it.
    #[serde(default)]
    panic_reset_in_loader: bool,
}

fn in_loader() -> bool {
    thread::current().name().is_some_and(|n| n.starts_with("slot-session"))
}

/// The `fake` runtime, slowed by the model file's `load_ms`, logging loads and resets.
#[derive(Default)]
struct ProbeRuntime;

struct Probed {
    inner: Box<dyn Session>,
    log: Log,
    panic_reset_in_loader: bool,
}

impl Session for Probed {
    fn signature(&self) -> &Signature {
        self.inner.signature()
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.log.lock().unwrap().push(Event::Ran);
        self.inner.run(inputs, outputs)
    }

    fn reset(&mut self) {
        self.log.lock().unwrap().push(Event::Reset(thread::current().id()));
        assert!(!(self.panic_reset_in_loader && in_loader()), "this model's reset was set to panic");
        self.inner.reset();
    }

    fn info(&self) -> SessionInfo {
        self.inner.info()
    }
}

impl Drop for Probed {
    fn drop(&mut self) {
        self.log.lock().unwrap().push(Event::Dropped(thread::current().id()));
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
        log_of(source.model).lock().unwrap().push(Event::Loading);
        assert!(!(extras.panic_in_loader && in_loader()), "this model's load was set to panic");
        std::thread::sleep(Duration::from_millis(extras.load_ms));
        if extras.fail_load_after.is_some_and(|n| count(source.model, &Event::Loaded) >= n) {
            return Err(LoadError::Format("this model's load was set to fail".into()));
        }
        let inner = FakeRuntime.load(source, slot, io, options)?;
        let log = log_of(source.model);
        log.lock().unwrap().push(Event::Loaded);
        Ok(Box::new(Probed { inner, log, panic_reset_in_loader: extras.panic_reset_in_loader }))
    }
}

register_runtime!(ProbeRuntime);

fn echo_slot() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

/// An identity model file; `tag` makes its digest unique, `load_ms` its load slow, `crash_after` its runs fail.
fn model_file(tag: &str, load_ms: u64, crash_after: Option<u64>) -> String {
    let crash = crash_after.map(|n| format!("crash_after = {n}\n")).unwrap_or_default();
    model_file_with(tag, load_ms, &crash)
}

fn model_file_with(tag: &str, load_ms: u64, extra: &str) -> String {
    format!(
        "# {tag}\nscale = 1.0\nbias = 0.0\nload_ms = {load_ms}\n{extra}\n[[inputs]]\nname = \"x\"\ndtype = \"f32\"\n\
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

/// The slot's status. `models` must agree with it about what is active, whenever it is read.
fn status(registry: &Registry) -> omuzaki_ai_api::SlotStatus {
    let status = registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap();
    let models = registry.models(Some(&echo_slot()));
    let marked: Vec<_> = models.iter().filter(|e| e.active).map(|e| e.model.clone()).collect();
    if marked != status.active.clone().into_iter().collect::<Vec<_>>() {
        // Read again: a hand-over may have landed between the two reads.
        let again = registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap();
        let models = registry.models(Some(&echo_slot()));
        let marked: Vec<_> = models.iter().filter(|e| e.active).map(|e| e.model.clone()).collect();
        assert_eq!(marked, again.active.clone().into_iter().collect::<Vec<_>>(), "models and slots disagree");
        return again;
    }
    status
}

/// Polls `check` every few milliseconds until it holds.
fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Keeps running, checking the budget, until a run answers. The answers before that must be `Unavailable`.
fn settle(session: &mut dyn Session, out: &mut [TensorBuf]) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let (result, took) = timed_run(session, out);
        assert!(took < RUN_BUDGET, "a run took {took:?}, longer than a run may");
        match result {
            Ok(()) => return,
            Err(RunError::Unavailable) => {}
            Err(other) => panic!("unexpected {other:?}"),
        }
        assert!(Instant::now() < deadline, "the session never answered again");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn ran(model: &ModelRef) -> usize {
    count(model, &Event::Ran)
}

/// Where `model`'s log is now, to look only at what happens after.
fn mark(model: &ModelRef) -> usize {
    log_of(model).lock().unwrap().len()
}

/// True once `model`'s session was dropped since `mark`, and every such drop was on a thread other than this one.
fn dropped_off_thread_since(model: &ModelRef, mark: usize) -> bool {
    let me = thread::current().id();
    let log = log_of(model);
    let log = log.lock().unwrap();
    let drops: Vec<_> =
        log[mark..].iter().filter_map(|e| if let Event::Dropped(t) = e { Some(*t) } else { None }).collect();
    !drops.is_empty() && drops.iter().all(|t| *t != me)
}

#[test]
fn a_selection_change_keeps_the_old_model_answering_until_the_new_one_is_in() {
    let rig = Rig::new(&[("1.0.0", model_file("swap-1", 0, None)), ("2.0.0", model_file("swap-2", 200, None))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    let loads_before = count(&v2, &Event::Loaded);
    let ran_before = ran(&v1);
    let v2_ran_before = ran(&v2); // the install's smoke test ran it once

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let started = Instant::now();
    let mut checked = false;
    let during = run_until(&mut *session, &mut out, |s| {
        if !checked && count(&v2, &Event::Loading) > loads_before && count(&v2, &Event::Loaded) == loads_before {
            // Mid-load: the old model is what answers, and the health meter says what it waits for.
            let slot = status(&registry);
            assert_eq!(slot.active, Some(v1.clone()));
            assert!(slot.fallback.as_ref().is_some_and(|f| f.contains("is loading")), "{slot:?}");
            assert_eq!(s.info().fallback_for, Some(v2.clone()));
            checked = true;
        }
        count(&v2, &Event::Loaded) > loads_before
    });

    assert!(started.elapsed() >= Duration::from_millis(150), "the 200 ms load happened on another thread");
    assert!(during.iter().all(Result::is_ok), "the old model answers while the new one loads: {during:?}");
    assert!(ran(&v1) > ran_before, "the old model answered");
    assert!(checked, "the load was seen in progress");
    // The load is done: within two runs the new model answers.
    std::thread::sleep(Duration::from_millis(50));
    let v2_before = ran(&v2);
    for _ in 0..2 {
        let (result, took) = timed_run(&mut *session, &mut out);
        assert!(took < RUN_BUDGET, "{took:?}");
        assert_eq!(result, Ok(()));
    }
    assert!(ran(&v2) > v2_before.max(v2_ran_before), "the new model answers");
    assert_eq!(session.info().model, v2);
    assert_eq!(session.info().fallback_for, None);
    let slot = status(&registry);
    assert_eq!((slot.active, slot.fallback), (Some(v2), None));
}
#[test]
fn a_load_that_finishes_after_the_selection_moved_on_never_answers() {
    let files = [
        ("1.0.0", model_file("obsolete-1", 0, None)),
        ("2.0.0", model_file("obsolete-2", 200, None)),
        ("3.0.0", model_file("obsolete-3", 0, None)),
    ];
    let rig = Rig::new(&files);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "3.0.0", "1.0.0"]);
    let (v2, v3) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    let loads = count(&v2, &Event::Loaded);
    let starts = count(&v2, &Event::Loading);
    let ran_v2 = ran(&v2); // the install's smoke test

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()), "the old model answers meanwhile");
    wait_for("v2 to start loading", || count(&v2, &Event::Loading) > starts);
    registry.select(&echo_slot(), Some(&v3), Requester::Person).unwrap();
    wait_for("v2's load to finish", || count(&v2, &Event::Loaded) > loads);
    run_until(&mut *session, &mut out, |s| s.info().model == v3);

    assert_eq!(ran(&v2), ran_v2, "v2 was no longer the choice when it arrived");
    assert_eq!(status(&registry).active, Some(v3));
}

#[test]
fn a_change_that_leaves_the_choice_alone_loads_nothing() {
    let rig = Rig::new(&[("1.0.0", model_file("keep-1", 0, None))]);
    let registry = rig.open();
    let v1 = install(&registry, &["1.0.0"]).remove(0);
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    let loaded = count(&v1, &Event::Loaded);

    registry.select(&echo_slot(), None, Requester::Person).unwrap(); // bumps the generation, same chain
    settle(&mut *session, &mut out);
    for _ in 0..5 {
        let (result, took) = timed_run(&mut *session, &mut out);
        assert_eq!(result, Ok(()));
        assert!(took < RUN_BUDGET);
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(count(&v1, &Event::Loaded), loaded, "the same model isn't loaded again");
}

#[test]
fn a_crash_falls_back_to_the_shipped_default_and_says_why() {
    let rig = Rig::new(&[("1.0.0", model_file("crash-1", 0, None)), ("2.0.0", model_file("crash-2", 0, Some(3)))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let mut session = registry.open(&echo_slot(), None).unwrap();
    wait_for("the session to report v2", || session.info().model == v2);
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    for _ in 0..2 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }
    let at = mark(&v2);

    let (fourth, took) = timed_run(&mut *session, &mut out);
    assert_eq!(fourth, Err(RunError::Unavailable), "the crash is not passed on");
    assert!(took < RUN_BUDGET, "{took:?}");
    settle(&mut *session, &mut out);

    wait_for("the session to report v1", || session.info().model == v1);
    wait_for("the session to report its fallback", || session.info().fallback_for == Some(v2.clone()));
    let slot = status(&registry);
    assert_eq!(slot.active, Some(v1));
    let fallback = slot.fallback.unwrap();
    assert!(fallback.contains("can't run") && fallback.contains("crashed"), "{fallback}");
    let ModelStatus::Disabled { reason } = entry(&registry, "2.0.0").status else { panic!("v2 should be disabled") };
    assert!(reason.contains("stopped working") && reason.contains("crashed"), "{reason}");
    wait_for("the crashed session to be dropped", || dropped_off_thread_since(&v2, at));

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
    settle(&mut *session, &mut out);
    for _ in 0..2 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }

    for _ in 0..20 {
        let (result, took) = timed_run(&mut *session, &mut out);
        assert_eq!(result, Err(RunError::Unavailable));
        assert!(took < RUN_BUDGET, "{took:?}");
        std::thread::sleep(Duration::from_millis(10));
    }

    let slot = status(&registry);
    assert_eq!(slot.active, None);
    assert!(!slot.info.without_model.is_empty(), "the health meter has the slot's own words");
    assert!(slot.fallback.is_some_and(|f| f.contains("crashed")));
    assert!(matches!(entry(&registry, "1.0.0").status, ModelStatus::Disabled { .. }));
}

#[test]
fn removing_the_only_model_retires_the_session_and_answers_unavailable() {
    let rig = Rig::new(&[("1.0.0", model_file("remove-1", 0, None))]);
    let registry = rig.open();
    let v1 = install(&registry, &["1.0.0"]).remove(0);
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    let ran_before = ran(&v1);
    let at = mark(&v1);

    registry.remove(&v1).unwrap();
    for _ in 0..10 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Err(RunError::Unavailable));
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(ran(&v1), ran_before, "the removed model never ran again");
    wait_for("the removed model's session to be dropped", || dropped_off_thread_since(&v1, at));
    assert_eq!(status(&registry).active, None);
}

#[test]
fn a_fallback_that_is_still_loading_is_not_reported_as_active() {
    // Selected v2 crashes; the shipped default v1 takes 300 ms to load.
    let rig = Rig::new(&[("1.0.0", model_file("slots-1", 300, None)), ("2.0.0", model_file("slots-2", 0, Some(3)))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    for _ in 0..2 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }
    assert_eq!(timed_run(&mut *session, &mut out).0, Err(RunError::Unavailable)); // the crash

    // Running on, as a stream does: if the mailbox was taken when v2 failed, its report waits for the next run.
    wait_for("the crash to show", || {
        assert_eq!(timed_run(&mut *session, &mut out).0, Err(RunError::Unavailable), "v1 is still loading");
        status(&registry).fallback.is_some_and(|f| f.contains("crashed"))
    });
    let meanwhile = status(&registry);
    assert_eq!(meanwhile.active, None, "v1 is still loading: {meanwhile:?}");
    assert!(meanwhile.fallback.unwrap().contains("crashed"));
    settle(&mut *session, &mut out);
    wait_for("slots to show v1", || status(&registry).active == Some(v1.clone()));
}

#[test]
fn an_unselected_default_that_crashes_is_recorded_too() {
    let rig = Rig::new(&[("1.0.0", model_file("default-1", 0, None)), ("2.0.0", model_file("default-2", 0, Some(3)))]);
    let registry = rig.open();
    let models = install(&registry, &["1.0.0", "2.0.0"]); // 2.0.0 becomes the default
    let (v1, v2) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    wait_for("the session to report v2", || session.info().model == v2);
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    for _ in 0..2 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }
    assert_eq!(timed_run(&mut *session, &mut out).0, Err(RunError::Unavailable));
    settle(&mut *session, &mut out);

    wait_for("the session to report v1", || session.info().model == v1);
    wait_for("the session to report its fallback", || session.info().fallback_for == Some(v2.clone()));
    let slot = status(&registry);
    assert_eq!(slot.active, Some(v1));
    assert!(slot.fallback.is_some_and(|f| f.contains("crashed")), "no selection, yet the crash is shown");
}

#[test]
fn a_healthy_selection_or_none_clears_what_the_fallback_stood_in_for() {
    let files = [
        ("1.0.0", model_file("meta-1", 0, None)),
        ("2.0.0", model_file("meta-2", 0, Some(3))),
        ("3.0.0", model_file("meta-3", 0, None)),
    ];
    let rig = Rig::new(&files);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "3.0.0", "1.0.0"]);
    let (v2, v3) = (models[0].clone(), models[1].clone());
    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    for _ in 0..2 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }
    assert_eq!(timed_run(&mut *session, &mut out).0, Err(RunError::Unavailable));
    settle(&mut *session, &mut out);
    wait_for("the session to report its fallback", || session.info().fallback_for == Some(v2.clone()));

    // Back to the shipped default, which is what runs: nothing is stood in for any more (the Keep path).
    registry.select(&echo_slot(), None, Requester::Person).unwrap();
    settle(&mut *session, &mut out);
    wait_for("the session to report its fallback", || session.info().fallback_for.is_none());

    // A healthy selection clears it too (the Load path).
    registry.select(&echo_slot(), Some(&v3), Requester::Person).unwrap();
    run_until(&mut *session, &mut out, |s| s.info().model == v3);
    wait_for("the session to report its fallback", || session.info().fallback_for.is_none());
}

#[test]
fn keeping_the_model_because_the_new_selection_wont_load_marks_the_fallback() {
    let rig = Rig::new(&[
        ("1.0.0", model_file("noload-1", 0, None)),
        ("2.0.0", model_file_with("noload-2", 0, "fail_load_after = 1\n")),
    ]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    wait_for("the session to report its fallback", || session.info().fallback_for.is_none());

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let during =
        run_until(&mut *session, &mut out, |_| status(&registry).fallback.is_some_and(|f| f.contains("set to fail")));

    assert!(during.iter().all(Result::is_ok), "v1 kept answering: {during:?}");
    assert_eq!(session.info().model, v1);
    assert_eq!(session.info().fallback_for, Some(v2.clone()));
    let slot = status(&registry);
    assert_eq!(slot.active, Some(v1));
    let fallback = slot.fallback.expect("the selection isn't what runs");
    assert!(fallback.contains("set to fail"), "the load failure is the reason: {fallback}");
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
    assert_eq!(resets(&v1), vec![thread::current().id()], "forwarded to the session that runs");

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()), "v1 answers while v2 loads");
    session.reset(); // while v2 loads
    assert_eq!(resets(&v1).len(), 2, "the session that answers is reset at once, here");
    run_until(&mut *session, &mut out, |s| s.info().model == v2);
    assert_eq!(resets(&v2).len(), 1, "replayed on the session that was swapped in");
    assert!(resets(&v2).iter().all(|t| *t != thread::current().id()), "by the loader, not on the caller's thread");
}

#[test]
fn a_reset_right_after_a_selection_change_is_replayed_too() {
    let rig = Rig::new(&[("1.0.0", model_file("early-1", 0, None)), ("2.0.0", model_file("early-2", 100, None))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let v2 = models[0].clone();
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    session.reset(); // before any run has noticed the change
    run_until(&mut *session, &mut out, |s| s.info().model == v2);
    assert_eq!(resets(&v2).len(), 1);
}

#[test]
fn a_crash_disable_is_not_saved_by_a_later_commit_after_a_failed_one() {
    let rig = Rig::new(&[("1.0.0", model_file("save-1", 0, None)), ("2.0.0", model_file("save-2", 0, Some(3)))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let v2 = models[0].clone();
    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    for _ in 0..2 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }
    assert_eq!(timed_run(&mut *session, &mut out).0, Err(RunError::Unavailable));
    settle(&mut *session, &mut out);

    // A reinstall whose state can't be saved: the state file's place is taken by a folder.
    let file = rig.dir.path().join("models").join("state.toml");
    let kept = std::fs::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir_all(file.join("in the way")).unwrap();
    assert!(registry.install(&v2, &NoProgress).is_err(), "the commit can't be written");
    std::fs::remove_dir_all(&file).unwrap();
    std::fs::write(&file, kept).unwrap();
    registry.select(&echo_slot(), None, Requester::Person).unwrap(); // the next commit writes the file

    drop(session);
    assert_eq!(entry(&rig.open(), "2.0.0").status, ModelStatus::Ready, "the crash was for that app session only");
}

#[test]
fn a_load_that_panics_leaves_the_old_model_answering_and_says_why() {
    let rig = Rig::new(&[
        ("1.0.0", model_file("panic-1", 0, None)),
        ("2.0.0", model_file_with("panic-2", 0, "panic_in_loader = true\n")),
    ]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    let ran_before = ran(&v1);

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let during = run_until(&mut *session, &mut out, |_| {
        status(&registry).fallback.is_some_and(|f| f.contains("runtime failed while loading"))
    });
    for _ in 0..10 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }

    assert!(during.iter().all(Result::is_ok), "the old model kept answering: {during:?}");
    assert!(ran(&v1) > ran_before);
    assert_eq!(status(&registry).active, Some(v1.clone()));
    assert_eq!(session.info().model, v1);
    assert_eq!(session.info().fallback_for, Some(v2), "v1 stands in for the selection that won't load");
}
#[test]
fn a_replacement_whose_reset_panics_is_a_load_failure_and_the_chain_goes_on() {
    let rig = Rig::new(&[
        ("1.0.0", model_file("reset-1", 0, None)),
        ("2.0.0", model_file_with("reset-2", 0, "panic_reset_in_loader = true\n")),
    ]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    let ran_before = (ran(&v1), ran(&v2));
    let dropped_mark = mark(&v2);

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let during = run_until(&mut *session, &mut out, |_| {
        status(&registry).fallback.is_some_and(|f| f.contains("failed while resetting"))
    });
    // The loader settled on v1 again: it keeps answering, and the replacement never does.
    for _ in 0..10 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    }

    assert!(during.iter().all(Result::is_ok), "the old model kept answering: {during:?}");
    assert!(ran(&v1) > ran_before.0);
    assert_eq!(ran(&v2), ran_before.1, "the replacement that failed its reset never answered");
    wait_for("the failed replacement to be dropped off the caller's thread", || {
        dropped_off_thread_since(&v2, dropped_mark)
    });
    let health = status(&registry);
    assert_eq!(health.active, Some(v1.clone()));
    assert_eq!(session.info().model, v1);
    assert_eq!(session.info().fallback_for, Some(v2), "v1 stands in for the selection whose reset panicked");
}

#[test]
fn a_result_that_finishes_during_a_slower_one_is_never_reported_as_running() {
    let files = [
        ("1.0.0", model_file("stale-1", 0, None)),
        ("2.0.0", model_file("stale-2", 100, None)),
        ("3.0.0", model_file("stale-3", 600, None)),
    ];
    let rig = Rig::new(&files);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "3.0.0", "1.0.0"]);
    let (v2, v3, v1) = (models[0].clone(), models[1].clone(), models[2].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    let started = count(&v2, &Event::Loading);
    let loaded = count(&v2, &Event::Loaded);
    let v3_started = count(&v3, &Event::Loading);

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
    wait_for("v2 to start loading", || count(&v2, &Event::Loading) > started);
    registry.select(&echo_slot(), Some(&v3), Requester::Person).unwrap();
    wait_for("v2 to finish loading", || count(&v2, &Event::Loaded) > loaded);
    wait_for("v3 to start loading", || count(&v3, &Event::Loading) > v3_started);

    // v3 still has most of its 600 ms to go. v1 answers, and nothing says v2 does.
    for _ in 0..10 {
        assert_eq!(timed_run(&mut *session, &mut out).0, Ok(()));
        assert_eq!(session.info().model, v1);
        assert_eq!(status(&registry).active, Some(v1.clone()));
        std::thread::sleep(Duration::from_millis(5));
    }
    run_until(&mut *session, &mut out, |s| s.info().model == v3);
    assert_eq!(status(&registry).active, Some(v3));
}

#[test]
fn removing_the_running_model_stops_it_at_once_while_its_replacement_loads() {
    let rig = Rig::new(&[("1.0.0", model_file("gone-1", 300, None)), ("2.0.0", model_file("gone-2", 0, None))]);
    let registry = rig.open();
    let models = install(&registry, &["1.0.0", "2.0.0"]); // 2.0.0 is the default and runs
    let (v1, v2) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);
    assert_eq!(session.info().model, v2);
    let ran_before = ran(&v2);

    registry.remove(&v2).unwrap();
    let (first, took) = timed_run(&mut *session, &mut out);
    assert_eq!(first, Err(RunError::Unavailable), "the very next run after the removal");
    assert!(took < RUN_BUDGET, "{took:?}");
    let slot = status(&registry);
    assert_eq!(slot.active, None, "nothing answers while v1 loads: {slot:?}");
    settle(&mut *session, &mut out);

    assert_eq!(ran(&v2), ran_before, "the removed model never answered again");
    assert_eq!(session.info().model, v1);
    assert_eq!(status(&registry).active, Some(v1));
}

#[test]
fn a_reset_after_the_replacement_was_published_resets_it_again_before_it_answers() {
    let rig = Rig::new(&[("1.0.0", model_file("count-1", 0, None)), ("2.0.0", model_file("count-2", 0, None))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let (v2, v1) = (models[0].clone(), models[1].clone());
    let mut session = registry.open(&echo_slot(), None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    settle(&mut *session, &mut out);

    // No run takes it in: the loader loads v2, resets it to count 0 and publishes it.
    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    wait_for("the loader to reset v2", || resets(&v2).len() == 1);
    std::thread::sleep(Duration::from_millis(50));
    session.reset(); // count 1: what waits is stale

    run_until(&mut *session, &mut out, |s| s.info().model == v2);
    assert_eq!(resets(&v1), vec![thread::current().id()], "the session that answered was reset here");
    let v2_resets = resets(&v2);
    assert_eq!(v2_resets.len(), 2, "reset again before it was applied");
    assert!(v2_resets.iter().all(|t| *t != thread::current().id()), "both by the loader");
}

#[test]
fn every_session_of_a_registry_shares_one_loader_thread() {
    let rig = Rig::new(&[("1.0.0", model_file("shared-1", 0, None)), ("2.0.0", model_file("shared-2", 0, None))]);
    let registry = rig.open();
    let models = install(&registry, &["2.0.0", "1.0.0"]);
    let v2 = models[0].clone();
    let mut sessions: Vec<_> = (0..8).map(|_| registry.open(&echo_slot(), None).unwrap()).collect();
    let mut out = TensorBuf::for_outputs(sessions[0].signature());

    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    for session in &mut sessions {
        run_until(&mut **session, &mut out, |s| s.info().model == v2);
    }

    let threads = resets(&v2); // the loader resets each replacement it loads
    assert_eq!(threads.len(), sessions.len());
    assert!(threads.iter().all(|t| *t == threads[0]), "one loader for all of them: {threads:?}");
    assert_ne!(threads[0], thread::current().id());
}
