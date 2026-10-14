//! The shipped-model registry against the mock's `fake` runtime and `test_echo@1` slot: install with its smoke test,
//! rollback, the choice chain, `min_app_version`, the state file and bad registrations. Everything is offline: the
//! files come from an in-memory fetcher.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::sync::Arc;

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{
    Body, FetchError, Fetcher, FileSource, ModelEntry, ModelRef, ModelRegistry, ModelStatus, NoProgress, Progress,
    RegistryError, Requester, SlotRef, TensorBuf, TensorRef, Version,
};
use omuzaki_ai_mock::{MemFetcher, fixtures_fetcher};

const MODELS: &str = "echo-fake";

fn echo_slot() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

fn version(text: &str) -> Version {
    text.parse().unwrap()
}

/// The `fake` runtime's model file: `y = scale * x`, with `tag` making the bytes (and so the digest) differ.
fn model_file(scale: f32, tag: &str) -> String {
    format!(
        "# {tag}\nscale = {scale:?}\nbias = 0.0\n\n[[inputs]]\nname = \"x\"\ndtype = \"f32\"\nshape = [\"any\"]\n\n\
         [[outputs]]\nname = \"y\"\ndtype = \"f32\"\nshape = [\"any\"]\n"
    )
}

fn url(version: &str) -> String {
    format!("https://models.invalid/{MODELS}/{version}/echo.fake.toml")
}

/// A shipped manifest for `echo-fake` at `version` pinned to `bytes`.
fn manifest(version: &str, bytes: &str, min_app: &str) -> (String, String) {
    let pin = common::pin(&[&url(version)], bytes.as_bytes());
    let text = format!(
        "id = \"{MODELS}\"\nversion = \"{version}\"\nname = \"Echo (fake)\"\nslot = \"test_echo@1\"\nruntime = \"fake\"\n\
         min_app_version = \"{min_app}\"\nproviders = [\"cpu\"]\n\n[[files]]\nname = \"echo.fake.toml\"\n\
         sha256 = \"{}\"\nsize = {}\nurls = [\"{}\"]\n\n[card]\nregister = \"omuzaki-ai-mock test fixtures\"\n\n\
         [card.licence]\nspdx = \"MIT OR Apache-2.0\"\n",
        pin.sha256,
        pin.size,
        url(version)
    );
    (MODELS.to_owned(), text)
}

/// A fetcher the test keeps a handle on after the cache owns it.
struct Shared(Arc<MemFetcher>);

impl Fetcher for Shared {
    fn open(&self, url: &str, from: u64) -> Result<Body, FetchError> {
        self.0.open(url, from)
    }
}

/// A data folder, a network and the registries opened over them.
struct Rig {
    dir: tempfile::TempDir,
    net: Arc<MemFetcher>,
}

impl Rig {
    fn new(served: &[(&str, &str)]) -> Self {
        let net = served.iter().fold(MemFetcher::new(), |net, (version, bytes)| net.serving(url(version), *bytes));
        Self { dir: tempfile::tempdir().unwrap(), net: Arc::new(net) }
    }

    fn open_with(&self, app: &str, models: Vec<(String, String)>) -> Registry {
        let files = Arc::new(VerifiedCache::new(self.dir.path().join(CACHE_DIR), Shared(self.net.clone())));
        Registry::with_models(self.dir.path(), files, version(app), RegistrySettings::default(), models).unwrap()
    }

    fn open(&self, models: Vec<(String, String)>) -> Registry {
        self.open_with("1.0.0", models)
    }
}

fn entry(registry: &Registry, version: &str) -> ModelEntry {
    registry.models(Some(&echo_slot())).into_iter().find(|e| e.model.version == self::version(version)).unwrap()
}

fn model(registry: &Registry, version: &str) -> ModelRef {
    entry(registry, version).model
}

fn active(registry: &Registry) -> Option<ModelRef> {
    registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap().active
}

