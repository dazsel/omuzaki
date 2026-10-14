//! `sync_catalogue` against the mock's `fake` runtime and `test_echo@1` slot: models delivered between releases are
//! imported, installed, selected and rolled back like shipped ones, refreshes change nothing, and a poisoned or
//! colliding import is revoked when the root raises the epoch. Offline; every scenario is checked after a restart of
//! the registry too.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::sync::Arc;

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache, model_ref};
use omuzaki_ai_api::{
    CatalogueEntry, ImportError, ModelEntry, ModelRef, ModelRegistry, ModelStatus, NoProgress, Origin, RegistryError,
    Requester, Sha256, SlotRef, Version,
};
use omuzaki_ai_mock::MemFetcher;
use sha2::{Digest, Sha256 as Hasher};

const ID: &str = "silero-vad";
const VERSIONS: [&str; 4] = ["1.0.0", "1.1.0", "1.2.0", "4294967295.0.0"];
const TAGS: [&str; 3] = ["a", "b", "c"];

fn echo_slot() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

fn version(text: &str) -> Version {
    text.parse().unwrap()
}

/// The `fake` runtime's model file (`y = x`); `tag` makes the bytes, and so the digest, differ.
fn model_file(tag: &str) -> String {
    format!(
        "# {tag}\nscale = 1.0\nbias = 0.0\n\n[[inputs]]\nname = \"x\"\ndtype = \"f32\"\nshape = [\"any\"]\n\n\
         [[outputs]]\nname = \"y\"\ndtype = \"f32\"\nshape = [\"any\"]\n"
    )
}

fn url(id: &str, version: &str, tag: &str) -> String {
    format!("https://models.invalid/{id}/{version}-{tag}/echo.fake.toml")
}

/// A manifest file's text for `id` at `version` whose model file is variant `tag`.
fn manifest_text(id: &str, slot: &str, version: &str, tag: &str, min_app: &str) -> String {
    let bytes = model_file(&format!("{id} {version} {tag}"));
    let pin = common::pin(&[&url(id, version, tag)], bytes.as_bytes());
    format!(
        "id = \"{id}\"\nversion = \"{version}\"\nname = \"Silero\"\nslot = \"{slot}\"\nruntime = \"fake\"\n\
         min_app_version = \"{min_app}\"\nproviders = [\"cpu\"]\n\n[[files]]\nname = \"echo.fake.toml\"\n\
         sha256 = \"{}\"\nsize = {}\nurls = [\"{}\"]\n\n[card]\nregister = \"omuzaki-ai-mock test fixtures\"\n\n\
         [card.licence]\nspdx = \"MIT OR Apache-2.0\"\n",
        pin.sha256,
        pin.size,
        url(id, version, tag)
    )
}

/// What `omuzaki-update` hands over: the manifest and the hash of its file's bytes.
fn entry_of(text: &str) -> CatalogueEntry {
    let manifest = omuzaki_ai_api::ModelManifest::from_toml(text).unwrap();
    CatalogueEntry { manifest, manifest_sha256: Sha256(Hasher::digest(text.as_bytes()).into()) }
}

fn entry(version: &str, tag: &str) -> CatalogueEntry {
    entry_of(&manifest_text(ID, "test_echo@1", version, tag, "0.1.0"))
}

fn model(entry: &CatalogueEntry) -> ModelRef {
    model_ref(&entry.manifest)
}

struct Shared(Arc<MemFetcher>);

impl omuzaki_ai_api::Fetcher for Shared {
    fn open(&self, url: &str, from: u64) -> Result<omuzaki_ai_api::Body, omuzaki_ai_api::FetchError> {
        self.0.open(url, from)
    }
}

/// A data folder and a network serving every variant of every version.
struct Rig {
    dir: tempfile::TempDir,
    net: Arc<MemFetcher>,
    shipped: (String, String),
}

impl Rig {
    fn new() -> Self {
        let mut net = MemFetcher::new();
        for v in VERSIONS {
            for tag in TAGS {
                net = net.serving(url(ID, v, tag), model_file(&format!("{ID} {v} {tag}")));
            }
        }
        // The shipped 1.0.0 is variant "a" of the same family; the app registers it with `register_model!`.
        let shipped = (ID.to_owned(), manifest_text(ID, "test_echo@1", "1.0.0", "a", "0.1.0"));
        Self { dir: tempfile::tempdir().unwrap(), net: Arc::new(net), shipped }
    }

