//! The Models page of the Settings window (#688, spec ML-13) over a registry this test fakes: every slot shows what
//! runs; every model its origin, licence (an unknown one flagged), status and smoke test, and the impersonation warning
//! where its card names a real person; a person accepts the "not from Omuzaki" notice, switches to a model and back
//! to the shipped one, removes an added model only after confirming, and adds one by dropping its file on the slot,
//! which runs `model.add` as a job that copies the file. With no registry the page says why, and every word goes
//! through the message table.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_ai_api::{
    AddModel, Better, Check, EvalLevel, EvalReport, Licence, Metric, ModelEntry, ModelId, ModelRef, ModelRegistry,
    ModelStatus, Origin, Progress, RegistryError, RuntimeId, Session, Sha256, SlotInfo, SlotMode, SlotRef, SlotStatus,
    Stream, TestSet, TestSetKind, Version,
};
use omuzaki_ops_api::{Actor, ServicesBuilder};
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{all_strings, harness_state};
use omuzaki_ui::ds::{Settings, set_strings};
use omuzaki_ui::settings::{Page, SettingsWindow};
use omuzaki_ui::wiring::Core;

/// What the fake registry was asked to do, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Select(String, Option<String>),
    Acknowledge(String, String),
    Remove(String),
    /// The slot and the copied files' contents, main file first.
    Add(String, Vec<Vec<u8>>),
}

#[derive(Default)]
struct State {
    slots: Vec<SlotInfo>,
    models: Vec<ModelEntry>,
    selected: Vec<(SlotRef, ModelRef)>,
    calls: Vec<Call>,
}

/// A registry that holds its slots and models in memory and records what it is asked. An add waits for the test's
/// go-ahead when it holds a gate, so the page can be seen while the job runs.
#[derive(Default)]
struct FakeRegistry(Mutex<State>, Mutex<Option<std::sync::mpsc::Receiver<()>>>);

impl FakeRegistry {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn calls(&self) -> Vec<Call> {
        self.state().calls.clone()
    }
}

fn shipped_of(state: &State, slot: &SlotRef) -> Option<ModelRef> {
    state.models.iter().find(|m| &m.slot == slot && m.origin == Origin::Shipped).map(|m| m.model.clone())
}

impl ModelRegistry for FakeRegistry {
    fn slots(&self) -> Vec<SlotStatus> {
        let state = self.state();
        state
            .slots
            .iter()
            .map(|info| {
                let selected = state.selected.iter().find(|(s, _)| s == &info.slot).map(|(_, m)| m.clone());
                let active = selected.clone().or_else(|| shipped_of(&state, &info.slot));
                SlotStatus { info: info.clone(), selected, active, fallback: None }
            })
            .collect()
    }

    fn models(&self, slot: Option<&SlotRef>) -> Vec<ModelEntry> {
        let slots = self.slots();
        let state = self.state();
        state
            .models
            .iter()
            .filter(|m| slot.is_none_or(|s| &m.slot == s))
            .map(|m| {
                let active = slots.iter().any(|s| s.info.slot == m.slot && s.active.as_ref() == Some(&m.model));
                ModelEntry { active, ..m.clone() }
            })
            .collect()
    }

    fn open(&self, slot: &SlotRef, _: Option<&ModelRef>) -> Result<Box<dyn Session>, RegistryError> {
        Err(RegistryError::WrongMode(slot.clone()))
    }

    fn open_stream(&self, slot: &SlotRef, _: Option<&ModelRef>) -> Result<Stream, RegistryError> {
        Err(RegistryError::WrongMode(slot.clone()))
    }

    fn install(&self, model: &ModelRef, _: &dyn Progress) -> Result<(), RegistryError> {
        Err(RegistryError::UnknownModel(model.to_string()))
    }

    fn rollback(&self, slot: &SlotRef) -> Result<ModelRef, RegistryError> {
        Err(RegistryError::WrongMode(slot.clone()))
    }