fn disabled_reason(entry: &ModelEntry) -> String {
    match &entry.status {
        ModelStatus::Disabled { reason } => reason.clone(),
        other => panic!("expected Disabled, got {other:?}"),
    }
}

#[test]
fn the_mocks_echo_fake_installs_and_becomes_active() {
    let rig = Rig { dir: tempfile::tempdir().unwrap(), net: Arc::new(fixtures_fetcher()) };
    let files = Arc::new(VerifiedCache::new(rig.dir.path().join(CACHE_DIR), Shared(rig.net.clone())));
    let registry = Registry::new(rig.dir.path(), files, version("1.0.0"), RegistrySettings::default()).unwrap();
    let echo = model(&registry, "1.0.0");
    assert_eq!(entry(&registry, "1.0.0").status, ModelStatus::NotInstalled);
    assert_eq!(active(&registry), None);

    registry.install(&echo, &NoProgress).unwrap();

    let installed = entry(&registry, "1.0.0");
    assert_eq!(installed.status, ModelStatus::Ready);
    assert!(installed.active);
    assert!(installed.eval.unwrap().passed(), "the smoke test report is stored");
    assert_eq!(active(&registry), Some(echo));
}

#[test]
fn an_offline_install_fails_with_a_fetch_error_and_changes_nothing() {
    let (id, v1) = manifest("1.0.0", &model_file(1.0, "one"), "0.1.0");
    let rig = Rig::new(&[("1.0.0", &model_file(1.0, "one"))]);
    let registry = rig.open(vec![(id, v1)]);
    rig.net.set_offline(true);

    let err = registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap_err();

    assert!(matches!(err, RegistryError::Fetch(_)), "{err:?}");
    assert_eq!(entry(&registry, "1.0.0").status, ModelStatus::NotInstalled);
    assert_eq!(active(&registry), None);
}

#[test]
fn a_file_with_the_wrong_bytes_is_refused_and_nothing_changes() {
    let (id, v1) = manifest("1.0.0", &model_file(1.0, "one"), "0.1.0");
    let same_size_other_bytes = model_file(1.0, "ONE");
    let rig = Rig::new(&[("1.0.0", &same_size_other_bytes)]);
    let registry = rig.open(vec![(id, v1)]);

    let err = registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap_err();

    assert!(matches!(err, RegistryError::Fetch(_)), "{err:?}");
    assert_eq!(entry(&registry, "1.0.0").status, ModelStatus::NotInstalled);
    assert!(!rig.dir.path().join("models/state.toml").exists(), "no state was written for a refused file");
}

#[test]
fn a_cancelled_install_replaces_nothing() {
    struct Cancelled;
    impl Progress for Cancelled {
        fn stage(&self, _: &str, _: f32) {}
        fn is_cancelled(&self) -> bool {
            true
        }
    }
    let (id, v1) = manifest("1.0.0", &model_file(1.0, "one"), "0.1.0");
    let rig = Rig::new(&[("1.0.0", &model_file(1.0, "one"))]);
    let registry = rig.open(vec![(id, v1)]);

    let err = registry.install(&model(&registry, "1.0.0"), &Cancelled).unwrap_err();

    assert_eq!(err, RegistryError::Cancelled);
    assert_eq!(entry(&registry, "1.0.0").status, ModelStatus::NotInstalled);
}

/// Two versions that both pass, plus a 3.0.0 whose model fails the smoke test (it doubles its input).
fn three_versions() -> (Rig, Vec<(String, String)>) {
    let rig = Rig::new(&[
        ("1.0.0", &model_file(1.0, "one")),
        ("2.0.0", &model_file(1.0, "two")),
        ("3.0.0", &model_file(2.0, "three")),
    ]);
    let models = vec![
        manifest("1.0.0", &model_file(1.0, "one"), "0.1.0"),
        manifest("2.0.0", &model_file(1.0, "two"), "0.1.0"),
        manifest("3.0.0", &model_file(2.0, "three"), "0.1.0"),
    ];
    (rig, models)
}

