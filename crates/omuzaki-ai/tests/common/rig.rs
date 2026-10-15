//! A person's data folder and picked files, a registry over them, and `fake` models and manifests: shared by the
//! user-model, consent and sandbox tests.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{AddModel, ModelEntry, ModelManifest, ModelRegistry, ModelStatus, NoProgress, SlotRef};
use omuzaki_ai_mock::MemFetcher;
use omuzaki_update_api::clock::{Timestamp, WallClock};

pub const PORTS_X_Y: &str = "[[inputs]]\nname = \"x\"\ndtype = \"f32\"\nshape = [\"any\"]\n\n\
                         [[outputs]]\nname = \"y\"\ndtype = \"f32\"\nshape = [\"any\"]\n";

pub fn echo() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

pub fn stream() -> SlotRef {
    "test_stream@1".parse().unwrap()
}

/// A person's data folder, and a folder holding the files they pick.
pub struct Rig {
    pub data: tempfile::TempDir,
    pub picked: tempfile::TempDir,
}

impl Default for Rig {
    fn default() -> Self {
        Self::new()
    }
}

impl Rig {
    pub fn new() -> Self {
        Self { data: tempfile::tempdir().unwrap(), picked: tempfile::tempdir().unwrap() }
    }

    pub fn registry(&self) -> Registry {
        let files = Arc::new(VerifiedCache::new(self.data.path().join(CACHE_DIR), MemFetcher::new()));
        let app = "1.0.0".parse().unwrap();
        let registry =
            Registry::with_models(self.data.path(), files, app, RegistrySettings::default(), Vec::new()).unwrap();
        registry.set_clock(Arc::new(At(1_700_000_000)));
        registry
    }

    /// The state file, as text.
    pub fn state(&self) -> String {
        std::fs::read_to_string(self.data.path().join("models").join("state.toml")).unwrap()
    }

    /// A file the person picked.
    pub fn pick(&self, name: &str, text: &str) -> PathBuf {
        let path = self.picked.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    /// A `fake` model: `scale`, the ports (`PORTS_X_Y` fits `test_echo@1`).
    pub fn fake(&self, name: &str, scale: &str, ports: &str) -> PathBuf {
        self.pick(name, &format!("scale = {scale}\nbias = 0.0\n\n{ports}"))
    }

    pub fn blobs(&self) -> Vec<String> {
        super::cache_files(&self.data.path().join(CACHE_DIR))
    }
}

/// A calendar clock stopped at one time.
pub struct At(pub i64);

impl WallClock for At {
    fn now(&self) -> Timestamp {
        Timestamp(self.0)
    }
}

/// A manifest for `files` (pinned to what is on disk), with `extra` TOML after the keys and before the tables.
pub fn manifest(slot: &SlotRef, files: &[&Path], extra: &str) -> ModelManifest {
    let mut entries = String::new();
    for path in files {
        let pin = super::pin(&[], &std::fs::read(path).unwrap());
        let name = path.file_name().unwrap().to_string_lossy();
        writeln!(entries, "[[files]]\nname = \"{name}\"\nsha256 = \"{}\"\nsize = {}\n", pin.sha256, pin.size).unwrap();
    }
    let text = format!(
        "id = \"user-mine\"\nversion = \"1.0.0\"\nname = \"Mine\"\nslot = \"{slot}\"\nruntime = \"fake\"\n{extra}\n\
         {entries}[card.licence]\nspdx = \"MIT\"\n"
    );
    ModelManifest::from_toml(&text).unwrap()
}

pub fn add(
    registry: &Registry,
    slot: &SlotRef,
    files: Vec<PathBuf>,
    given: Option<ModelManifest>,
    by: Option<&str>,
) -> ModelEntry {
    let request = AddModel { slot: slot.clone(), files, manifest: given, acknowledged_by: by.map(str::to_owned) };
    registry.add(request, &NoProgress).unwrap()
}

pub fn reason(entry: &ModelEntry) -> String {
    match &entry.status {
        ModelStatus::Disabled { reason } => reason.clone(),
        other => panic!("expected Disabled, got {other:?}"),
    }
}
