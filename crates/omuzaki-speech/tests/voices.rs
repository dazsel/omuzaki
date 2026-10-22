//! Voices (#1310, `ai-performer-voice-face.md` §3.4, §3.5, §7 "Voices"): the catalog lists the `tts@1` models'
//! voices; `voice.assign` writes only `voice-assignment` and leaves the effect presets of `voice.toml` alone (and the
//! other way round), round-trips, and undoes; an unknown id, an out-of-range speed or pitch, a cloned voice without a
//! consent record, and a revoked record at start are refused; a public figure's name warns; a hosted voice needs
//! `spend` and previews its cost per hour; `voice.preview` plays to `monitor_out` and refuses a graph that would carry
//! it to the stream.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_ai_api::{
    AddModel, ConsentLookup, Licence, Likeness, ModelEntry, ModelId, ModelManifest, ModelRef, ModelRegistry,
    ModelStatus, Origin, Progress, RegistryError, RuntimeId, Session, Sha256, SlotRef, SlotStatus, Stream, Version,
};
use omuzaki_ai_mock::FakeTts;
use omuzaki_audio_api::FxPreset;
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, SendSpec, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId};
use omuzaki_audio_api::tap::{DropPolicy, TapFormat, TapPoint, TapRate, TapReader, TapRequest};
use omuzaki_audio_mock::MockEngine;
use omuzaki_char_api::CharacterDef;
use omuzaki_char_api::ids::CharId;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Grants, Op, OpCtx, OpError, Plan, Scope, Services, ServicesBuilder};
use omuzaki_performer_api::ids::{ConnectorId, VoiceId};
use omuzaki_performer_api::speech::VoiceError;
use omuzaki_performer_api::voice::{AssignError, TtsPrice, VoiceAssignment, VoiceCatalog, VoiceInfo, VoiceProvider};
use omuzaki_speech::slots::tts_slot;
use omuzaki_speech::voices::ops::{VoiceAssign, VoiceGet, VoiceList, VoicePreviewOp};
use omuzaki_speech::voices::{
    ASSIGNMENT_KEY, Catalog, LocalVoices, PREVIEW_FEED, TtsPreview, VoicePreview, admit, assignment_path, open_host,
    read_assignment,
};
use omuzaki_store_api::Consent;
use serde_json::{Value, json};

/// A text-to-speech model with a stock voice in each English, one whose card imitates a real person and one cloned
/// from the streamer's recording.
const FAKE_TTS: &str = r#"
id = "fake-tts"
version = "1.0.0"
name = "Fake TTS"
slot = "tts@1"
runtime = "onnx"
providers = ["cpu"]

[[files]]
name = "fake.onnx"
sha256 = "abababababababababababababababababababababababababababababababab"
size = 1000

[tts]
honours = ["speed"]

[[voices]]
id = "plain"
languages = ["en-US"]

[[voices]]
id = "deep"
languages = ["en-GB"]

[[voices]]
id = "star"
languages = ["en-US"]
imitates = "A Famous Person"

[[voices]]
id = "me"
languages = ["en-US"]
imitates = "The Streamer"
cloned = true

[card.licence]
spdx = "Apache-2.0"
"#;

/// A registry holding one `tts@1` model, opening [`FakeTts`] sessions.
struct FakeRegistry {
    entry: ModelEntry,
    manifest: ModelManifest,
}

impl FakeRegistry {
    fn new(status: ModelStatus) -> Self {
        let manifest = ModelManifest::from_toml(FAKE_TTS).unwrap();
        let entry = ModelEntry {
            model: ModelRef {
                id: ModelId::new("fake-tts").unwrap(),
                version: Version::new(1, 0, 0),
                digest: Sha256([7; 32]),
            },
            name: "Fake TTS".into(),
            slot: tts_slot(),
            runtime: RuntimeId::new("onnx").unwrap(),
            origin: Origin::Shipped,
            licence: Licence { spdx: Some("Apache-2.0".into()), text: None, url: None },
            size: 1000,
            status,
            active: true,
            eval: None,
            imitates: None,
        };
        Self { entry, manifest }
    }
}

