//! The `model.*` ops over a real registry (the mock's runtime, slots and fixtures; this test binary as the sandboxed
//! worker): what an agent and a person may each do, did-you-mean on names, `model.add` through the person's file roots,
//! and `model.status` while a download runs and after it fails. Offline.
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::rig::{PORTS_X_Y, Rig, add, echo, manifest, worker_settings};
use omuzaki_ai::ops::acknowledge::ModelAcknowledge;
use omuzaki_ai::ops::add::ModelAdd;
use omuzaki_ai::ops::full_name;
use omuzaki_ai::ops::list::ModelList;
use omuzaki_ai::ops::remove::ModelRemove;
use omuzaki_ai::ops::select::ModelSelect;
use omuzaki_ai::ops::status::ModelStatus;
use omuzaki_ai::sandbox::DEFAULT_MEMORY_MIB;
use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{Body, FetchError, Fetcher, ModelRegistry, NoProgress};
use omuzaki_ai_mock::{ECHO_MODEL, ECHO_MODEL_URL, MemFetcher};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, ErrorDetail, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, Scope,
    Services, ServicesBuilder,
};
use serde_json::Value;

fn person() -> Actor {
    Actor::Person { name: "Pat".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "helper".into(), model: None }
}

fn services(registry: Registry) -> Services {
    ServicesBuilder::new().insert::<dyn ModelRegistry>(Arc::new(registry)).build()
}

/// Plans `op` as `actor`: for these ops, planning is running (they change the registry, not the document).
fn run<O: Op>(op: &O, actor: &Actor, services: &Services) -> Result<Value, OpError> {
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, actor, &clock).with_services(services);
    op.plan(&ctx).map(|plan| plan.output.unwrap_or(Value::Null))
}

/// A job signal that cancels nothing and drops progress.
#[derive(Default)]
struct Signal(CancelFlag);

impl JobSignal for Signal {
    fn progress(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancel_flag(&self) -> CancelFlag {
        self.0.clone()
    }
    fn spend(&self, _: u64) -> Result<(), OpError> {
        Ok(())
    }
}

struct Doc(MemDoc);

impl JobDoc for Doc {
    fn read(&self, f: &mut dyn FnMut(&dyn omuzaki_doc_api::DocRead)) {
        f(&self.0);
    }
}

/// Plans and works `model.add` as `actor`, who may read the folders in `roots`.
fn add_op(op: &ModelAdd, actor: &Actor, services: &Services, roots: &[&std::path::Path]) -> Result<Value, OpError> {
    let grants = roots.iter().fold(Grants::of([Scope::EditLibrary, Scope::Fs]), |g, root| {
        g.with_fs_root(root.to_string_lossy().into_owned())
    });
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, actor, &clock).with_services(services).with_grants(&grants);
    op.plan(&ctx)?;
    let (doc, signal) = (Doc(MemDoc::new()), Signal::default());
    let job = JobCtx::new(JobId(1), actor, &doc, &signal).with_services(services).with_grants(&grants);
    op.work(&job)
}

fn detail_is_needs_person(e: &OpError) -> bool {
    matches!(e.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. }))
}

fn detail_is_person_only(e: &OpError) -> bool {
    matches!(e.detail.as_deref(), Some(ErrorDetail::PersonOnly { .. }))
}

#[test]
fn an_agent_asks_the_person_acknowledges_and_then_the_agent_may_switch() {
    let rig = Rig::new();
    let registry = rig.registry();
    let file = rig.fake("mine.fake.toml", "1.0", PORTS_X_Y);
    let entry = add(&registry, &echo(), vec![file.clone()], Some(manifest(&echo(), &[&file], "")), None);
    let services = services(registry);
    let name = full_name(&entry.model);
    let select = ModelSelect { slot: "test_echo@1".into(), model: Some(name.clone()) };

    let asked = run(&select, &agent(), &services).unwrap_err();
    assert!(detail_is_needs_person(&asked), "{asked:?}");
    let refused = run(&ModelAcknowledge { model: name.clone() }, &agent(), &services).unwrap_err();
    assert!(detail_is_person_only(&refused), "{refused:?}");

    let accepted = run(&ModelAcknowledge { model: name.clone() }, &person(), &services).unwrap();
    assert!(accepted["notice"].as_str().unwrap().contains("isn't made or distributed by Omuzaki"));
    let switched = run(&select, &agent(), &services).unwrap();
    assert_eq!(switched["selected"], name);
    let listing = run(&ModelList::default(), &agent(), &services).unwrap();
    let slot = listing["slots"].as_array().unwrap().iter().find(|s| s["info"]["slot"] == "test_echo@1").unwrap();
    assert_eq!(slot["selected"]["id"], "user-mine");
}

