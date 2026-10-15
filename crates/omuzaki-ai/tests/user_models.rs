//! Models a person adds, against the mock's `fake` runtime and its test slots: the checks `add` runs and what each
//! refusal says, the acknowledgement flow, the requester rule in `select`, and `remove` freeing the cache. Offline.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{
    AddModel, ManifestError, ModelEntry, ModelManifest, ModelRegistry, ModelStatus, NoProgress, RegistryError, SlotRef,
};
use omuzaki_ai_mock::MemFetcher;

const PORTS_X_Y: &str = "[[inputs]]\nname = \"x\"\ndtype = \"f32\"\nshape = [\"any\"]\n\n\
                         [[outputs]]\nname = \"y\"\ndtype = \"f32\"\nshape = [\"any\"]\n";

/// An edit that makes a good manifest bad.
type Edit = Box<dyn FnOnce(&mut ModelManifest)>;

fn echo() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

fn stream() -> SlotRef {
    "test_stream@1".parse().unwrap()
}

/// A person's data folder, and a folder holding the files they pick.
struct Rig {
    data: tempfile::TempDir,
    picked: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self { data: tempfile::tempdir().unwrap(), picked: tempfile::tempdir().unwrap() }
    }

    fn registry(&self) -> Registry {
        let files = Arc::new(VerifiedCache::new(self.data.path().join(CACHE_DIR), MemFetcher::new()));
        let app = "1.0.0".parse().unwrap();
        Registry::with_models(self.data.path(), files, app, RegistrySettings::default(), Vec::new()).unwrap()
    }

    /// A file the person picked.
    fn pick(&self, name: &str, text: &str) -> PathBuf {
        let path = self.picked.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    /// A `fake` model: `scale`, the ports (`PORTS_X_Y` fits `test_echo@1`).
    fn fake(&self, name: &str, scale: &str, ports: &str) -> PathBuf {
        self.pick(name, &format!("scale = {scale}\nbias = 0.0\n\n{ports}"))
    }

    fn blobs(&self) -> Vec<String> {
        common::cache_files(&self.data.path().join(CACHE_DIR))
    }
}

/// A manifest for `files` (pinned to what is on disk), with `extra` TOML after the keys and before the tables.
fn manifest(slot: &SlotRef, files: &[&Path], extra: &str) -> ModelManifest {
    let mut entries = String::new();
    for path in files {
        let pin = common::pin(&[], &std::fs::read(path).unwrap());
        let name = path.file_name().unwrap().to_string_lossy();
        writeln!(entries, "[[files]]\nname = \"{name}\"\nsha256 = \"{}\"\nsize = {}\n", pin.sha256, pin.size).unwrap();
    }
    let text = format!(
        "id = \"user-mine\"\nversion = \"1.0.0\"\nname = \"Mine\"\nslot = \"{slot}\"\nruntime = \"fake\"\n{extra}\n\
         {entries}[card.licence]\nspdx = \"MIT\"\n"
    );
    ModelManifest::from_toml(&text).unwrap()
}

fn add(
    registry: &Registry,
    slot: &SlotRef,
    files: Vec<PathBuf>,
    given: Option<ModelManifest>,
    by: Option<&str>,
) -> ModelEntry {
    let request = AddModel { slot: slot.clone(), files, manifest: given, acknowledged_by: by.map(str::to_owned) };
    registry.add(request, &NoProgress).unwrap()
}

fn reason(entry: &ModelEntry) -> String {
    match &entry.status {
        ModelStatus::Disabled { reason } => reason.clone(),
        other => panic!("expected Disabled, got {other:?}"),
    }
}

/// Adds `file` with a manifest made by `edit` and returns why it was refused.
fn refused(rig: &Rig, slot: &SlotRef, file: &Path, edit: impl FnOnce(&mut ModelManifest)) -> String {
    let mut given = manifest(slot, &[file], "");
    edit(&mut given);
    reason(&add(&rig.registry(), slot, vec![file.to_owned()], Some(given), None))
}