impl ModelRegistry for FakeRegistry {
    fn slots(&self) -> Vec<SlotStatus> {
        Vec::new()
    }
    fn models(&self, slot: Option<&SlotRef>) -> Vec<ModelEntry> {
        if slot.is_none_or(|s| *s == tts_slot()) { vec![self.entry.clone()] } else { Vec::new() }
    }
    fn manifest(&self, model: &ModelRef) -> Option<ModelManifest> {
        (*model == self.entry.model).then(|| self.manifest.clone())
    }
    fn open(&self, slot: &SlotRef, model: Option<&ModelRef>) -> Result<Box<dyn Session>, RegistryError> {
        assert_eq!((slot, model), (&tts_slot(), Some(&self.entry.model)));
        Ok(Box::new(FakeTts::new().with_voices(&["plain", "deep", "star", "me"])))
    }
    fn open_stream(&self, slot: &SlotRef, _: Option<&ModelRef>) -> Result<Stream, RegistryError> {
        Err(RegistryError::WrongMode(slot.clone()))
    }
    fn install(&self, model: &ModelRef, _: &dyn Progress) -> Result<(), RegistryError> {
        Err(RegistryError::UnknownModel(model.to_string()))
    }
    fn rollback(&self, slot: &SlotRef) -> Result<ModelRef, RegistryError> {
        Err(RegistryError::NoRollback(slot.clone()))
    }
    fn add(&self, _: AddModel, _: &dyn Progress) -> Result<ModelEntry, RegistryError> {
        Err(RegistryError::Cancelled)
    }
    fn acknowledge(&self, model: &ModelRef, _: &str) -> Result<(), RegistryError> {
        Err(RegistryError::UnknownModel(model.to_string()))
    }
    fn select(&self, slot: &SlotRef, _: Option<&ModelRef>, _: omuzaki_ai_api::Requester) -> Result<(), RegistryError> {
        Err(RegistryError::UnknownSlot(slot.clone()))
    }
    fn remove(&self, model: &ModelRef) -> Result<(), RegistryError> {
        Err(RegistryError::UnknownModel(model.to_string()))
    }
}

/// Consent records the test adds and revokes.
#[derive(Default)]
struct Records(Mutex<BTreeSet<String>>);

impl Records {
    fn with(people: &[&str]) -> Arc<Self> {
        let records = Self::default();
        records.0.lock().unwrap().extend(people.iter().map(|p| (*p).to_owned()));
        Arc::new(records)
    }

    fn revoke(&self, person: &str) {
        self.0.lock().unwrap().remove(person);
    }
}

impl ConsentLookup for Records {
    fn find(&self, person: &str, likeness: Likeness) -> Option<Consent> {
        (likeness == Likeness::VoiceClone && self.0.lock().unwrap().contains(person)).then(|| Consent {
            person: person.to_owned(),
            scope: likeness.scope().to_owned(),
            document: None,
        })
    }
}

/// A hosted provider at $15 per million characters.
struct Hosted;