    /// Opens (or reopens: a restart) the registry over the same folder.
    fn open(&self, app: &str) -> Registry {
        let files = Arc::new(VerifiedCache::new(self.dir.path().join(CACHE_DIR), Shared(self.net.clone())));
        let models = vec![self.shipped.clone()];
        Registry::with_models(self.dir.path(), files, version(app), RegistrySettings::default(), models).unwrap()
    }

    fn state_text(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("models/state.toml")).unwrap_or_default()
    }

    /// A registry with the shipped 1.0.0 installed.
    fn installed(&self) -> Registry {
        let registry = self.open("1.0.0");
        let shipped = registry.models(None).into_iter().find(|e| e.model.version == version("1.0.0")).unwrap();
        registry.install(&shipped.model, &NoProgress).unwrap();
        registry
    }
}

fn listed(registry: &Registry, model: &ModelRef) -> ModelEntry {
    registry.models(None).into_iter().find(|e| &e.model == model).unwrap_or_else(|| panic!("{model} isn't listed"))
}

fn active(registry: &Registry) -> Option<ModelRef> {
    registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap().active
}

fn selected(registry: &Registry) -> Option<ModelRef> {
    registry.slots().into_iter().find(|s| s.info.slot == echo_slot()).unwrap().selected
}

fn shipped_ref(registry: &Registry) -> ModelRef {
    registry.models(None).into_iter().find(|e| e.origin == Origin::Shipped).unwrap().model
}

#[test]
fn a_catalogue_upgrade_of_a_shipped_id_installs_selects_and_rolls_back() {
    let rig = Rig::new();
    let registry = rig.installed();
    let shipped = shipped_ref(&registry);
    let new = entry("1.1.0", "a");

    let report = registry.sync_catalogue(1, 1, std::slice::from_ref(&new)).unwrap();

    assert_eq!(report.imported, vec![model(&new)]);
    assert_eq!(active(&registry), Some(shipped.clone()), "nothing changes until the person installs it");
    // The same checks on the live registry and on one reopened over the same folder (no `register_model!` knows 1.1.0).
    upgrade_and_roll_back(&registry, &shipped, &model(&new));
    drop(registry);
    upgrade_and_roll_back(&rig.open("1.0.0"), &shipped, &model(&new));
}

fn upgrade_and_roll_back(registry: &Registry, shipped: &ModelRef, new: &ModelRef) {
    let listed_new = listed(registry, new);
    assert_eq!(listed_new.origin, Origin::Catalogue);
    registry.install(new, &NoProgress).unwrap();
    assert_eq!(active(registry), Some(new.clone()));
    registry.select(&echo_slot(), Some(new), Requester::Person).unwrap();
    assert_eq!(selected(registry), Some(new.clone()));
    assert_eq!(listed(registry, new).status, ModelStatus::Ready);

    assert_eq!(&registry.rollback(&echo_slot()).unwrap(), shipped, "the shipped version stayed as the target");
    registry.select(&echo_slot(), None, Requester::Person).unwrap();
    assert_eq!(active(registry).as_ref(), Some(shipped));
    assert_eq!(listed(registry, shipped).origin, Origin::Shipped);
    // Back to the upgraded state for the next round.
    registry.install(new, &NoProgress).unwrap();
}

#[test]
fn a_catalogue_only_id_installs_and_survives_a_restart() {
    let rig = Rig::new();
    let registry = rig.open("1.0.0");
    let extra = entry_of(&manifest_text(ID, "test_echo@1", "1.2.0", "a", "0.1.0"));

    registry.sync_catalogue(1, 1, std::slice::from_ref(&extra)).unwrap();
    registry.install(&model(&extra), &NoProgress).unwrap();
    drop(registry);

    let registry = rig.open("1.0.0");
    assert_eq!(active(&registry), Some(model(&extra)));
    assert_eq!(listed(&registry, &model(&extra)).origin, Origin::Catalogue);
    assert_eq!(listed(&registry, &model(&extra)).status, ModelStatus::Ready);
}