    fn add(&self, request: AddModel, _: &dyn Progress) -> Result<ModelEntry, RegistryError> {
        if let Some(gate) = self.1.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = gate.recv();
        }
        let contents = request.files.iter().map(|f| std::fs::read(f).unwrap_or_default()).collect();
        let name = request.files[0].file_stem().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let entry = ModelEntry {
            status: ModelStatus::NeedsAcknowledgement,
            licence: Licence::default(),
            ..entry(&format!("user-{name}"), &name, &request.slot.to_string(), Origin::User, 9)
        };
        let mut state = self.state();
        state.calls.push(Call::Add(request.slot.to_string(), contents));
        state.models.push(entry.clone());
        Ok(entry)
    }

    fn acknowledge(&self, model: &ModelRef, person: &str) -> Result<(), RegistryError> {
        let mut state = self.state();
        state.calls.push(Call::Acknowledge(model.id.to_string(), person.to_owned()));
        for m in state.models.iter_mut().filter(|m| &m.model == model) {
            m.status = ModelStatus::Ready;
        }
        Ok(())
    }

    fn select(
        &self,
        slot: &SlotRef,
        model: Option<&ModelRef>,
        _: omuzaki_ai_api::Requester,
    ) -> Result<(), RegistryError> {
        let mut state = self.state();
        state.calls.push(Call::Select(slot.to_string(), model.map(|m| m.id.to_string())));
        state.selected.retain(|(s, _)| s != slot);
        if let Some(model) = model {
            state.selected.push((slot.clone(), model.clone()));
        }
        Ok(())
    }

    fn remove(&self, model: &ModelRef) -> Result<(), RegistryError> {
        let mut state = self.state();
        state.calls.push(Call::Remove(model.id.to_string()));
        state.models.retain(|m| &m.model != model);
        state.selected.retain(|(_, m)| m != model);
        Ok(())
    }
}

fn slot_ref(text: &str) -> SlotRef {
    text.parse().unwrap_or_else(|e| panic!("{e}"))
}

fn info(slot: &str, summary: &str, without: &str) -> SlotInfo {
    SlotInfo {
        slot: slot_ref(slot),
        summary: summary.into(),
        without_model: without.into(),
        mode: SlotMode::Call { stall_ms: 200 },
        threads: 1,
        likeness: None,
    }
}

fn smoke(name: &str, value: f64, floor: f64) -> EvalReport {
    let set = TestSet { id: "fixture-smoke-1".into(), kind: TestSetKind::Synthetic };
    let metric = Metric { name: name.into(), value, better: Better::Lower, set };
    EvalReport { level: EvalLevel::Smoke, checks: vec![Check { metric, floor: Some(floor) }] }
}

fn entry(id: &str, name: &str, slot: &str, origin: Origin, digest: u8) -> ModelEntry {
    ModelEntry {
        model: ModelRef {
            id: ModelId::new(id).unwrap_or_else(|e| panic!("{e}")),
            version: Version::new(1, 0, 0),
            digest: Sha256([digest; 32]),
        },
        name: name.into(),
        slot: slot_ref(slot),
        runtime: RuntimeId::new("onnx").unwrap_or_else(|e| panic!("{e}")),
        origin,
        licence: Licence { spdx: Some("MIT".into()), text: None, url: None },
        size: 148_000_000,
        status: ModelStatus::Ready,
        active: false,
        eval: Some(smoke("wer", 0.08, 0.15)),
        imitates: None,
    }
}

/// Two slots: speech to text with its shipped model and one the person added and hasn't accepted yet (no licence
/// declared); text to speech with its shipped voice, a voice whose card says it imitates a real person, and one whose
/// smoke test failed.
fn registry() -> Arc<FakeRegistry> {
    let registry = FakeRegistry::default();
    {
        let mut state = registry.state();
        state.slots = vec![
            info("asr@1", "Speech to text: turns your voice into live captions.", "live captions are off"),
            info("tts@1", "Text to speech: reads chat messages aloud.", "messages aren't read aloud"),
        ];
        state.models = vec![
            entry("whisper-base-en", "Whisper base (English)", "asr@1", Origin::Shipped, 1),
            ModelEntry {
                status: ModelStatus::NeedsAcknowledgement,
                licence: Licence::default(),
                ..entry("user-captions-small", "Captions small", "asr@1", Origin::User, 2)
            },
            ModelEntry { size: 61_000_000, ..entry("studio-voice", "Studio voice", "tts@1", Origin::Shipped, 3) },
            ModelEntry {
                status: ModelStatus::NeedsConsent,
                imitates: Some("a narrator".into()),
                ..entry("user-narrator", "Narrator voice", "tts@1", Origin::User, 4)
            },
            ModelEntry {
                status: ModelStatus::Disabled { reason: "its smoke test failed".into() },
                eval: Some(smoke("wer", 0.41, 0.15)),
                ..entry("user-rough-voice", "Rough voice", "tts@1", Origin::User, 5)
            },
        ];
    }
    Arc::new(registry)
}

struct Rig {
    window: Option<SettingsWindow>,
    core: Core,
    person: Actor,
}