#[test]
fn an_upgrade_that_fails_the_smoke_test_leaves_the_old_version_active_and_is_disabled_with_the_metric() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();

    let err = registry.install(&model(&registry, "3.0.0"), &NoProgress).unwrap_err();

    let RegistryError::BelowFloor(report) = err else { panic!("expected BelowFloor, got {err:?}") };
    assert!(!report.passed());
    assert_eq!(active(&registry), Some(model(&registry, "1.0.0")), "rolled back to 1.0.0 (never left it)");
    let failed = entry(&registry, "3.0.0");
    let reason = disabled_reason(&failed);
    assert!(reason.contains("max_abs_error"), "the failing metric is named: {reason}");
    assert!(failed.eval.is_some(), "the failing report is kept for the Models page");
    assert!(!failed.active);
}

#[test]
fn a_manual_rollback_goes_back_one_version_and_then_has_nowhere_to_go() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);
    assert_eq!(
        registry.rollback(&echo_slot()).unwrap_err(),
        RegistryError::NoRollback(echo_slot()),
        "nothing installed"
    );
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();
    assert!(matches!(registry.rollback(&echo_slot()), Err(RegistryError::NoRollback(_))), "only one version");
    registry.install(&model(&registry, "2.0.0"), &NoProgress).unwrap();
    assert_eq!(active(&registry), Some(model(&registry, "2.0.0")));

    let back = registry.rollback(&echo_slot()).unwrap();

    assert_eq!(back, model(&registry, "1.0.0"));
    assert_eq!(active(&registry), Some(back));
    assert_eq!(entry(&registry, "2.0.0").status, ModelStatus::Ready, "the version we left stays installed");
    assert!(matches!(registry.rollback(&echo_slot()), Err(RegistryError::NoRollback(_))));
}

#[test]
fn a_rollback_target_survives_an_app_update_that_stops_registering_it() {
    let (rig, models) = three_versions();
    let registry = rig.open(models.clone());
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();
    registry.install(&model(&registry, "2.0.0"), &NoProgress).unwrap();
    let v1 = model(&registry, "1.0.0");
    drop(registry);

    let newer_app = rig.open(vec![models[1].clone()]);

    assert_eq!(entry(&newer_app, "1.0.0").status, ModelStatus::Ready, "listed from the state file");
    assert_eq!(newer_app.rollback(&echo_slot()).unwrap(), v1);
}

#[test]
fn a_model_that_needs_a_newer_app_is_disabled_with_the_reason() {
    let rig = Rig::new(&[("1.0.0", &model_file(1.0, "one"))]);
    let registry = rig.open_with("1.0.0", vec![manifest("1.0.0", &model_file(1.0, "one"), "9.0.0")]);

    let listed = entry(&registry, "1.0.0");

    assert!(disabled_reason(&listed).contains("9.0.0"));
    let err = registry.install(&listed.model, &NoProgress).unwrap_err();
    assert!(matches!(err, RegistryError::NotUsable { .. }), "{err:?}");
}

#[test]
fn select_overrides_the_default_and_none_returns_to_it() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);
    let (v1, v2) = (model(&registry, "1.0.0"), model(&registry, "2.0.0"));
    let not_installed = registry.select(&echo_slot(), Some(&v1), Requester::Person).unwrap_err();
    assert!(matches!(not_installed, RegistryError::NotUsable { .. }), "{not_installed:?}");
    registry.install(&v1, &NoProgress).unwrap();
    registry.install(&v2, &NoProgress).unwrap();

    registry.select(&echo_slot(), Some(&v1), Requester::Agent).unwrap();
    assert_eq!(active(&registry), Some(v1.clone()));
    let status = registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap();
    assert_eq!(status.selected, Some(v1));

    registry.select(&echo_slot(), None, Requester::Person).unwrap();
    assert_eq!(active(&registry), Some(v2));
}