impl VoiceCatalog for Hosted {
    fn voices(&self, _: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError> {
        Ok(vec![
            VoiceInfo::new(VoiceId::new("tts-cloud/Rachel").unwrap(), vec![]),
            // Another source's id: the catalog drops it.
            VoiceInfo::new(VoiceId::new("elsewhere/Rachel").unwrap(), vec![]),
        ])
    }
    fn price(&self, _: &VoiceProvider) -> Option<TtsPrice> {
        Some(TtsPrice::new(15_000_000, "2026-10-01"))
    }
}

fn cloud() -> ConnectorId {
    ConnectorId::new("tts-cloud").unwrap()
}

fn mochi() -> CharId {
    CharId::new("mochi").unwrap()
}

fn vid(s: &str) -> VoiceId {
    VoiceId::new(s).unwrap()
}

fn registry() -> Arc<FakeRegistry> {
    Arc::new(FakeRegistry::new(ModelStatus::Ready))
}

fn catalog(registry: &Arc<FakeRegistry>) -> Arc<dyn VoiceCatalog> {
    Arc::new(Catalog::new(Arc::clone(registry) as Arc<dyn ModelRegistry>).with_hosted(cloud(), Arc::new(Hosted)))
}

fn services(registry: &Arc<FakeRegistry>, consent: Option<Arc<Records>>) -> Services {
    let mut builder = ServicesBuilder::new()
        .insert::<dyn ModelRegistry>(Arc::clone(registry) as Arc<dyn ModelRegistry>)
        .insert::<dyn VoiceCatalog>(catalog(registry));
    if let Some(consent) = consent {
        builder = builder.insert::<dyn ConsentLookup>(consent as Arc<dyn ConsentLookup>);
    }
    builder.build()
}

/// A project with Mochi, who already has an effect preset in `voice.toml`.
fn doc() -> MemDoc {
    MemDoc::from_value(json!({
        "characters": {
            "mochi": {
                "name": "Mochi",
                "template": "chibi",
                "voice": { "presets": [{ "name": "Radio", "chain": [] }] },
            }
        }
    }))
}

fn person() -> Actor {
    Actor::Person { name: "Pat".into() }
}

fn plan<O: Op>(op: &O, doc: &MemDoc, services: &Services, grants: &Grants) -> Result<Plan, OpError> {
    let (clock, actor) = (FakeClock::new(Seconds(0.0)), person());
    let ctx = OpCtx::new(doc, &actor, &clock).with_services(services).with_grants(grants);
    op.plan(&ctx)
}

fn editor() -> Grants {
    Grants::of([Scope::Read, Scope::EditCharacter])
}

fn assign(voice: &str) -> VoiceAssign {
    VoiceAssign {
        character: mochi(),
        voice: voice.into(),
        provider: None,
        speed: None,
        pitch_semitones: None,
        consent: None,
    }
}

#[test]
fn the_catalog_lists_every_tts_models_voices_with_who_they_sound_like() {
    let registry = registry();
    let local = LocalVoices::new(Arc::clone(&registry) as Arc<dyn ModelRegistry>);
    let voices = local.voices(&VoiceProvider::Local).unwrap();
    let ids: Vec<&str> = voices.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, ["fake-tts/deep", "fake-tts/me", "fake-tts/plain", "fake-tts/star"]);
    let needs: Vec<bool> = voices.iter().map(VoiceInfo::needs_consent).collect();
    assert_eq!(needs, [false, true, false, true]);
    assert_eq!(voices[0].languages[0].as_str(), "en-GB");
    assert!(matches!(local.voices(&VoiceProvider::Hosted(cloud())), Err(VoiceError::Provider(_))));
    let hosted = catalog(&registry).voices(&VoiceProvider::Hosted(cloud())).unwrap();
    assert_eq!(hosted.len(), 1, "a connector lists only its own voices");
}