#[test]
fn refreshing_and_restarting_change_nothing_but_provenance() {
    let rig = Rig::new();
    let registry = rig.installed();
    let new = entry("1.1.0", "a");
    registry.sync_catalogue(1, 1, std::slice::from_ref(&new)).unwrap();
    let before = registry.models(None);
    assert!(rig.state_text().contains(&new.manifest_sha256.to_string()), "the manifest hash is persisted");

    let again = registry.sync_catalogue(1, 2, std::slice::from_ref(&new)).unwrap();
    assert_eq!((again.imported.len(), again.unchanged), (0, vec![model(&new)]));
    drop(registry);
    let registry = rig.open("1.0.0");
    let after_restart = registry.sync_catalogue(2, 3, std::slice::from_ref(&new)).unwrap();

    assert_eq!(after_restart.unchanged, vec![model(&new)]);
    assert!(after_restart.revoked.is_empty() && after_restart.superseded.is_empty());
    assert_eq!(registry.models(None), before);
    let text = rig.state_text();
    assert!(text.contains(&new.manifest_sha256.to_string()));
    assert!(text.contains("epoch = 2"), "provenance moved to the new epoch: {text}");
}

#[test]
fn a_different_manifest_for_a_version_in_the_same_epoch_is_a_conflict_that_changes_nothing() {
    let rig = Rig::new();
    let registry = rig.installed();
    let first = entry("1.1.0", "a");
    registry.sync_catalogue(1, 1, std::slice::from_ref(&first)).unwrap();
    let (state, models) = (rig.state_text(), registry.models(None));

    let err = registry.sync_catalogue(1, 2, &[entry("1.1.0", "b")]).unwrap_err();
    assert_eq!(err, ImportError::Conflict { model: model(&entry("1.1.0", "b")) });
    // Inside one catalogue too, and it aborts the other entries' import.
    let twice = [entry("1.2.0", "a"), entry("1.1.0", "b"), entry("1.1.0", "c")];
    assert!(matches!(registry.sync_catalogue(1, 3, &twice), Err(ImportError::Conflict { .. })));

    assert_eq!(rig.state_text(), state);
    assert_eq!(registry.models(None), models);
    drop(registry);
    assert_eq!(rig.open("1.0.0").models(None), models);
}

#[test]
fn a_poisoned_version_order_is_revoked_when_the_epoch_is_raised() {
    let rig = Rig::new();
    let registry = rig.installed();
    let shipped = shipped_ref(&registry);
    let poison = entry("4294967295.0.0", "a");
    registry.sync_catalogue(1, 1, std::slice::from_ref(&poison)).unwrap();
    registry.install(&model(&poison), &NoProgress).unwrap();
    registry.select(&echo_slot(), Some(&model(&poison)), Requester::Person).unwrap();
    assert!(matches!(registry.sync_catalogue(1, 2, &[entry("1.1.0", "a")]), Err(ImportError::NotNewer { .. })));

    let legit = entry("1.1.0", "a");
    let report = registry.sync_catalogue(2, 3, std::slice::from_ref(&legit)).unwrap();

    assert_eq!((report.revoked, report.imported), (vec![model(&poison)], vec![model(&legit)]));
    for registry in [&registry as &Registry, &rig.open("1.0.0")] {
        assert_eq!(listed(registry, &model(&poison)).status, ModelStatus::Revoked);
        assert_eq!(selected(registry), None, "the selection fell back");
        assert_eq!(active(registry), Some(shipped.clone()));
        assert!(matches!(
            registry.select(&echo_slot(), Some(&model(&poison)), Requester::Person),
            Err(RegistryError::NotUsable { .. })
        ));
        assert!(matches!(registry.install(&model(&poison), &NoProgress), Err(RegistryError::NotUsable { .. })));
    }
    registry.install(&model(&legit), &NoProgress).unwrap();
    assert_eq!(active(&registry), Some(model(&legit)));
}

#[test]
fn a_poisoned_hash_for_a_real_version_is_superseded_when_the_epoch_is_raised() {
    let rig = Rig::new();
    let registry = rig.installed();
    let (poisoned, real, other) = (entry("1.1.0", "a"), entry("1.1.0", "b"), entry("1.2.0", "a"));
    registry.sync_catalogue(1, 1, std::slice::from_ref(&poisoned)).unwrap();
    registry.sync_catalogue(1, 2, &[entry("1.1.0", "a")]).unwrap();

    let report = registry.sync_catalogue(2, 3, &[real.clone(), other.clone()]).unwrap();

    assert_eq!(report.superseded, vec![model(&poisoned)]);
    assert_eq!(report.imported, vec![model(&real), model(&other)]);
    for registry in [&registry as &Registry, &rig.open("1.0.0")] {
        assert_eq!(listed(registry, &model(&poisoned)).status, ModelStatus::Revoked);
        assert_eq!(listed(registry, &model(&real)).status, ModelStatus::NotInstalled);
        assert_eq!(listed(registry, &model(&other)).origin, Origin::Catalogue);
        assert!(
            matches!(registry.sync_catalogue(2, 4, &[entry("1.1.0", "c")]), Err(ImportError::Conflict { .. })),
            "a same-epoch hash change is still refused"
        );
    }
    registry.install(&model(&real), &NoProgress).unwrap();
}