#[test]
fn select_refuses_a_model_of_another_slot_and_an_unknown_one() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);
    let v1 = model(&registry, "1.0.0");
    let stream: SlotRef = "test_stream@1".parse().unwrap();

    let wrong = registry.select(&stream, Some(&v1), Requester::Person).unwrap_err();
    assert!(matches!(wrong, RegistryError::WrongSlot { .. }), "{wrong:?}");
    let unknown = ModelRef { version: version("7.0.0"), ..v1 };
    let err = registry.select(&echo_slot(), Some(&unknown), Requester::Person).unwrap_err();
    assert!(matches!(err, RegistryError::UnknownModel(_)), "{err:?}");
}

#[test]
fn state_survives_a_reload() {
    let (rig, models) = three_versions();
    let registry = rig.open(models.clone());
    let (v1, v2) = (model(&registry, "1.0.0"), model(&registry, "2.0.0"));
    registry.install(&v1, &NoProgress).unwrap();
    registry.install(&v2, &NoProgress).unwrap();
    registry.select(&echo_slot(), Some(&v1), Requester::Person).unwrap();
    let _ = registry.install(&model(&registry, "3.0.0"), &NoProgress);
    drop(registry);

    let reopened = rig.open(models);

    assert_eq!(entry(&reopened, "2.0.0").status, ModelStatus::Ready);
    assert!(entry(&reopened, "2.0.0").eval.is_some());
    assert_eq!(active(&reopened), Some(v1.clone()));
    assert!(disabled_reason(&entry(&reopened, "3.0.0")).contains("max_abs_error"));
    assert_eq!(reopened.rollback(&echo_slot()).unwrap(), v1);
}

#[test]
fn a_corrupted_state_file_is_reported_and_rebuilt_from_the_downloaded_files() {
    let (rig, models) = three_versions();
    let registry = rig.open(models.clone());
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();
    drop(registry);
    let state = rig.dir.path().join("models/state.toml");
    std::fs::write(&state, "this is [not toml").unwrap();

    let reopened = rig.open(models);

    assert!(reopened.notes().iter().any(|n| n.contains("state file")), "{:?}", reopened.notes());
    assert_eq!(entry(&reopened, "1.0.0").status, ModelStatus::Ready, "found again from the cache");
    assert_eq!(entry(&reopened, "2.0.0").status, ModelStatus::NotInstalled);
    assert_eq!(active(&reopened), Some(model(&reopened, "1.0.0")));
    assert!(rig.dir.path().join("models/state.toml.corrupt").exists(), "the broken file is kept");
}

#[test]
fn open_without_an_installed_model_says_what_the_app_does_instead() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);

    let Err(err) = registry.open(&echo_slot(), None) else { panic!("nothing is installed") };

    assert_eq!(err, RegistryError::NoModel { slot: echo_slot(), without_model: "the test sees no output".into() });
}

#[test]
fn open_runs_the_active_model_and_an_explicit_model_wins() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);
    let (v1, v2) = (model(&registry, "1.0.0"), model(&registry, "2.0.0"));
    registry.install(&v1, &NoProgress).unwrap();
    registry.install(&v2, &NoProgress).unwrap();

    let mut by_default = registry.open(&echo_slot(), None).unwrap();
    let explicit = registry.open(&echo_slot(), Some(&v1)).unwrap();

    assert_eq!(by_default.info().model, v2);
    assert_eq!(explicit.info().model, v1);
    let mut out = TensorBuf::for_outputs(by_default.signature());
    by_default.run(&[TensorRef::f32(&[0.5, 2.0], &[2])], &mut out).unwrap();
    assert_eq!(out[0].as_f32().unwrap(), [0.5, 2.0]);
}