#[test]
fn assign_writes_only_its_own_file_round_trips_and_undoes() {
    let registry = registry();
    let services = services(&registry, None);
    let mut doc = doc();
    let presets = doc.get(&DocPath::root().join("characters").join("mochi").join("voice")).cloned().unwrap();

    let op = VoiceAssign { speed: Some(1.1), pitch_semitones: Some(-2.0), ..assign("fake-tts/plain") };
    let planned = plan(&op, &doc, &services, &editor()).unwrap();
    let paths: Vec<String> = planned.patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert_eq!(paths, [format!("characters/mochi/{ASSIGNMENT_KEY}")], "one change, in its own file");
    doc.apply(&planned.patch).unwrap();

    let voice = DocPath::root().join("characters").join("mochi").join("voice");
    assert_eq!(doc.get(&voice), Some(&presets), "voice.toml is untouched");
    let stored = read_assignment(&doc, &mochi()).unwrap().unwrap();
    assert_eq!(stored, VoiceAssignment::new(mochi(), vid("fake-tts/plain"), VoiceProvider::Local).tuned(1.1, -2.0));
    // Both files are valid on their own: the assignment as TOML, the presets as `FxPreset`s, and the character still
    // reads with both siblings next to it.
    let text = toml::to_string(&stored).unwrap();
    assert_eq!(toml::from_str::<VoiceAssignment>(&text).unwrap(), stored);
    let preset: FxPreset = serde_json::from_value(presets["presets"][0].clone()).unwrap();
    assert_eq!(preset.name, "Radio");
    let character = doc.get(&omuzaki_char_api::paths::character(&mochi())).unwrap();
    assert_eq!(CharacterDef::from_node(character).unwrap().name, "Mochi");

    // An effects save (what `audio.fx` writes) leaves the assignment alone.
    let fx = Patch::new().with(voice.clone(), Some(presets.clone()), Some(json!({ "presets": [] })));
    doc.apply(&fx).unwrap();
    assert_eq!(read_assignment(&doc, &mochi()).unwrap(), Some(stored.clone()));

    // A second assign keeps speed and pitch it isn't given; undo puts the first back.
    let second = plan(&assign("fake-tts/deep"), &doc, &services, &editor()).unwrap();
    assert_eq!(second.output.as_ref().unwrap()["previous"], json!("fake-tts/plain"));
    doc.apply(&second.patch).unwrap();
    let now = read_assignment(&doc, &mochi()).unwrap().unwrap();
    assert_eq!((now.voice.as_str(), now.speed, now.pitch_semitones), ("fake-tts/deep", 1.1, -2.0));
    doc.apply(&second.patch.inverse()).unwrap();
    assert_eq!(read_assignment(&doc, &mochi()).unwrap(), Some(stored));
    doc.apply(&planned.patch.inverse()).unwrap();
    assert_eq!(doc.get(&assignment_path(&mochi())), None, "undo leaves no file behind");

    let got = plan(&VoiceGet { character: mochi() }, &doc, &services, &editor()).unwrap();
    assert_eq!(got.output.unwrap()["assignment"], Value::Null);
}

fn code(e: &OpError) -> ErrorCode {
    e.code
}

#[test]
fn bad_assignments_are_refused_with_what_to_change() {
    let registry = registry();
    let services = services(&registry, None);
    let doc = doc();

    let unknown = plan(&assign("fake-tts/plian"), &doc, &services, &editor()).unwrap_err();
    assert_eq!(code(&unknown), ErrorCode::NotFound);
    assert!(unknown.hint.as_deref().unwrap().contains("fake-tts/plain"), "{unknown:?}");

    let fast = plan(&VoiceAssign { speed: Some(1.5), ..assign("fake-tts/plain") }, &doc, &services, &editor());
    assert_eq!(fast.unwrap_err().field.as_deref(), Some("speed"));
    let high =
        plan(&VoiceAssign { pitch_semitones: Some(4.5), ..assign("fake-tts/plain") }, &doc, &services, &editor());
    assert_eq!(high.unwrap_err().field.as_deref(), Some("pitch_semitones"));
    let malformed = plan(&assign("no slash"), &doc, &services, &editor()).unwrap_err();
    assert_eq!(malformed.field.as_deref(), Some("voice"));
    let nobody = plan(
        &VoiceAssign { character: CharId::new("nobody").unwrap(), ..assign("fake-tts/plain") },
        &doc,
        &services,
        &editor(),
    );
    assert_eq!(code(&nobody.unwrap_err()), ErrorCode::NotFound);
}