#[test]
fn a_manifest_the_user_path_cannot_accept_lists_each_error() {
    let rig = Rig::new();
    let file = rig.fake("m.fake.toml", "1.0", PORTS_X_Y);
    let cases: Vec<(&str, Edit)> = vec![
        ("starting with user-", Box::new(|m| m.id = "mine".parse().unwrap())),
        ("never downloads", Box::new(|m| m.files[0].urls = vec!["https://x.invalid/m".into()])),
        ("stage name \"a b\"", Box::new(|m| m.stages = vec![stage("a b", "m.fake.toml")])),
        ("which isn't one of the model's files", Box::new(|m| m.stages = vec![stage("s", "nope.bin")])),
        ("only earlier stages", Box::new(|m| m.stages = vec![stage_reading("s", "m.fake.toml", "later.y")])),
        (
            "doesn't name a stage",
            Box::new(|m| {
                m.stages = vec![stage("s", "m.fake.toml")];
                m.io.insert("y".into(), "ghost.y".into());
            }),
        ),
        ("stream settings are out of range", Box::new(|m| m.stream = Some(spec(1, 1)))),
        (
            "maps \"y\" to an empty name",
            Box::new(|m| {
                m.io.insert("y".into(), String::new());
            }),
        ),
        ("two files are called", Box::new(|m| m.files.push(m.files[0].clone()))),
    ];
    for (expected, edit) in cases {
        let why = refused(&rig, &echo(), &file, edit);
        assert!(why.contains(expected), "{expected:?} not in {why:?}");
    }
    let empty = rig.pick("empty.fake.toml", "");
    assert!(refused(&rig, &echo(), &empty, |_| {}).contains("has size 0"));
    let spaced = rig.fake("a b.fake.toml", "1.0", PORTS_X_Y);
    assert!(refused(&rig, &echo(), &spaced, |_| {}).contains("file name \"a b.fake.toml\" isn't allowed"));
}

fn stage(name: &str, file: &str) -> omuzaki_ai_api::Stage {
    omuzaki_ai_api::Stage { name: name.into(), file: file.into(), inputs: std::collections::BTreeMap::default() }
}

fn stage_reading(name: &str, file: &str, source: &str) -> omuzaki_ai_api::Stage {
    omuzaki_ai_api::Stage { inputs: [("x".to_owned(), source.to_owned())].into(), ..stage(name, file) }
}

fn spec(sample_rate: u32, block: u32) -> omuzaki_ai_api::StreamSpec {
    omuzaki_ai_api::StreamSpec { sample_rate, block, latency: 0 }
}

#[test]
fn no_files_is_an_error_not_a_listed_model() {
    let rig = Rig::new();
    let request = AddModel { slot: echo(), files: vec![], manifest: None, acknowledged_by: None };
    let err = rig.registry().add(request, &NoProgress).unwrap_err();
    assert_eq!(err, RegistryError::Manifest(vec![ManifestError::NoFiles]));
}

#[test]
fn a_pin_that_does_not_match_the_file_is_refused() {
    let rig = Rig::new();
    let file = rig.fake("m.fake.toml", "1.0", PORTS_X_Y);
    let why = refused(&rig, &echo(), &file, |m| {
        m.files[0].size += 1;
        m.files.push(m.files[0].clone());
        m.files[1].name = "missing.bin".into();
    });
    assert!(why.contains("isn't the file the manifest pins"), "{why}");
    assert!(why.contains("\"missing.bin\", which wasn't among the files you added"), "{why}");
}

#[test]
fn a_model_for_a_newer_app_is_refused() {
    let rig = Rig::new();
    let file = rig.fake("m.fake.toml", "1.0", PORTS_X_Y);
    let why = refused(&rig, &echo(), &file, |m| m.min_app_version = "9.0.0".parse().unwrap());
    assert!(why.contains("needs Omuzaki 9.0.0 or newer (this is 1.0.0)"), "{why}");
}

#[test]
fn a_model_the_probe_refuses_says_why() {
    let rig = Rig::new();
    let file = rig.fake("m.fake.toml", "1.0", &format!("refuse = \"custom op domain com.example\"\n{PORTS_X_Y}"));
    let why = refused(&rig, &echo(), &file, |_| {});
    assert!(why.contains("refused: custom op domain com.example"), "{why}");
}