fn rig(registry: Option<Arc<FakeRegistry>>, settings: Settings) -> Harness<'static, Rig> {
    omuzaki_ui::link();
    let mut services = ServicesBuilder::new();
    if let Some(registry) = registry {
        services = services.insert::<dyn ModelRegistry>(registry);
    }
    let core = omuzaki_ui::wiring::core_on(omuzaki_ui::wiring::Document::empty(), services.build())
        .unwrap_or_else(|e| panic!("{e}"));
    let state =
        Rig { window: Some(SettingsWindow::on(Page::Models)), core, person: Actor::Person { name: "you".into() } };
    let mut h = harness_state(settings, vec2(900.0, 1600.0), state, |ui, r| {
        let runner = Arc::clone(&r.core.runner);
        if let Some(window) = r.window.as_mut()
            && !window.show(ui.ctx(), &runner, &r.person)
        {
            r.window = None;
        }
    });
    h.run_steps(4);
    h
}

fn shows(h: &Harness<'_, Rig>, text: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(text))
}

fn click(h: &mut Harness<'_, Rig>, label: &str) {
    h.get_by_role_and_label(Role::Button, label).click();
    h.run_steps(3);
}

#[test]
fn every_slot_shows_what_runs_and_every_model_where_it_stands() {
    let h = rig(Some(registry()), Settings::default());
    for text in [
        "Speech to text: turns your voice into live captions.",
        "Runs Whisper base (English)",
        "Text to speech: reads chat messages aloud.",
        "Runs Studio voice",
        "Ships with Omuzaki",
        "Added by you",
        "Licence: MIT",
        "Licence unknown",
        "148 MB",
        "61 MB",
        "In use",
        "Waiting for you to accept its notice",
        "Needs the consent of the person it imitates",
        "Disabled: its smoke test failed",
        "Smoke test passed",
        "Smoke test failed: wer",
        "Not from Omuzaki",
        omuzaki_ai_api::DISCLAIMER,
        omuzaki_ai::IMPERSONATION_WARNING,
    ] {
        assert!(shows(&h, text), "the page shows {text:?}: {:#?}", all_strings(&h));
    }
    // Only a ready model that isn't running can be switched to; only an added model can be removed. The shipped
    // models are in use, and the added ones wait for a notice, a consent or a fix.
    assert!(h.query_by_role_and_label(Role::Button, "Use this model").is_none());
    let removes = h.query_all_by_role_and_label(Role::Button, "Remove").count();
    assert_eq!(removes, 3, "one Remove per added model");
    assert_eq!(h.query_all_by_role_and_label(Role::Button, "Accept the notice").count(), 1);
}

#[test]
fn a_person_accepts_the_notice_switches_to_the_model_and_back() {
    let registry = registry();
    let mut h = rig(Some(Arc::clone(&registry)), Settings::default());
    click(&mut h, "Accept the notice");
    assert!(!shows(&h, "Waiting for you to accept its notice"), "accepted: the model is ready");
    click(&mut h, "Use this model");
    assert!(shows(&h, "Runs Captions small"), "{:#?}", all_strings(&h));
    click(&mut h, "Back to the shipped model");
    assert!(shows(&h, "Runs Whisper base (English)"));
    assert_eq!(
        registry.calls(),
        vec![
            Call::Acknowledge("user-captions-small".into(), "you".into()),
            Call::Select("asr@1".into(), Some("user-captions-small".into())),
            Call::Select("asr@1".into(), None),
        ]
    );
}