#[test]
fn no_consent_records_keep_every_cloned_or_imitating_voice_unusable() {
    let registry = registry();
    let doc = doc();
    // No lookup wired: the ops fall back to `NoConsentRecords`.
    let services = services(&registry, None);
    for voice in ["fake-tts/me", "fake-tts/star"] {
        let refused = plan(&assign(voice), &doc, &services, &editor()).unwrap_err();
        assert_eq!(code(&refused), ErrorCode::Denied, "{voice}: {refused:?}");
        let refused =
            plan(&VoiceAssign { consent: Some("The Streamer".into()), ..assign(voice) }, &doc, &services, &editor());
        assert!(refused.is_err(), "{voice}: naming a record that doesn't exist changes nothing");
    }
    assert!(plan(&assign("fake-tts/plain"), &doc, &services, &editor()).is_ok(), "stock voices need none");

    // With the streamer's record, their cloned voice is admitted and names the record; a record of someone else's
    // doesn't fit it.
    let services = services_with(&registry, Records::with(&["The Streamer"]));
    let planned = plan(&assign("fake-tts/me"), &doc, &services, &editor()).unwrap();
    assert_eq!(planned.output.unwrap()["assignment"]["consent"], json!({ "person": "The Streamer" }));
    let wrong = plan(
        &VoiceAssign { consent: Some("Someone Else".into()), ..assign("fake-tts/me") },
        &doc,
        &services,
        &editor(),
    );
    assert_eq!(wrong.unwrap_err().field.as_deref(), Some("consent"));
    assert_eq!(code(&plan(&assign("fake-tts/star"), &doc, &services, &editor()).unwrap_err()), ErrorCode::Denied);
}

fn services_with(registry: &Arc<FakeRegistry>, records: Arc<Records>) -> Services {
    services(registry, Some(records))
}

#[test]
fn a_revoked_record_stops_the_next_start_and_the_session_open() {
    let registry = registry();
    let records = Records::with(&["The Streamer"]);
    let catalog = catalog(&registry);
    let assignment = VoiceAssignment::new(mochi(), vid("fake-tts/me"), VoiceProvider::Local)
        .with_consent(omuzaki_performer_api::voice::ConsentRef::new("The Streamer"));
    assert!(
        admit(&assignment, catalog.as_ref(), records.as_ref()).is_ok(),
        "admitted at start while the record exists"
    );
    let engine: Arc<dyn AudioEngine> = Arc::new(MockEngine::new());
    assert!(
        open_host(registry.as_ref(), Arc::clone(&engine), &assignment, catalog.as_ref(), records.as_ref(), None)
            .is_ok()
    );

    records.revoke("The Streamer");
    let refused = admit(&assignment, catalog.as_ref(), records.as_ref()).unwrap_err();
    assert_eq!(refused, AssignError::Voice(VoiceError::no_consent(&vid("fake-tts/me"))));
    let refused = open_host(registry.as_ref(), engine, &assignment, catalog.as_ref(), records.as_ref(), None);
    assert!(matches!(refused, Err(AssignError::Voice(VoiceError::NoConsent { .. }))));

    // A voice whose model isn't installed is refused at open with the way out, never swapped for another voice.
    let missing = Arc::new(FakeRegistry::new(ModelStatus::NotInstalled));
    let plain = VoiceAssignment::new(mochi(), vid("fake-tts/plain"), VoiceProvider::Local);
    let engine: Arc<dyn AudioEngine> = Arc::new(MockEngine::new());
    let Err(AssignError::Voice(VoiceError::Provider(why))) =
        open_host(missing.as_ref(), engine, &plain, catalog.as_ref(), records.as_ref(), None)
    else {
        panic!("a missing model must refuse the open")
    };
    assert!(why.contains("install"), "{why}");
}

#[test]
fn a_public_figures_name_warns_and_never_blocks() {
    let registry = registry();
    let services = services(&registry, None);
    let mut doc = doc();
    let plain = plan(&assign("fake-tts/plain"), &doc, &services, &editor()).unwrap();
    assert_eq!(plain.output.unwrap()["warnings"], json!([]));

    let name = DocPath::root().join("characters").join("mochi").join("name");
    doc.apply(&Patch::new().with(name, Some(json!("Mochi")), Some(json!("Morgan Freeman")))).unwrap();
    let warned = plan(&assign("fake-tts/plain"), &doc, &services, &editor()).unwrap();
    let warnings = warned.output.unwrap()["warnings"].clone();
    assert!(warnings[0].as_str().unwrap().contains("Morgan Freeman"), "{warnings}");
    assert!(!warned.patch.changes.is_empty(), "a warning, not a block");
}