#[test]
fn a_user_id_a_new_app_requirement_and_an_unknown_slot_are_handled_one_by_one() {
    let rig = Rig::new();
    let registry = rig.installed();

    let user = entry_of(&manifest_text("user-voice", "test_echo@1", "1.0.0", "a", "0.1.0"));
    assert_eq!(registry.sync_catalogue(1, 1, &[user]), Err(ImportError::UserId("user-voice".parse().unwrap())));

    let future = entry_of(&manifest_text(ID, "test_echo@1", "1.1.0", "a", "1.5.0"));
    let unknown = entry_of(&manifest_text("other", "no_such_slot@1", "1.0.0", "a", "0.1.0"));
    let report = registry.sync_catalogue(1, 2, &[future.clone(), unknown]).unwrap();

    assert_eq!(report.hidden, vec!["other".parse().unwrap()]);
    for registry in [&registry as &Registry, &rig.open("1.0.0")] {
        let ModelStatus::Disabled { reason } = listed(registry, &model(&future)).status else { panic!() };
        assert!(reason.contains("needs Omuzaki 1.5.0"), "{reason}");
        assert!(registry.models(None).iter().all(|e| e.name != "other"));
    }
    assert!(matches!(registry.install(&model(&future), &NoProgress), Err(RegistryError::NotUsable { .. })));
}

#[test]
fn a_refused_catalogue_changes_nothing_not_even_its_good_entries() {
    let rig = Rig::new();
    let registry = rig.installed();
    let (state, models) = (rig.state_text(), registry.models(None));
    let good = entry("1.1.0", "a");

    let old = entry("1.0.0", "b"); // the id's version 1.0.0 is shipped with other bytes
    assert!(matches!(registry.sync_catalogue(1, 1, &[good.clone(), old]), Err(ImportError::Conflict { .. })));
    let other_slot = entry_of(&manifest_text(ID, "test_stream@1", "1.2.0", "a", "0.1.0"));
    assert!(matches!(
        registry.sync_catalogue(1, 2, &[good.clone(), other_slot]),
        Err(ImportError::SlotMismatch { .. })
    ));
    let mut insecure = entry("1.2.0", "a");
    insecure.manifest.files[0].urls = vec!["http://insecure.invalid/x".into()];
    assert!(matches!(registry.sync_catalogue(1, 3, &[good, insecure]), Err(ImportError::Invalid(_))));

    assert_eq!(rig.state_text(), state);
    assert_eq!(registry.models(None), models);
}

#[test]
fn collision_recovery_works_when_a_newer_version_is_already_imported() {
    let rig = Rig::new();
    let registry = rig.installed();
    let (poisoned, real, newer) = (entry("1.1.0", "a"), entry("1.1.0", "b"), entry("1.2.0", "a"));
    registry.sync_catalogue(1, 1, &[poisoned.clone(), newer.clone()]).unwrap();

    let report = registry.sync_catalogue(2, 2, &[real.clone(), newer.clone()]).unwrap();

    assert_eq!(
        (report.superseded, report.imported, report.unchanged),
        (vec![model(&poisoned)], vec![model(&real)], vec![model(&newer)])
    );
    for registry in [&registry as &Registry, &rig.open("1.0.0")] {
        assert_eq!(listed(registry, &model(&poisoned)).status, ModelStatus::Revoked);
        assert_eq!(listed(registry, &model(&real)).status, ModelStatus::NotInstalled);
        assert_eq!(listed(registry, &model(&newer)).status, ModelStatus::NotInstalled);
    }
}