#[test]
fn removing_an_added_model_asks_first() {
    let registry = registry();
    let mut h = rig(Some(Arc::clone(&registry)), Settings::default());
    h.get_all_by_role_and_label(Role::Button, "Remove").next().unwrap_or_else(|| panic!("a Remove")).click();
    h.run_steps(3);
    assert!(shows(&h, "Remove Captions small?"), "{:#?}", all_strings(&h));
    assert!(shows(&h, "your own file stays where you keep it"));
    click(&mut h, "Cancel");
    assert!(!shows(&h, "Remove Captions small?"));
    assert!(registry.calls().is_empty(), "nothing is removed before the person confirms");

    h.get_all_by_role_and_label(Role::Button, "Remove").next().unwrap_or_else(|| panic!("a Remove")).click();
    h.run_steps(3);
    // The confirmation's Remove is the card's only button now, before the other cards' Removes.
    h.get_all_by_role_and_label(Role::Button, "Remove").next().unwrap_or_else(|| panic!("the confirm")).click();
    h.run_steps(3);
    assert_eq!(registry.calls(), vec![Call::Remove("user-captions-small".into())]);
    assert!(!shows(&h, "Captions small"), "the removed model's card is gone");
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

#[test]
fn a_dropped_file_is_added_to_the_slot_it_is_dropped_on() {
    let registry = registry();
    let (go, gate) = std::sync::mpsc::channel();
    *registry.1.lock().unwrap_or_else(PoisonError::into_inner) = Some(gate);
    let mut h = rig(Some(Arc::clone(&registry)), Settings::default());
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    // The real path: a root reached through a symlink covers nothing (ADR 0013).
    let folder_path = folder.path().canonicalize().unwrap_or_else(|e| panic!("{e}"));
    let file = folder_path.join("my-captions.onnx");
    std::fs::write(&file, b"not really a model").unwrap_or_else(|e| panic!("{e}"));

    let zones: Vec<_> = h.get_all_by_label("Drop a model file here to add it to this slot").collect();
    assert_eq!(zones.len(), 2, "one drop target per slot");
    zones[1].scroll_to_me();
    drop(zones);
    h.run_steps(3);
    let zones: Vec<_> = h.get_all_by_label("Drop a model file here to add it to this slot").collect();
    zones[1].hover();
    drop(zones);
    h.run_steps(1);
    h.input_mut().dropped_files.push(Arc::new(Dropped(file)));
    h.run_steps(3);
    // The job runs on a thread of its own, held at the gate: the page goes on drawing, with the add's row.
    assert!(shows(&h, "Adding my-captions.onnx: copying, checking and smoke-testing"), "{:#?}", all_strings(&h));
    go.send(()).unwrap_or_else(|e| panic!("{e}"));
    let window = h.state_mut().window.as_mut().unwrap_or_else(|| panic!("the window is open"));
    assert!(window.models().adding(), "the drop started model.add");
    window.models().finish_adding();
    h.run_steps(3);
    assert_eq!(registry.calls(), vec![Call::Add("tts@1".into(), vec![b"not really a model".to_vec()])]);
    assert!(!shows(&h, "Adding my-captions.onnx"));
    assert!(shows(&h, "my-captions"), "the added model has its card: {:#?}", all_strings(&h));
    assert_eq!(h.query_all_by_role_and_label(Role::Button, "Accept the notice").count(), 2);
}

#[test]
fn without_a_registry_the_page_says_why() {
    let h = rig(None, Settings::default());
    assert!(shows(&h, "The models can't be listed"), "{:#?}", all_strings(&h));
}

#[test]
fn every_word_on_the_page_goes_through_the_message_table() {
    let mut h = rig(Some(registry()), Settings::default());
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(4);
    for text in ["Ships with Omuzaki", "Licence unknown", "In use", "Not from Omuzaki", "Accept the notice"] {
        assert!(!shows(&h, text), "{text:?} is drawn in the pseudo-locale: {:#?}", all_strings(&h));
    }
}

/// Screenshots of the page for the pull request: dark and light at 100%, dark at 200%, and the pseudo-locale. Run with
/// `OMUZAKI_EVIDENCE_OUT=<dir> cargo test -p omuzaki-ui --test models_page models_evidence -- --ignored` (software
/// Vulkan).
#[test]
#[ignore = "writes screenshots; run by hand for a pull request"]
fn models_evidence() {
    use omuzaki_ui::ds::{Scale, ThemeKind, apply_settings};
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let shots = [
        ("dark", Settings::default(), false),
        ("light", Settings { theme: ThemeKind::Light, ..Settings::default() }, false),
        ("dark-200", Settings { scale: Scale::nearest(200), ..Settings::default() }, false),
        ("pseudo", Settings::default(), true),
        ("dark-voices", Settings::default(), false),
    ];
    for (name, settings, pseudo) in shots {
        omuzaki_ui::link();
        let services = ServicesBuilder::new().insert::<dyn ModelRegistry>(registry()).build();
        let core = omuzaki_ui::wiring::core_on(omuzaki_ui::wiring::Document::empty(), services)
            .unwrap_or_else(|e| panic!("{e}"));
        let person = Actor::Person { name: "you".into() };
        let mut window = SettingsWindow::on(Page::Models);
        let mut h = Harness::builder().with_size(vec2(900.0, 900.0)).wgpu().build_ui(move |ui| {
            apply_settings(ui.ctx(), settings);
            egui::CentralPanel::default().show(ui, |_| {});
            window.show(ui.ctx(), &core.runner, &person);
        });
        if pseudo {
            set_strings(&h.ctx, Arc::new(Pseudo(English)));
        }
        h.run_steps(8);
        if name == "dark-voices" {
            // The second slot: a voice that imitates a real person and one whose smoke test failed.
            h.get_by_label("Rough voice").scroll_to_me();
            h.run_steps(4);
        }
        let image = h.render().unwrap_or_else(|e| panic!("renders: {e}"));
        let path = format!("{out}/models-{name}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    }
}