#[test]
fn a_hosted_voice_needs_spend_and_shows_its_cost_per_hour() {
    let registry = registry();
    let services = services(&registry, None);
    let doc = doc();
    let hosted = VoiceAssign { provider: Some("tts-cloud".into()), ..assign("tts-cloud/Rachel") };
    let denied = plan(&hosted, &doc, &services, &editor()).unwrap_err();
    assert_eq!(code(&denied), ErrorCode::Denied);
    assert!(denied.message.contains("spend"), "{denied:?}");

    let spender = Grants::of([Scope::Read, Scope::EditCharacter, Scope::Spend]);
    let planned = plan(&hosted, &doc, &services, &spender).unwrap();
    // $15 per million characters × 21,600 characters an hour at a 40 % talk share = $0.324.
    assert_eq!(planned.cost.as_ref().unwrap().usd, Some(0.324));
    let output = planned.output.unwrap();
    assert_eq!(output["cost"], json!("≈ $0.32 per hour at a 40 % talk share (price of 2026-10-01)"));
    assert_eq!(output["assignment"]["provider"], json!({ "hosted": "tts-cloud" }));
}

#[test]
fn list_shows_each_voice_its_languages_consent_and_install_state() {
    let registry = Arc::new(FakeRegistry::new(ModelStatus::NotInstalled));
    let services = ServicesBuilder::new().insert::<dyn ModelRegistry>(registry as Arc<dyn ModelRegistry>).build();
    let doc = doc();
    let listed = plan(&VoiceList::default(), &doc, &services, &editor()).unwrap().output.unwrap();
    let voices = listed["voices"].as_array().unwrap();
    assert_eq!(voices.len(), 4);
    let me = voices.iter().find(|v| v["id"] == "fake-tts/me").unwrap();
    assert_eq!((me["needs_consent"].clone(), me["consent_found"].clone()), (json!(true), json!(false)));
    assert_eq!(me["installed"], json!(false));
    assert_eq!(me["model"], json!("Fake TTS"));

    let british = VoiceList { language: Some("en-GB".into()), ..VoiceList::default() };
    let listed = plan(&british, &doc, &services, &editor()).unwrap().output.unwrap();
    assert_eq!(listed["voices"].as_array().unwrap().len(), 1);
    let english = VoiceList { language: Some("en".into()), ..VoiceList::default() };
    assert_eq!(
        plan(&english, &doc, &services, &editor()).unwrap().output.unwrap()["voices"].as_array().unwrap().len(),
        4
    );
}

/// A graph where the preview feed's bus `preview` outputs to `out`, optionally also sending to a `program` bus.
fn preview_graph(out: &str, to_program: bool) -> GraphSpec {
    let bus = |id: &str, sources: Vec<SourceRef>, sends: Vec<SendSpec>, outputs: &[&str]| BusSpec {
        id: BusId::new(id).unwrap(),
        channels: Channels::Mono,
        sources,
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends,
        outputs: outputs.iter().map(|o| DeviceRole::new(*o).unwrap()).collect(),
    };
    let feed = SourceRef::Feed { id: FeedId::new(PREVIEW_FEED).unwrap() };
    let sends = if to_program {
        vec![SendSpec { to: BusId::new("program").unwrap(), gain_db: 0.0, pre_fader: false }]
    } else {
        vec![]
    };
    GraphSpec {
        buses: vec![bus("preview", vec![feed], sends, &[out]), bus("program", vec![], vec![], &["program_out"])],
        ..GraphSpec::default()
    }
}

