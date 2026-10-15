//! Models a person adds, against the mock's `fake` runtime and its test slots: the checks `add` runs and what each
//! refusal says, the acknowledgement flow, the requester rule in `select`, and `remove` freeing the cache. Offline.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::path::Path;

use common::rig::{PORTS_X_Y, Rig, add, echo, manifest, reason, stream};
use omuzaki_ai::Registry;
use omuzaki_ai_api::{
    AddModel, DISCLAIMER_VERSION, ManifestError, ModelEntry, ModelManifest, ModelRef, ModelRegistry, ModelStatus,
    NoProgress, RegistryError, Requester, SlotRef, TensorBuf, TensorRef,
};

/// An edit that makes a good manifest bad.
type Edit = Box<dyn FnOnce(&mut ModelManifest)>;

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
    // Stored as the cache stores a download: the blob and its record.
    assert_eq!(rig.blobs(), [pin.sha256.to_string(), format!("{}.rec", pin.sha256)]);
    assert_eq!(entry.size, pin.size);
    assert_eq!(rig.registry().models(None)[0].model, entry.model, "it is listed again after a reload");
}

/// Adds a good model for `test_echo@1` without an acknowledgement, as an agent would.
fn add_good(rig: &Rig, registry: &Registry, name: &str) -> ModelEntry {
    add_fake(rig, registry, name, PORTS_X_Y, None)
}

/// Adds a `fake` identity model for `test_echo@1` with `ports`, with its manifest, accepted by `by` if given.
fn add_fake(rig: &Rig, registry: &Registry, name: &str, ports: &str, by: Option<&str>) -> ModelEntry {
    let file = rig.fake(name, "1.0", ports);
    let given = manifest(&echo(), &[&file], "");
    add(registry, &echo(), vec![file], Some(given), by)
}

fn status_of(registry: &Registry, model: &ModelRef) -> ModelStatus {
    registry.models(None).into_iter().find(|e| &e.model == model).unwrap().status
}

#[test]
fn an_agent_adds_and_asks_and_only_the_person_acknowledges() {
    let rig = Rig::new();
    let registry = rig.registry();
    let entry = add_good(&rig, &registry, "mine.fake.toml");
    assert_eq!(entry.status, ModelStatus::NeedsAcknowledgement);

    let opened = registry.open(&echo(), Some(&entry.model)).map(|_| ()).unwrap_err();
    assert!(
        matches!(&opened, RegistryError::NotUsable { reason, .. } if reason.contains("licence notice")),
        "{opened}"
    );

    let refused = registry.select(&echo(), Some(&entry.model), Requester::Agent).unwrap_err();
    assert_eq!(refused, RegistryError::NeedsPerson(entry.model.clone()));
    // A person must acknowledge first, too: selecting never accepts the notice on their behalf.
    let by_person = registry.select(&echo(), Some(&entry.model), Requester::Person).unwrap_err();
    assert!(matches!(&by_person, RegistryError::NotUsable { reason, .. } if reason.contains("licence notice")));
    assert_eq!(registry.slots().iter().find(|s| s.info.slot == echo()).unwrap().selected, None, "nothing switched");

    registry.acknowledge(&entry.model, "Pat").unwrap();
    assert_eq!(status_of(&registry, &entry.model), ModelStatus::Ready);
    registry.select(&echo(), Some(&entry.model), Requester::Agent).unwrap();
    assert_eq!(registry.slots().iter().find(|s| s.info.slot == echo()).unwrap().selected, Some(entry.model));
}

#[test]
fn a_person_who_accepts_while_adding_gets_a_ready_model_and_the_record_says_who_when_and_which_words() {
    let rig = Rig::new();
    let entry = add_fake(&rig, &rig.registry(), "mine.fake.toml", PORTS_X_Y, Some("Pat"));
    assert_eq!(entry.status, ModelStatus::Ready);
    let state = rig.state();
    for expected in [
        format!("digest = \"{}\"", entry.model.digest),
        "person = \"Pat\"".to_owned(),
        format!("disclaimer_version = {DISCLAIMER_VERSION}"),
        "at = 1700000000".to_owned(),
    ] {
        assert!(state.contains(&expected), "{expected:?} not in\n{state}");
    }
    assert_eq!(status_of(&rig.registry(), &entry.model), ModelStatus::Ready, "the acknowledgement survives a reload");
}