#[test]
fn streams_are_wrong_mode_for_call_slots_and_missing_for_stream_slots() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);
    let stream: SlotRef = "test_stream@1".parse().unwrap();

    assert!(matches!(registry.open_stream(&echo_slot(), None), Err(RegistryError::WrongMode(_))));
    assert!(matches!(registry.open_stream(&stream, None), Err(RegistryError::NoModel { .. })));
    assert!(matches!(registry.open(&stream, None), Err(RegistryError::WrongMode(_))));
}

#[test]
fn bad_registrations_are_listed_as_disabled_or_noted_and_never_panic() {
    let rig = Rig::new(&[]);
    let (_, good) = manifest("1.0.0", &model_file(1.0, "one"), "0.1.0");
    let registry =
        rig.open(vec![("not-the-id".to_owned(), good), ("garbage".to_owned(), "this is [not toml".to_owned())]);

    let listed = entry(&registry, "1.0.0");

    assert!(disabled_reason(&listed).contains("not-the-id"));
    assert!(registry.notes().iter().any(|n| n.contains("garbage")), "{:?}", registry.notes());
    let err = registry.install(&listed.model, &NoProgress).unwrap_err();
    assert!(matches!(err, RegistryError::NotUsable { .. }));
}

/// A second handle on the cache the registry's files live in, to ask what is stored there.
fn cache_of(rig: &Rig) -> VerifiedCache<Shared> {
    VerifiedCache::new(rig.dir.path().join(CACHE_DIR), Shared(rig.net.clone()))
}

fn stored(rig: &Rig, bytes: &str) -> bool {
    let pin = common::pin(&[&url("1.0.0")], bytes.as_bytes());
    cache_of(rig).lookup(pin.sha256).unwrap().is_some()
}

#[test]
fn removing_a_model_forgets_its_own_blobs() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();
    registry.install(&model(&registry, "2.0.0"), &NoProgress).unwrap();
    assert!(stored(&rig, &model_file(1.0, "two")), "2.0.0's file is stored before the removal");

    registry.remove(&model(&registry, "2.0.0")).unwrap();

    assert!(!stored(&rig, &model_file(1.0, "two")), "its file is forgotten");
    assert!(stored(&rig, &model_file(1.0, "one")), "another model's file stays");
    assert_eq!(entry(&registry, "1.0.0").status, ModelStatus::Ready);
}

#[test]
fn removing_a_model_keeps_a_blob_another_installed_model_still_uses() {
    // 1.0.0 and 2.0.0 are served the same bytes, so they share one blob.
    let rig = Rig::new(&[("1.0.0", &model_file(1.0, "one")), ("2.0.0", &model_file(1.0, "one"))]);
    let models =
        vec![manifest("1.0.0", &model_file(1.0, "one"), "0.1.0"), manifest("2.0.0", &model_file(1.0, "one"), "0.1.0")];
    let registry = rig.open(models);
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();
    registry.install(&model(&registry, "2.0.0"), &NoProgress).unwrap();

    registry.remove(&model(&registry, "2.0.0")).unwrap();

    assert!(stored(&rig, &model_file(1.0, "one")), "1.0.0 still uses it");
    assert_eq!(entry(&registry, "1.0.0").status, ModelStatus::Ready);
}

#[test]
fn recovery_looks_where_the_registry_files_are_even_outside_the_data_folder() {
    let (rig, models) = three_versions();
    let elsewhere = tempfile::tempdir().unwrap();
    let open = |models: Vec<(String, String)>| {
        let files = Arc::new(VerifiedCache::new(elsewhere.path().join(CACHE_DIR), Shared(rig.net.clone())));
        Registry::with_models(rig.dir.path(), files, version("1.0.0"), RegistrySettings::default(), models).unwrap()
    };
    let registry = open(models.clone());
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();
    drop(registry);
    std::fs::write(rig.dir.path().join("models/state.toml"), "this is [not toml").unwrap();

    let reopened = open(models);

    assert_eq!(entry(&reopened, "1.0.0").status, ModelStatus::Ready, "found in the registry's own cache");
}