#[test]
fn every_interface_mismatch_is_listed_at_once() {
    let rig = Rig::new();
    let ports = "[[inputs]]\nname = \"q\"\ndtype = \"f32\"\nshape = [\"any\"]\n\n\
                 [[outputs]]\nname = \"z\"\ndtype = \"f32\"\nshape = [\"any\"]\n";
    let file = rig.fake("m.fake.toml", "1.0", ports);
    let why = refused(&rig, &echo(), &file, |_| {});
    for expected in ["no input `x`", "needs an input `q`", "no output `y`"] {
        assert!(why.contains(expected), "{expected:?} not in {why}");
    }
}

#[test]
fn renamed_ports_fit_through_the_io_table() {
    let rig = Rig::new();
    let ports = PORTS_X_Y.replace("\"x\"", "\"in0\"").replace("\"y\"", "\"out0\"");
    let file = rig.fake("m.fake.toml", "1.0", &ports);
    let given = manifest(&echo(), &[&file], "io = { x = \"in0\", y = \"out0\" }");
    let entry = add(&rig.registry(), &echo(), vec![file], Some(given), None);
    assert_eq!(entry.status, ModelStatus::NeedsAcknowledgement, "{entry:?}");
}

#[test]
fn a_stream_table_belongs_to_streaming_slots_only() {
    let rig = Rig::new();
    let file = rig.fake("m.fake.toml", "1.0", PORTS_X_Y);
    let on_call = refused(&rig, &echo(), &file, |m| m.stream = Some(spec(16_000, 160)));
    assert!(on_call.contains("must not have a [stream] table"), "{on_call}");
    let missing = refused(&rig, &stream(), &file, |_| {});
    assert!(missing.contains("needs a [stream] table"), "{missing}");
    let given = manifest(&stream(), &[&file], "stream = { sample_rate = 16000, block = 160 }");
    let ok = add(&rig.registry(), &stream(), vec![file], Some(given), None);
    assert_eq!(ok.status, ModelStatus::NeedsAcknowledgement, "{ok:?}");
}

#[test]
fn a_model_that_misses_the_smoke_floor_is_disabled_with_the_number() {
    let rig = Rig::new();
    let file = rig.fake("m.fake.toml", "2.0", PORTS_X_Y);
    let entry = add(&rig.registry(), &echo(), vec![file.clone()], Some(manifest(&echo(), &[&file], "")), None);
    let why = reason(&entry);
    assert!(why.contains("max_abs_error was") && why.contains("the floor is"), "{why}");
}

#[test]
fn a_model_with_no_manifest_gets_one_and_a_format_it_cannot_read_is_listed_disabled() {
    let rig = Rig::new();
    let registry = rig.registry();
    let file = rig.pick("My Voice.onnx", "not really onnx");
    let first = add(&registry, &echo(), vec![file.clone()], None, None);
    let second = add(&registry, &echo(), vec![rig.pick("my-voice.onnx", "different bytes")], None, None);
    assert_eq!(first.model.id.as_str(), "user-my-voice");
    assert_eq!(second.model.id.as_str(), "user-my-voice-2");
    assert_eq!((first.name.as_str(), first.model.version.to_string().as_str()), ("My Voice", "0.0.0"));
    assert!(first.licence.is_unknown());
    assert!(reason(&first).contains("no `onnx` runtime"), "{first:?}");
    let unknown = add(&registry, &echo(), vec![rig.pick("blob.bin", "???")], None, None);
    assert!(reason(&unknown).contains("`unknown` runtime"), "{unknown:?}");
}

#[test]
fn files_are_copied_into_the_cache_under_their_hash_so_the_originals_can_go() {
    let rig = Rig::new();
    let file = rig.fake("m.fake.toml", "1.0", PORTS_X_Y);
    let pin = common::pin(&[], &std::fs::read(&file).unwrap());
    let given = manifest(&echo(), &[&file], "");
    let entry = add(&rig.registry(), &echo(), vec![file.clone()], Some(given), None);
    std::fs::remove_file(&file).unwrap();
    assert_eq!(rig.blobs(), [pin.sha256.to_string()]);
    assert_eq!(entry.size, pin.size);
    assert_eq!(rig.registry().models(None)[0].model, entry.model, "it is listed again after a reload");
}