#[test]
fn the_list_names_models_as_the_ops_take_them_and_shows_no_local_path() {
    let rig = Rig::new();
    let registry = rig.registry();
    let file = rig.fake("mine.fake.toml", "1.0", PORTS_X_Y);
    let entry = add(&registry, &echo(), vec![file.clone()], Some(manifest(&echo(), &[&file], "")), Some("Pat"));
    let services = services(registry);
    let listing = run(&ModelList { slot: Some("test_echo@1".into()) }, &agent(), &services).unwrap();
    let models = listing["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["key"], full_name(&entry.model));
    assert_eq!(models[0]["status"]["state"], "ready");
    assert_eq!(models[0]["licence"]["spdx"], "MIT");
    assert!(models[0]["eval"]["checks"].is_array(), "the smoke test's result: {}", models[0]);
    assert!(models[0].get("warning").is_none());
    let text = listing.to_string();
    for folder in [rig.data.path(), rig.picked.path()] {
        assert!(!text.contains(&*folder.to_string_lossy()), "a local path leaked: {text}");
    }
}

#[test]
fn names_that_are_almost_right_get_a_did_you_mean() {
    let rig = Rig::new();
    let registry = rig.registry();
    let file = rig.fake("mine.fake.toml", "1.0", PORTS_X_Y);
    add(&registry, &echo(), vec![file.clone()], Some(manifest(&echo(), &[&file], "")), Some("Pat"));
    let services = services(registry);
    let slot = run(&ModelStatus { slot: "test_ecoh@1".into() }, &agent(), &services).unwrap_err();
    assert_eq!((slot.code, slot.field.as_deref()), (ErrorCode::NotFound, Some("slot")));
    assert!(slot.hint.unwrap().contains("did you mean \"test_echo@1\""));
    let model = run(&ModelRemove { model: "user-mien".into() }, &agent(), &services).unwrap_err();
    assert!(model.hint.unwrap().contains("did you mean \"user-mine\""));
}

#[test]
fn remove_forgets_the_model_and_a_dry_run_changes_nothing() {
    let rig = Rig::new();
    let registry = rig.registry();
    let file = rig.fake("mine.fake.toml", "1.0", PORTS_X_Y);
    add(&registry, &echo(), vec![file.clone()], Some(manifest(&echo(), &[&file], "")), Some("Pat"));
    let services = services(registry);
    let remove = ModelRemove { model: "user-mine".into() };
    let (doc, clock, actor) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), agent());
    let dry = OpCtx::new(&doc, &actor, &clock).with_services(&services).with_dry_run(true);
    remove.plan(&dry).unwrap();
    assert_eq!(rig.blobs().len(), 2, "a dry run removes nothing");
    run(&remove, &agent(), &services).unwrap();
    assert_eq!(rig.blobs(), Vec::<String>::new());
    assert_eq!(run(&ModelList::default(), &agent(), &services).unwrap()["models"], Value::Array(vec![]));
}

#[test]
fn model_add_reads_through_the_persons_roots_and_only_a_person_accepts_the_notice() {
    let rig = Rig::new();
    let services = services(rig.registry());
    rig.fake("mine.fake.toml", "1.0", PORTS_X_Y);
    let file = rig.picked.path().join("mine.fake.toml");
    std::fs::write(rig.picked.path().join("model.toml"), manifest(&echo(), &[&file], "").to_toml().unwrap()).unwrap();
    let path = |name: &str| rig.picked.path().join(name).to_string_lossy().into_owned();
    let op = |accept_notice| ModelAdd {
        slot: "test_echo@1".into(),
        path: path("mine.fake.toml"),
        more: vec![],
        manifest: Some(path("model.toml")),
        accept_notice,
    };

    let outside = add_op(&op(false), &agent(), &services, &[]).unwrap_err();
    assert_eq!(outside.code, ErrorCode::Denied, "no root was shared: {outside:?}");
    let agent_accepts = add_op(&op(true), &agent(), &services, &[rig.picked.path()]).unwrap_err();
    assert!(detail_is_person_only(&agent_accepts), "{agent_accepts:?}");

    let by_agent = add_op(&op(false), &agent(), &services, &[rig.picked.path()]).unwrap();
    assert_eq!(by_agent["status"]["state"], "needs_acknowledgement", "{by_agent}");
    assert!(by_agent["notice"].as_str().unwrap().starts_with("This model isn't made or distributed by Omuzaki."));
    let by_person = add_op(&op(true), &person(), &services, &[rig.picked.path()]).unwrap();
    assert_eq!(by_person["status"]["state"], "ready", "{by_person}");
    assert!(!by_person.to_string().contains(&*rig.picked.path().to_string_lossy()), "{by_person}");
}

/// Serves `bytes` for one URL a few bytes at a time, each read waiting until the test lets it go.
struct Trickle {
    url: &'static str,
    bytes: Vec<u8>,
    go: Arc<AtomicBool>,
}

struct Drip {
    bytes: Vec<u8>,
    at: usize,
    go: Arc<AtomicBool>,
}