#[test]
fn remove_uninstalls_and_deletes_the_files() {
    let (rig, models) = three_versions();
    let registry = rig.open(models);
    let (v1, v2) = (model(&registry, "1.0.0"), model(&registry, "2.0.0"));
    registry.install(&v1, &NoProgress).unwrap();
    registry.install(&v2, &NoProgress).unwrap();
    registry.select(&echo_slot(), Some(&v2), Requester::Person).unwrap();
    let blobs = rig.dir.path().join(CACHE_DIR).join("sha256");
    let before = common::cache_files(&rig.dir.path().join(CACHE_DIR)).len();

    registry.remove(&v2).unwrap();

    assert_eq!(entry(&registry, "2.0.0").status, ModelStatus::NotInstalled);
    assert_eq!(active(&registry), Some(v1), "the slot falls back");
    assert!(common::cache_files(&rig.dir.path().join(CACHE_DIR)).len() < before, "{blobs:?} shrank");
    registry.install(&model(&registry, "2.0.0"), &NoProgress).unwrap();
    assert_eq!(entry(&registry, "2.0.0").status, ModelStatus::Ready, "it can be installed again");
}

#[test]
fn recovery_tests_models_again_so_a_failed_upgrade_stays_disabled() {
    let (rig, models) = three_versions();
    let registry = rig.open(models.clone());
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();
    assert!(registry.install(&model(&registry, "3.0.0"), &NoProgress).is_err(), "its files are cached anyway");
    drop(registry);
    std::fs::write(rig.dir.path().join("models/state.toml"), "this is [not toml").unwrap();

    let reopened = rig.open(models);

    assert_eq!(entry(&reopened, "1.0.0").status, ModelStatus::Ready);
    assert!(disabled_reason(&entry(&reopened, "3.0.0")).contains("max_abs_error"), "not promoted on its bytes alone");
    assert_eq!(active(&reopened), Some(model(&reopened, "1.0.0")));
}

#[test]
fn a_model_of_another_slot_never_runs_in_this_one_even_with_the_same_tensors() {
    let (rig, mut models) = three_versions();
    let bytes = model_file(1.0, "one"); // the same file as echo 1.0.0, so the same URL serves it
    let (_, other) = manifest("1.0.0", &bytes, "0.1.0");
    let other = other.replace("id = \"echo-fake\"", "id = \"stream-fake\"").replace("test_echo@1", "test_stream@1");
    models.push(("stream-fake".to_owned(), other));
    let registry = rig.open(models);
    let stream_model = registry.models(None).into_iter().find(|e| e.model.id.as_str() == "stream-fake").unwrap().model;
    registry.install(&stream_model, &NoProgress).unwrap();

    let Err(err) = registry.open(&echo_slot(), Some(&stream_model)) else { panic!("ran in the wrong slot") };

    assert!(matches!(err, RegistryError::WrongSlot { .. }), "{err:?}");
    assert_eq!(active(&registry), None, "and it is not the echo slot's default either");
}

#[test]
fn a_raised_min_app_version_disables_an_installed_model_everywhere() {
    let rig = Rig::new(&[("1.0.0", &model_file(1.0, "one"))]);
    let registry = rig.open(vec![manifest("1.0.0", &model_file(1.0, "one"), "0.1.0")]);
    registry.install(&model(&registry, "1.0.0"), &NoProgress).unwrap();
    drop(registry);

    let reopened = rig.open(vec![manifest("1.0.0", &model_file(1.0, "one"), "9.0.0")]);

    let listed = entry(&reopened, "1.0.0");
    assert!(disabled_reason(&listed).contains("9.0.0"));
    assert_eq!(active(&reopened), None);
    let selected = reopened.select(&echo_slot(), Some(&listed.model), Requester::Person).unwrap_err();
    assert!(matches!(selected, RegistryError::NotUsable { .. }), "{selected:?}");
    assert!(matches!(reopened.open(&echo_slot(), None), Err(RegistryError::NoModel { .. })));
}