#[test]
fn an_acknowledgement_is_per_hash_so_a_changed_file_asks_again() {
    let rig = Rig::new();
    let registry = rig.registry();
    let first = add_fake(&rig, &registry, "mine.fake.toml", PORTS_X_Y, Some("Pat"));
    assert_eq!(first.status, ModelStatus::Ready);
    // The same name and manifest, other bytes: another model, which the person hasn't accepted.
    let second = add_fake(&rig, &registry, "mine.fake.toml", &format!("# edited\n{PORTS_X_Y}"), None);
    assert_ne!(second.model.digest, first.model.digest);
    assert_eq!(second.status, ModelStatus::NeedsAcknowledgement);
    assert_eq!(status_of(&registry, &first.model), ModelStatus::Ready);
}

#[test]
fn acknowledging_twice_keeps_the_first_record_and_unknown_models_are_refused() {
    let rig = Rig::new();
    let registry = rig.registry();
    let entry = add_good(&rig, &registry, "mine.fake.toml");
    registry.acknowledge(&entry.model, "Pat").unwrap();
    registry.acknowledge(&entry.model, "Sam").unwrap();
    assert!(rig.state().contains("Pat") && !rig.state().contains("Sam"), "{}", rig.state());
    let mut ghost = entry.model.clone();
    ghost.digest = omuzaki_ai_api::Sha256([7; 32]);
    assert!(matches!(registry.acknowledge(&ghost, "Pat"), Err(RegistryError::UnknownModel(_))));
}

#[test]
fn an_acknowledged_user_model_runs_when_selected_and_remove_really_removes() {
    let rig = Rig::new();
    let registry = rig.registry();
    // scale 1.0 is the identity, so its outputs are its inputs.
    let entry = add_fake(&rig, &registry, "mine.fake.toml", PORTS_X_Y, Some("Pat"));
    registry.select(&echo(), Some(&entry.model), Requester::Person).unwrap();
    let mut session = registry.open(&echo(), None).unwrap();
    assert_eq!(session.info().model, entry.model);
    assert!(session.info().sandboxed, "a user model runs in its worker");
    let mut out = TensorBuf::for_outputs(session.signature());
    let x = [0.25f32, -1.0, 3.0];
    session.run(&[TensorRef::f32(&x, &[3])], &mut out).unwrap();
    assert_eq!(out[0].as_f32().unwrap(), x);
    drop(session);

    registry.remove(&entry.model).unwrap();
    assert!(registry.models(None).is_empty(), "it is forgotten");
    assert_eq!(registry.slots().iter().find(|s| s.info.slot == echo()).unwrap().selected, None, "the slot let go");
    assert!(rig.blobs().is_empty(), "its files are gone: {:?}", rig.blobs());
    assert!(!rig.state().contains("Pat"), "its acknowledgement went with it");
    assert!(matches!(registry.remove(&entry.model), Err(RegistryError::UnknownModel(_))));
    assert!(matches!(registry.open(&echo(), None), Err(RegistryError::NoModel { .. })));
}

#[test]
fn removing_one_model_keeps_the_files_another_still_uses() {
    let rig = Rig::new();
    let registry = rig.registry();
    let file = rig.fake("mine.fake.toml", "1.0", PORTS_X_Y);
    let given = add(&registry, &echo(), vec![file.clone()], Some(manifest(&echo(), &[&file], "")), None);
    // The written manifest names a runtime this app lacks, so it is listed disabled; it still shares the bytes.
    let written = add(&registry, &echo(), vec![file.clone()], None, None);
    assert_ne!(written.model, given.model, "two manifests over the same bytes");
    registry.remove(&written.model).unwrap();
    assert_eq!(rig.blobs().len(), 2, "blob and record stay for the other model: {:?}", rig.blobs());
    registry.remove(&given.model).unwrap();
    assert_eq!(rig.blobs(), Vec::<String>::new());
}