impl Read for Drip {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while !self.go.swap(false, Ordering::AcqRel) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let n = buf.len().min(256 * 1024).min(self.bytes.len() - self.at);
        buf[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Fetcher for Trickle {
    fn open(&self, url: &str, _from: u64) -> Result<Body, FetchError> {
        if url != self.url {
            return Err(FetchError::Network { url: url.into(), reason: "not served here".into() });
        }
        Ok(Body { start: 0, reader: Box::new(Drip { bytes: self.bytes.clone(), at: 0, go: Arc::clone(&self.go) }) })
    }
}

fn shipped_registry(rig: &Rig, fetcher: impl Fetcher + 'static) -> Registry {
    let files = Arc::new(VerifiedCache::new(rig.data.path().join(CACHE_DIR), fetcher));
    let registry =
        Registry::new(rig.data.path(), files, "1.0.0".parse().unwrap(), RegistrySettings::default()).unwrap();
    registry.set_sandbox(worker_settings(DEFAULT_MEMORY_MIB));
    registry
}

/// The echo model padded to 4 MiB (the cache reports progress once per MiB), with a shipped manifest for it.
fn big_echo() -> (Vec<u8>, String) {
    let bytes = format!("{ECHO_MODEL}\n# {}\n", "x".repeat(4 << 20)).into_bytes();
    let pin = common::pin(&[BIG_URL], &bytes);
    let manifest = format!(
        "id = \"echo-big\"\nversion = \"1.0.0\"\nname = \"Echo (big)\"\nslot = \"test_echo@1\"\nruntime = \"fake\"\n\
         providers = [\"cpu\"]\n\n[[files]]\nname = \"echo.fake.toml\"\nsha256 = \"{}\"\nsize = {}\n\
         urls = [\"{BIG_URL}\"]\n\n[card]\nregister = \"omuzaki-ai tests\"\n\n[card.licence]\nspdx = \"MIT OR Apache-2.0\"\n",
        pin.sha256, pin.size
    );
    (bytes, manifest)
}

const BIG_URL: &str = "https://models.invalid/echo-big/1.0.0/echo.fake.toml";

#[test]
fn status_reports_the_percent_while_the_model_downloads_and_ready_after() {
    let rig = Rig::new();
    let go = Arc::new(AtomicBool::new(false));
    let (bytes, manifest) = big_echo();
    let fetcher = Trickle { url: BIG_URL, bytes, go: Arc::clone(&go) };
    let files = Arc::new(VerifiedCache::new(rig.data.path().join(CACHE_DIR), fetcher));
    let (app, settings) = ("1.0.0".parse().unwrap(), RegistrySettings::default());
    let registry = Registry::with_models(rig.data.path(), files, app, settings, [("echo-big".into(), manifest)]);
    let registry = Arc::new(registry.unwrap());
    let services =
        ServicesBuilder::new().insert::<dyn ModelRegistry>(Arc::clone(&registry) as Arc<dyn ModelRegistry>).build();
    let status = || run(&ModelStatus { slot: "test_echo@1".into() }, &agent(), &services).unwrap();
    assert_eq!(status(), serde_json::json!({ "state": "unavailable" }), "nothing installed yet");

    let model = registry.models(Some(&echo())).remove(0).model;
    let installing = {
        let registry = Arc::clone(&registry);
        std::thread::spawn(move || registry.install(&model, &NoProgress))
    };
    let seen = Mutex::new(Vec::new());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !installing.is_finished() {
        assert!(Instant::now() < deadline, "the install never finished");
        if let Some(percent) = status()["percent"].as_f64() {
            seen.lock().unwrap().push(percent);
        }
        go.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(2));
    }
    installing.join().unwrap().unwrap();
    let seen = seen.into_inner().unwrap();
    assert!(seen.windows(2).all(|w| w[0] <= w[1]) && seen.len() > 2, "the percent rises: {seen:?}");
    assert!(seen.last().unwrap() > &50.0, "{seen:?}");
    assert_eq!(status(), serde_json::json!({ "state": "ready" }));
}

#[test]
fn a_download_that_fails_its_hash_leaves_the_slot_unavailable() {
    let rig = Rig::new();
    let registry = shipped_registry(&rig, MemFetcher::new().serving(ECHO_MODEL_URL, "not the pinned bytes"));
    let model = registry.models(Some(&echo())).remove(0).model;
    assert!(registry.install(&model, &NoProgress).is_err());
    let services = services(registry);
    let status = run(&ModelStatus { slot: "test_echo@1".into() }, &agent(), &services).unwrap();
    assert_eq!(status["state"], "unavailable", "{status}");
}

#[test]
fn an_installed_model_is_ready() {
    let rig = Rig::new();
    let registry = shipped_registry(&rig, omuzaki_ai_mock::fixtures_fetcher());
    let model = registry.models(Some(&echo())).remove(0).model;
    registry.install(&model, &NoProgress).unwrap();
    let services = services(registry);
    let status = run(&ModelStatus { slot: "test_echo@1".into() }, &agent(), &services).unwrap();
    assert_eq!(status, serde_json::json!({ "state": "ready" }));
}