#[test]
fn a_replacement_with_the_same_files_but_a_newer_app_requirement_disables_what_was_installed() {
    let rig = Rig::new();
    let registry = rig.installed();
    let shipped = shipped_ref(&registry);
    let old = entry("1.1.0", "a");
    registry.sync_catalogue(1, 1, std::slice::from_ref(&old)).unwrap();
    registry.install(&model(&old), &NoProgress).unwrap();
    registry.select(&echo_slot(), Some(&model(&old)), Requester::Person).unwrap();
    let replacement = entry_of(&manifest_text(ID, "test_echo@1", "1.1.0", "a", "9.0.0"));
    assert_eq!(model(&replacement), model(&old), "same files, so the same model reference");

    let report = registry.sync_catalogue(2, 2, std::slice::from_ref(&replacement)).unwrap();

    assert_eq!((report.superseded, report.imported), (vec![model(&old)], vec![model(&replacement)]));
    for registry in [&registry as &Registry, &rig.open("1.0.0")] {
        let ModelStatus::Disabled { reason } = listed(registry, &model(&replacement)).status else { panic!() };
        assert!(reason.contains("needs Omuzaki 9.0.0"), "{reason}");
        assert_eq!((selected(registry), active(registry)), (None, Some(shipped.clone())));
        assert!(matches!(
            registry.select(&echo_slot(), Some(&model(&old)), Requester::Person),
            Err(RegistryError::NotUsable { .. })
        ));
    }
}

#[test]
fn a_hidden_replacement_still_quarantines_the_import_it_would_have_replaced() {
    let rig = Rig::new();
    let registry = rig.installed();
    let poisoned = entry("1.1.0", "a");
    registry.sync_catalogue(1, 1, std::slice::from_ref(&poisoned)).unwrap();
    let hidden = entry_of(&manifest_text(ID, "no_such_slot@1", "1.1.0", "b", "0.1.0"));

    let report = registry.sync_catalogue(2, 2, &[hidden]).unwrap();

    assert_eq!((report.superseded, report.hidden), (vec![model(&poisoned)], vec![ID.parse().unwrap()]));
    for registry in [&registry as &Registry, &rig.open("1.0.0")] {
        assert_eq!(listed(registry, &model(&poisoned)).status, ModelStatus::Revoked);
    }
}

#[test]
fn a_reimport_after_a_revocation_does_not_keep_running_the_old_installation() {
    let rig = Rig::new();
    let registry = rig.installed();
    let shipped = shipped_ref(&registry);
    let old = entry("1.1.0", "a");
    registry.sync_catalogue(1, 1, std::slice::from_ref(&old)).unwrap();
    registry.install(&model(&old), &NoProgress).unwrap();
    registry.sync_catalogue(2, 2, &[]).unwrap();
    assert_eq!(listed(&registry, &model(&old)).status, ModelStatus::Revoked);
    let again = entry_of(&manifest_text(ID, "test_echo@1", "1.1.0", "a", "9.0.0"));
    assert_eq!(model(&again), model(&old));

    registry.sync_catalogue(3, 3, std::slice::from_ref(&again)).unwrap();

    for registry in [&registry as &Registry, &rig.open("1.0.0")] {
        assert!(matches!(listed(registry, &model(&again)).status, ModelStatus::Disabled { .. }));
        assert_eq!(active(registry), Some(shipped.clone()));
        assert!(matches!(
            registry.select(&echo_slot(), Some(&model(&again)), Requester::Person),
            Err(RegistryError::NotUsable { .. })
        ));
    }
}

#[test]
fn hidden_entries_take_part_in_conflict_detection() {
    let rig = Rig::new();
    let registry = rig.installed();
    let first = entry("1.1.0", "a");
    registry.sync_catalogue(1, 1, std::slice::from_ref(&first)).unwrap();
    let (state, models) = (rig.state_text(), registry.models(None));
    let hidden = entry_of(&manifest_text(ID, "no_such_slot@1", "1.1.0", "b", "0.1.0"));

    // Against an import of the same epoch, and within one catalogue.
    let against_import = registry.sync_catalogue(1, 2, std::slice::from_ref(&hidden));
    assert!(matches!(against_import, Err(ImportError::Conflict { .. })), "{against_import:?}");
    let supported = entry("1.2.0", "a");
    let hidden_twin = entry_of(&manifest_text(ID, "no_such_slot@1", "1.2.0", "b", "0.1.0"));
    let within = registry.sync_catalogue(1, 3, &[supported, hidden_twin]);
    assert!(matches!(within, Err(ImportError::Conflict { .. })), "{within:?}");

    assert_eq!(rig.state_text(), state);
    assert_eq!(registry.models(None), models);
    drop(registry);
    assert_eq!(rig.open("1.0.0").models(None), models);
}