fn tap(engine: &MockEngine, bus: &str) -> TapReader {
    let request = TapRequest {
        point: TapPoint::BusPostFader(BusId::new(bus).unwrap()),
        format: TapFormat { rate: TapRate::Graph48k, mono: true },
        capacity: Seconds(30.0),
        policy: DropPolicy::DropNewest,
        consumer: "test".into(),
        leaves_device: false,
    };
    engine.tap_ring(&request).unwrap().0
}

#[test]
fn preview_plays_to_the_monitor_only() {
    let registry = registry();
    let engine = Arc::new(MockEngine::new());
    engine.apply(&preview_graph("monitor_out", false)).unwrap();
    let (mut monitor, mut program) = (tap(&engine, "preview"), tap(&engine, "program"));
    let preview = Arc::new(TtsPreview::new(
        Arc::clone(&registry) as Arc<dyn ModelRegistry>,
        Arc::clone(&engine) as Arc<dyn AudioEngine>,
    ));
    let services = ServicesBuilder::new()
        .insert::<dyn ModelRegistry>(Arc::clone(&registry) as Arc<dyn ModelRegistry>)
        .insert::<dyn VoicePreview>(Arc::clone(&preview) as Arc<dyn VoicePreview>)
        .build();
    let op = VoicePreviewOp {
        voice: Some("fake-tts/plain".into()),
        text: Some("Hi chat!".into()),
        ..VoicePreviewOp::default()
    };
    let out = plan(&op, &doc(), &services, &editor()).unwrap().output.unwrap();
    assert_eq!((out["played"].clone(), out["to"].clone()), (json!(true), json!("monitor_out")));

    // "Hi chat!" sounds in its 6 letters, 1,440 samples each at 24 kHz: 17,280 samples at the engine's 48 kHz, less the
    // tone's zero crossings. Play until it is heard (the engine advances on this thread, the preview on its own).
    let (mut heard, mut on_program) = (Vec::new(), 0usize);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut buf = [0.0f32; 240];
    while heard.iter().filter(|s: &&f32| s.abs() > 1e-4).count() < 16_000 && Instant::now() < deadline {
        engine.advance(240);
        let n = monitor.read(&mut buf).len;
        heard.extend_from_slice(&buf[..n]);
        let n = program.read(&mut buf).len;
        on_program += buf[..n].iter().filter(|s| s.abs() > 0.0).count();
        std::thread::yield_now();
    }
    let audible = heard.iter().filter(|s| s.abs() > 1e-4).count();
    assert!((16_000..=17_280).contains(&audible), "heard {audible} samples");
    assert_eq!(on_program, 0, "never the program bus");
    // The preview's thread ends once the engine has played what it wrote.
    for _ in 0..400 {
        engine.advance(240);
    }
    preview.wait();

    // A cloned voice without its record is refused before anything plays.
    let cloned = VoicePreviewOp { voice: Some("fake-tts/me".into()), ..VoicePreviewOp::default() };
    assert_eq!(code(&plan(&cloned, &doc(), &services, &editor()).unwrap_err()), ErrorCode::Denied);
}

#[test]
fn preview_refuses_a_graph_that_would_reach_the_stream() {
    let registry = registry();
    let assignment = VoiceAssignment::new(mochi(), vid("fake-tts/plain"), VoiceProvider::Local);
    for (graph, why) in [
        (preview_graph("monitor_out", true), "program_out"),
        (preview_graph("program_out", false), "program_out"),
        (GraphSpec::default(), "nothing plays"),
    ] {
        let engine = Arc::new(MockEngine::new());
        engine.apply(&graph).unwrap();
        let preview = TtsPreview::new(Arc::clone(&registry) as Arc<dyn ModelRegistry>, engine as Arc<dyn AudioEngine>);
        let Err(VoiceError::Provider(message)) = preview.preview(&assignment, "Hello there.") else {
            panic!("the preview must refuse {graph:?}")
        };
        assert!(message.contains(why), "{message}");
    }
}
