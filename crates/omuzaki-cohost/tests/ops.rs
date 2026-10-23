//! The `performer.*` ops over a co-host built from fakes: start, say, intent, respond, status, transcript and stop as
//! a person and an agent call them; the persona read and written with the character; a hosted LLM needing a person;
//! and the local factory's checks and voice buses.
// Test helpers may unwrap: a panic there is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_anim_api::{Calibration, DeviceProfile, RetargetConfig};
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{AI_VOICE_BUS, BusSpec, Channels, GraphSpec, MIC_BUS, SourceRef};
use omuzaki_audio_api::ids::BusId;
use omuzaki_audio_mock::MockEngine;
use omuzaki_cohost::local::{AI_VOICE_PRIVATE_BUS, LocalFactory, LocalParts, ensure_voice_buses};
use omuzaki_cohost::service::{Booking, Built, CohostFactory, CohostService, StartError};
use omuzaki_cohost::settings::{DeciderChoice, LlmChoice, PerformerDecider, PerformerRealtime, RealtimeChoice};
use omuzaki_cohost::stage::PerformStage;
use omuzaki_cohost::{Cohost, attach_voice};
use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_doc_api::{DocWrite, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::ExpressionSet;
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_mock::{FakeLlm, Script, ScriptTurn};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpKind, Plan, Scope, Services, ServicesBuilder, Undo, find};
use omuzaki_performer::brain::LlmBrain;
use omuzaki_performer::show::{Parts, Show, ShowConfig};
use omuzaki_performer_api::ears::Heard;
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::stage::Stage;
use omuzaki_performer_mock::{FakeVoice, ScriptedEars};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{
    CalibrationState, PerformError, PerformHost, PerformStatus, RetargetChange, RetargetReceipt,
};
use omuzaki_stage::perform::triggers::{BindOutcome, TriggerBinding};
use omuzaki_track_voice::{LipSyncConfig, VoiceReader};
use serde_json::json;

/// Records binds, presses and whether a voice is attached.
#[derive(Default)]
struct Host {
    calls: Mutex<Vec<String>>,
    voice: Mutex<Option<VoiceReader>>,
}

impl Host {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl PerformHost for Host {
    fn calibrate(&self, _: usize) -> Result<(), PerformError> {
        Ok(())
    }

    fn set_retarget(&self, _: RetargetChange) -> Result<RetargetReceipt, PerformError> {
        Err(PerformError::Unsupported)
    }

    fn set_calibration(&self, _: Calibration) -> Result<(), PerformError> {
        Ok(())
    }

    fn calibration(&self) -> Calibration {
        Calibration::identity(DeviceProfile::Webcam)
    }

    fn status(&self) -> PerformStatus {
        PerformStatus {
            tracking: Tracking::Tracking,
            calibration: CalibrationState::Capturing { frames: 0, of: 1 },
            retarget: RetargetConfig::default(),
            retarget_seq: 0,
            source_fps: 30.0,
        }
    }

    fn set_voice(&self, voice: Option<VoiceReader>, _: LipSyncConfig) -> Result<(), PerformError> {
        *self.voice.lock().unwrap() = voice;
        Ok(())
    }

    fn bind_trigger_checked(&self, binding: TriggerBinding) -> Result<BindOutcome, PerformError> {
        if !self.expressions().0.contains_key(&binding.expression) {
            return Err(PerformError::UnknownExpression);
        }
        self.calls.lock().unwrap().push(format!("bind {} {}", binding.key, binding.expression));
        Ok(BindOutcome { slot: 0, replaced: false })
    }

    fn unbind_trigger(&self, key: &str) -> Result<bool, PerformError> {
        self.calls.lock().unwrap().push(format!("unbind {key}"));
        Ok(true)
    }

    fn press(&self, key: &str, down: bool) -> Result<bool, PerformError> {
        self.calls.lock().unwrap().push(format!("{} {key}", if down { "down" } else { "up" }));
        Ok(true)
    }

    fn expressions(&self) -> ExpressionSet {
        ExpressionSet::starter()
    }
}

fn reply(text: &str) -> ScriptTurn {
    ScriptTurn::new(vec![
        LlmEvent::start("fake"),
        LlmEvent::TextDelta(text.into()),
        LlmEvent::Stop(StopReason::EndTurn),
    ])
}

/// Builds a co-host from fakes on a clock the test moves; remembers what it was booked with.
struct Fakes {
    host: Arc<Host>,
    clock: Arc<FakeClock>,
    booked: Mutex<Vec<Booking>>,
    stopped: Arc<Mutex<u32>>,
    sends_to: Option<String>,
}

impl Fakes {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            host: Arc::new(Host::default()),
            clock: Arc::new(FakeClock::default()),
            booked: Mutex::default(),
            stopped: Arc::default(),
            sends_to: None,
        })
    }
}

impl CohostFactory for Fakes {
    fn sends_to(&self) -> Option<String> {
        self.sends_to.clone()
    }

    fn build(&self, booking: &Booking) -> Result<Built, StartError> {
        self.booked.lock().unwrap().push(booking.clone());
        let stage = Arc::new(PerformStage::new(Arc::clone(&self.host) as Arc<dyn PerformHost>, booking.member.clone()));
        let llm = FakeLlm::new(Script::new("fake").with_turn(reply("[emote happy 0.8 hold 1s] Hi chat, welcome in!")));
        let brain = LlmBrain::new(Arc::new(llm) as Arc<dyn LlmClient>, booking.persona.clone());
        let ears = vec![(Seconds(0.5), Heard::final_text(0, "Mochi, say hello", Seconds(0.0), Seconds(0.5)))];
        let parts = Parts::new(
            Box::new(brain),
            Box::new(ScriptedEars::new(Arc::clone(&self.clock), ears)),
            Box::new(FakeVoice::new(Arc::clone(&self.clock), Seconds(0.2))),
            Arc::clone(&stage) as Arc<dyn Stage>,
        );
        let config = ShowConfig::new(booking.member.clone(), VoiceId::new("kokoro-82m/af_heart").unwrap())
            .respond(booking.respond);
        let show = Show::new(&booking.persona, parts, config);
        let voice = attach_voice(self.host.as_ref(), LipSyncConfig::default()).map_err(StartError::failed)?;
        let stopped = Arc::clone(&self.stopped);
        Ok(Built::new(Cohost::new(show, stage, Some(voice)), Arc::clone(&self.clock) as Arc<dyn Clock>)
            .about(json!({ "llm": "fake" }))
            .on_stop(move || *stopped.lock().unwrap() += 1))
    }
}

fn person() -> Actor {
    Actor::Person { name: "host".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "agent-1".into(), model: None }
}

fn services(service: &Arc<CohostService>) -> Services {
    ServicesBuilder::new().insert::<CohostService>(Arc::clone(service)).build()
}

fn project() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mochi": { "name": "Mochi", "template": "soft-sd" } } }))
}

fn plan(
    name: &str,
    args: Value,
    doc: &MemDoc,
    services: &Services,
    actor: &Actor,
    dry_run: bool,
) -> Result<Plan, (ErrorCode, String)> {
    omuzaki_cohost::link();
    let op = (find(name).unwrap().decode)(args).map_err(|e| (e.code, e.message))?;
    let clock = FakeClock::new(Seconds(0.0));
    let ctx = OpCtx::new(doc, actor, &clock).with_services(services).with_dry_run(dry_run);
    op.plan(&ctx).map_err(|e| (e.code, format!("{} ({})", e.message, e.hint.unwrap_or_default())))
}

fn call(name: &str, args: Value, doc: &MemDoc, services: &Services) -> Result<Value, (ErrorCode, String)> {
    plan(name, args, doc, services, &person(), false).map(|p| p.output.unwrap_or(Value::Null))
}

/// Moves the fake clock in 20 ms steps until the transcript holds a line that `done` accepts, at most 10 s of
/// program time; the co-host thread steps on its own and reads the clock.
fn run_until(fakes: &Fakes, service: &Arc<CohostService>, done: impl Fn(&Value) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut t = fakes.clock.now().0;
    loop {
        let lines = service.transcript(0, 512)["lines"].as_array().cloned().unwrap_or_default();
        if lines.iter().any(&done) {
            return lines;
        }
        assert!(Instant::now() < deadline && t < 10.0, "the co-host never got there: {lines:#?}");
        t += 0.02;
        fakes.clock.set(Seconds(t));
        // Long enough for the co-host thread (it steps every 5 ms) to see each step.
        std::thread::sleep(Duration::from_millis(6));
    }
}

#[test]
fn every_performer_op_is_registered_with_honest_metadata() {
    omuzaki_cohost::link();
    let expect = [
        ("performer.start", OpKind::Edit, Scope::Perform, Undo::None),
        ("performer.stop", OpKind::Edit, Scope::Perform, Undo::None),
        ("performer.say", OpKind::Edit, Scope::Perform, Undo::None),
        ("performer.intent", OpKind::Edit, Scope::Perform, Undo::None),
        ("performer.respond", OpKind::Edit, Scope::Perform, Undo::None),
        ("performer.status", OpKind::Query, Scope::Perform, Undo::None),
        ("performer.transcript", OpKind::Query, Scope::Perform, Undo::None),
        ("performer.persona.get", OpKind::Query, Scope::Read, Undo::None),
        ("performer.persona.set", OpKind::Edit, Scope::EditCharacter, Undo::Auto),
    ];
    for (name, kind, scope, undo) in expect {
        let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
        let meta = (registration.meta)();
        assert_eq!((meta.kind, meta.scope, meta.undo), (kind, scope, undo), "{name}");
        let examples = registration.examples();
        assert!(!examples.is_empty(), "{name} has no examples");
        for example in examples {
            assert!((registration.decode)(example.clone()).is_ok(), "{name}: {example} does not decode");
        }
    }
}

#[test]
fn a_person_starts_steers_and_stops_the_cohost_through_ops() {
    let fakes = Fakes::new();
    let service = Arc::new(CohostService::new(Arc::clone(&fakes) as Arc<dyn CohostFactory>));
    let (doc, services) = (project(), services(&service));

    // A dry run says what would start, and starts nothing.
    let dry = plan("performer.start", json!({}), &doc, &services, &person(), true).unwrap().output.unwrap();
    assert_eq!((dry["applied"].as_bool(), dry["ready"].as_bool()), (Some(false), Some(true)));
    assert_eq!((dry["character"].as_str(), dry["persona"].as_str()), (Some("mochi"), Some("Mochi")));
    assert!(!service.is_running() && fakes.booked.lock().unwrap().is_empty());

    // Started: the project's only character, its default persona, answering when named.
    let started = call("performer.start", json!({}), &doc, &services).unwrap();
    assert_eq!(started["running"], true);
    assert_eq!((started["member"].as_str(), started["respond"].as_str()), (Some("mochi"), Some("addressed")));
    assert_eq!(started["runs_on"], json!({ "llm": "fake" }));
    assert!(fakes.host.voice.lock().unwrap().is_some(), "the co-host's voice has the mouth");

    // It hears "Mochi, say hello" and answers with a face.
    let lines = run_until(&fakes, &service, |l| {
        l["ev"] == "speaking" && l["text"].as_str().is_some_and(|t| t.contains("welcome in"))
    });
    assert!(lines.iter().any(|l| l["ev"] == "heard" && l["text"] == "Mochi, say hello"), "{lines:#?}");
    assert!(fakes.host.calls().contains(&"down ai:happy".to_owned()), "{:?}", fakes.host.calls());

    // A line and an expression on demand.
    let said = call("performer.say", json!({ "text": "[emote surprised] Oh, a raid!" }), &doc, &services).unwrap();
    assert_eq!(said["applied"], true);
    run_until(&fakes, &service, |l| l["ev"] == "speaking" && l["text"].as_str().is_some_and(|t| t.contains("a raid")));
    assert!(fakes.host.calls().contains(&"down ai:surprised".to_owned()), "{:?}", fakes.host.calls());
    let intent = json!({ "intent": { "emote": { "expression": "sad", "intensity": 0.5, "hold_s": 1.0 } } });
    assert_eq!(call("performer.intent", intent, &doc, &services).unwrap()["applied"], true);
    // The stage presses it on the co-host's next step.
    run_until(&fakes, &service, |_| fakes.host.calls().contains(&"down ai:sad".to_owned()));
    assert_eq!(
        call("performer.respond", json!({ "respond": "always" }), &doc, &services).unwrap()["respond"],
        "always"
    );

    // Status and the transcript, read in pages by sequence number.
    let status = call("performer.status", json!({}), &doc, &services).unwrap();
    assert_eq!((status["running"].as_bool(), status["respond"].as_str()), (Some(true), Some("always")));
    assert!(status["status"]["turns"].as_u64().unwrap() >= 2, "{status}");
    let first = call("performer.transcript", json!({ "limit": 2 }), &doc, &services).unwrap();
    let page: Vec<u64> = first["lines"].as_array().unwrap().iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert_eq!(page, [1, 2]);
    let rest = call("performer.transcript", json!({ "after": first["next"] }), &doc, &services).unwrap();
    assert_eq!(rest["lines"][0]["seq"], 3);

    // Stopped: the face is let go of, the parts are stopped once, and the transcript is still there to read.
    let stopped = call("performer.stop", json!({}), &doc, &services).unwrap();
    assert_eq!(stopped["was_running"], true);
    assert!(fakes.host.calls().iter().any(|c| c.starts_with("unbind ai:")), "{:?}", fakes.host.calls());
    assert_eq!(*fakes.stopped.lock().unwrap(), 1);
    assert_eq!(call("performer.status", json!({}), &doc, &services).unwrap()["running"], false);
    let kept = call("performer.transcript", json!({}), &doc, &services).unwrap();
    assert_ne!(kept["lines"], json!([]), "the transcript outlives the co-host");
    assert_eq!(call("performer.stop", json!({}), &doc, &services).unwrap()["was_running"], false);
}

#[test]
fn steering_a_cohost_that_is_not_running_says_how_to_start_it() {
    let fakes = Fakes::new();
    let service = Arc::new(CohostService::new(fakes));
    let (doc, services) = (project(), services(&service));
    for (name, args) in [
        ("performer.say", json!({ "text": "hi" })),
        ("performer.respond", json!({ "respond": "always" })),
        (
            "performer.intent",
            json!({ "intent": { "emote": { "expression": "happy", "intensity": 1.0, "hold_s": 1.0 } } }),
        ),
    ] {
        let (code, message) = call(name, args, &doc, &services).unwrap_err();
        assert_eq!(code, ErrorCode::Conflict, "{name}");
        assert!(message.contains("performer.start"), "{name}: {message}");
    }
    let (code, _) = call("performer.say", json!({ "text": "  " }), &doc, &services).unwrap_err();
    assert_eq!(code, ErrorCode::Invalid);
    let speak = json!({ "intent": { "speak": { "text": "hi", "tags": [] } } });
    let (code, message) = call("performer.intent", speak, &doc, &services).unwrap_err();
    assert!(code == ErrorCode::Invalid && message.contains("performer.say"), "{message}");
}

#[test]
fn the_persona_lives_with_the_character_and_undoes() {
    let fakes = Fakes::new();
    let service = Arc::new(CohostService::new(Arc::clone(&fakes) as Arc<dyn CohostFactory>));
    let (mut doc, services) = (project(), services(&service));

    let default = call("performer.persona.get", json!({ "character": "mochi" }), &doc, &services).unwrap();
    assert_eq!((default["stored"].as_bool(), default["persona"]["name"].as_str()), (Some(false), Some("Mochi")));

    let persona = json!({ "name": "Mochi-chan", "aliases": ["Mochie"], "personality": "Bubbly.", "style": "Short." });
    let set = plan(
        "performer.persona.set",
        json!({ "character": "mochi", "persona": persona }),
        &doc,
        &services,
        &person(),
        false,
    )
    .unwrap();
    assert_eq!(set.patch.changes.len(), 1);
    assert_eq!(set.patch.changes[0].path.segments(), ["characters", "mochi", "persona"]);
    doc.apply(&set.patch).unwrap();
    let stored = call("performer.persona.get", json!({ "character": "mochi" }), &doc, &services).unwrap();
    assert_eq!((stored["stored"].as_bool(), stored["persona"]["name"].as_str()), (Some(true), Some("Mochi-chan")));
    assert!(stored["toml"].as_str().unwrap().contains("name = \"Mochi-chan\""));
    // The character still reads as a character with its persona beside it.
    let node = doc.root()["characters"]["mochi"].clone();
    assert!(omuzaki_char_api::CharacterDef::from_node(&node).is_ok(), "{node}");

    // The co-host takes it at its next start; a persona passed to start wins for that run only.
    call("performer.start", json!({ "character": "mochi" }), &doc, &services).unwrap();
    call("performer.start", json!({ "persona": { "name": "Guest" } }), &doc, &services).unwrap();
    let names: Vec<String> = fakes.booked.lock().unwrap().iter().map(|b| b.persona.name.clone()).collect();
    assert_eq!(names, ["Mochi-chan", "Guest"]);
    call("performer.stop", json!({}), &doc, &services).unwrap();

    // Undo is the patch's inverse.
    doc.apply(&set.patch.inverse()).unwrap();
    assert!(doc.root()["characters"]["mochi"].get("persona").is_none());

    // Bad input names its field.
    let (code, message) =
        call("performer.persona.set", json!({ "character": "mochi", "persona": { "name": " " } }), &doc, &services)
            .unwrap_err();
    assert!(code == ErrorCode::Invalid && message.contains("name"), "{message}");
    let (code, message) = call("performer.persona.get", json!({ "character": "kiri" }), &doc, &services).unwrap_err();
    assert!(code == ErrorCode::NotFound && message.contains("mochi"), "{message}");
}

#[test]
fn a_cohost_that_sends_speech_off_this_machine_needs_a_person() {
    let fakes = Arc::new(Fakes {
        host: Arc::new(Host::default()),
        clock: Arc::new(FakeClock::default()),
        booked: Mutex::default(),
        stopped: Arc::default(),
        sends_to: Some("llm.example.com".into()),
    });
    let service = Arc::new(CohostService::new(Arc::clone(&fakes) as Arc<dyn CohostFactory>));
    let (doc, services) = (project(), services(&service));
    let (code, message) = plan("performer.start", json!({}), &doc, &services, &agent(), false).unwrap_err();
    assert!(code == ErrorCode::Denied && message.contains("llm.example.com"), "{message}");
    assert!(!service.is_running());
    // A person may start it.
    plan("performer.start", json!({}), &doc, &services, &person(), false).unwrap();
    assert!(service.is_running());
    service.stop();
}

#[test]
fn the_local_factory_says_what_is_missing_and_stays_local_by_default() {
    let host: Arc<dyn PerformHost> = Arc::new(Host::default());
    let factory = LocalFactory::new(LocalParts::new(host));
    assert_eq!(factory.sends_to(), None, "the default LLM is on this machine");
    let Err(StartError::NotReady { what, fix }) = factory.check() else { panic!("ready without an audio engine") };
    assert!(what.contains("audio engine") && fix.contains("Settings"), "{what} / {fix}");
}

#[test]
fn the_cohost_voice_buses_are_added_once() {
    let engine = MockEngine::new();
    ensure_voice_buses(&engine).unwrap();
    let graph = engine.graph();
    for bus in [AI_VOICE_BUS, AI_VOICE_PRIVATE_BUS] {
        assert!(graph.bus(&BusId::new(bus).unwrap()).is_some(), "{bus}");
    }
    ensure_voice_buses(&engine).unwrap();
    assert_eq!(engine.graph().buses.len(), 2);
}

/// A one-route HTTP server on loopback for the decision provider: answers every request with `answer` and keeps what
/// it was sent (the head lower-cased, then the body).
struct FakeSystemOne {
    url: String,
    seen: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

impl FakeSystemOne {
    fn start(answer: serde_json::Value) -> Self {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line.to_ascii_lowercase());
                }
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                kept.lock().unwrap().push((head, serde_json::from_slice(&body).unwrap()));
                let text = answer.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        Self { url, seen }
    }
}

fn factory_with(decider: serde_json::Value, keys: &[(&str, &str)]) -> LocalFactory {
    factory_full(decider, json!({}), keys, None)
}

fn factory_full(
    decider: serde_json::Value,
    realtime: serde_json::Value,
    keys: &[(&str, &str)],
    engine: Option<Arc<dyn AudioEngine>>,
) -> LocalFactory {
    use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
    use omuzaki_settings_api::service::{Layer, Settings, SettingsExt as _};
    omuzaki_cohost::link();
    let settings = Arc::new(omuzaki_settings_mock::MemorySettings::default());
    let choice: DeciderChoice = serde_json::from_value(decider).unwrap();
    let person = Actor::Person { name: "host".into() };
    settings.set::<PerformerDecider>(&choice, Layer::Base, omuzaki_ops_api::Mode::Apply, &person).unwrap();
    let realtime: RealtimeChoice = serde_json::from_value(realtime).unwrap();
    settings.set::<PerformerRealtime>(&realtime, Layer::Base, omuzaki_ops_api::Mode::Apply, &person).unwrap();
    let secrets = Arc::new(omuzaki_secrets_mock::MemorySecrets::new());
    for (label, value) in keys {
        secrets.set(&SecretLabel::new(*label).unwrap(), SecretValue::new(*value)).unwrap();
    }
    let host: Arc<dyn PerformHost> = Arc::new(Host::default());
    let mut parts = LocalParts::new(host).settings(settings as Arc<dyn Settings>).secrets(secrets);
    if let Some(engine) = engine {
        parts = parts.engine(engine);
    }
    LocalFactory::new(parts)
}

fn talking() -> Arc<dyn LlmClient> {
    Arc::new(FakeLlm::new(Script::new("fake")))
}

#[test]
fn the_decider_is_the_rule_floor_unless_the_person_picks_a_model() {
    let factory = factory_with(json!({}), &[]);
    let llm = LlmChoice::default();
    assert!(factory.decider(&DeciderChoice::default(), &llm, &talking()).unwrap().is_none());
    // `llm` with no endpoint shares the co-host's own model.
    let shared = factory.decider(&serde_json::from_value(json!({ "kind": "llm" })).unwrap(), &llm, &talking());
    assert_eq!(shared.unwrap().map(|d| d.kind().to_owned()), Some("llm".into()));
    assert_eq!(factory.sends_to(), None);
}

#[test]
fn jev_is_asked_with_the_persons_own_key_through_the_systemone_api() {
    use omuzaki_performer::decide::{COMMIT, REACTION};
    use omuzaki_performer_api::decide::{Answer, Ask, Moment};

    let server = FakeSystemOne::start(json!({
        "model": "jev-1.13.0",
        "answers": {
            "reaction": { "type": "choice", "choice": "happy", "confidence": 0.9,
                          "probabilities": { "happy": 0.9, "sad": 0.1 } },
            "commit": { "type": "noul", "noul": 0.05 }
        },
        "usage": { "input_tokens": 80, "output_tokens": 0 }
    }));
    let decider = json!({ "kind": "systemone", "endpoint": server.url, "key": "typesafe", "model": "jev-latest" });
    let factory = factory_with(decider.clone(), &[("typesafe", "ts-test-key-0123")]);
    let choice: DeciderChoice = serde_json::from_value(decider).unwrap();
    let mut jev = factory.decider(&choice, &LlmChoice::default(), &talking()).unwrap().unwrap();
    assert_eq!(jev.kind(), "systemone");

    let inventory = omuzaki_performer::eval::inventory();
    let ask = Ask::new(
        Moment::Speaking,
        "person: hi\nmochi: Hi chat!",
        omuzaki_performer::decide::questions_speaking(&inventory),
    );
    let ticket = jev.ask(ask);
    let started = Instant::now();
    let decision = loop {
        if let Some(result) = ticket.poll() {
            break result.unwrap();
        }
        assert!(started.elapsed() < Duration::from_secs(10), "no answer from the fake server");
        std::thread::yield_now();
    };
    assert_eq!(decision.provider, "systemone");
    assert_eq!(decision.answer(REACTION).and_then(Answer::best).map(|b| b.0), Some(0));
    assert!(decision.answer(COMMIT).is_some_and(|a| a.yes() < 0.1));

    let seen = server.seen.lock().unwrap();
    let (head, body) = &seen[0];
    assert!(head.starts_with("post /v1/systemone "), "{head}");
    assert!(head.contains("authorization: bearer ts-test-key-0123"));
    assert_eq!(body["model"], "jev-latest");
    assert_eq!(body["questions"]["commit"]["type"], "noul");
}

#[test]
fn a_hosted_decider_needs_a_key_permission_and_a_person() {
    let llm = LlmChoice::default();
    // TypeSafe's own endpoint, with no key named.
    let factory = factory_with(json!({ "kind": "systemone", "allow_remote": true }), &[]);
    let choice: DeciderChoice = serde_json::from_value(json!({ "kind": "systemone", "allow_remote": true })).unwrap();
    let Err(StartError::NotReady { what, .. }) = factory.decider(&choice, &llm, &talking()) else {
        panic!("a hosted decider without a key")
    };
    assert!(what.contains("no key"), "{what}");
    // Its host is where `performer.start` says the transcript goes, so a person must confirm.
    assert_eq!(factory.sends_to().as_deref(), Some("api.typesafe.ai"));

    // A key named but missing from the keychain.
    let named = json!({ "kind": "systemone", "allow_remote": true, "key": "typesafe" });
    let Err(StartError::NotReady { what, .. }) =
        factory.decider(&serde_json::from_value(named).unwrap(), &llm, &talking())
    else {
        panic!("a key that isn't there")
    };
    assert!(what.contains("not in the keychain"), "{what}");

    // A key, but sending off this machine not allowed.
    let factory = factory_with(json!({ "kind": "systemone", "key": "typesafe" }), &[("typesafe", "ts-test")]);
    let choice: DeciderChoice = serde_json::from_value(json!({ "kind": "systemone", "key": "typesafe" })).unwrap();
    let Err(StartError::NotReady { fix, .. }) = factory.decider(&choice, &llm, &talking()) else {
        panic!("a remote host without allow_remote")
    };
    assert!(fix.contains("allow_remote"), "{fix}");
}

#[test]
fn a_realtime_model_needs_a_key_and_permission_and_says_what_it_costs() {
    let engine: Arc<dyn AudioEngine> = Arc::new(MockEngine::new());
    let factory = factory_full(json!({}), json!({ "kind": "openai" }), &[], Some(Arc::clone(&engine)));
    // The provider's host is where the co-host's hearing goes; the local LLM isn't asked.
    assert_eq!(factory.sends_to().as_deref(), Some("api.openai.com"));
    let Err(StartError::NotReady { fix, .. }) = factory.check() else { panic!("a hosted model without allow_remote") };
    assert!(fix.contains("allow_remote"), "{fix}");

    let factory =
        factory_full(json!({}), json!({ "kind": "openai", "allow_remote": true }), &[], Some(Arc::clone(&engine)));
    let Err(StartError::NotReady { what, .. }) = factory.check() else { panic!("a hosted model without a key") };
    assert!(what.contains("no key"), "{what}");

    let realtime = json!({ "kind": "openai", "allow_remote": true, "key": "openai", "cap_cents": 250 });
    let factory = factory_full(json!({}), realtime, &[("openai", "sk-test-key")], Some(engine));
    factory.check().unwrap();
    // A minute heard is 600 audio tokens at $32 per million; a minute spoken, 1200 at $64.
    let costs = factory.costs().unwrap();
    assert_eq!(costs["listening_usd_per_minute"], 0.0192);
    assert_eq!(costs["speaking_usd_per_minute"], 0.0768);
    assert_eq!(costs["cap_usd"], 2.5);
    assert_eq!(costs["prices_known"], true);
    // Off by default: nothing is billed.
    assert!(factory_with(json!({}), &[]).costs().is_none());
}

/// A provider on this machine for one realtime session: it checks the key and the session, waits for the
/// microphone's audio, hears `heard`, and answers the first `response.create` with `reply` (100 ms of audio per
/// delta) and its bill.
fn fake_realtime(
    heard: &'static str,
    reply: &'static str,
    deltas: usize,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use base64::Engine as _;
    use tungstenite::Message;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/v1/realtime", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut seen = Vec::new();
        #[allow(clippy::result_large_err)] // tungstenite's own callback signature
        let callback = |req: &tungstenite::handshake::server::Request,
                        res: tungstenite::handshake::server::Response|
         -> Result<
            tungstenite::handshake::server::Response,
            tungstenite::handshake::server::ErrorResponse,
        > {
            seen.push(format!(
                "auth {}",
                req.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("")
            ));
            Ok(res)
        };
        let mut ws = tungstenite::accept_hdr(tcp, callback).unwrap();
        let next = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, kind: &str| loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                    if v["type"] == kind {
                        break Some(v);
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break None,
                Ok(_) => {}
            }
        };
        let update = next(&mut ws, "session.update").unwrap();
        seen.push(format!("instructions {}", update["session"]["instructions"]));
        seen.push(format!("voice {}", update["session"]["audio"]["output"]["voice"]));
        next(&mut ws, "input_audio_buffer.append").unwrap();
        seen.push("heard the microphone".into());
        let say = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, v: serde_json::Value| {
            ws.send(Message::text(v.to_string())).unwrap();
        };
        say(&mut ws, json!({"type": "input_audio_buffer.speech_started", "item_id": "u1"}));
        say(&mut ws, json!({"type": "input_audio_buffer.speech_stopped", "item_id": "u1"}));
        say(
            &mut ws,
            json!({"type": "conversation.item.input_audio_transcription.completed", "item_id": "u1", "transcript": heard}),
        );
        next(&mut ws, "response.create").unwrap();
        seen.push("asked for a reply".into());
        say(&mut ws, json!({"type": "response.created", "response": {"id": "r1"}}));
        say(&mut ws, json!({"type": "response.output_audio_transcript.delta", "response_id": "r1", "delta": reply}));
        let tone: Vec<u8> = (0..2_400u16)
            .flat_map(|i| i16::try_from((i32::from(i % 100) - 50) * 100).unwrap_or(0).to_le_bytes())
            .collect();
        let audio = base64::engine::general_purpose::STANDARD.encode(tone);
        for _ in 0..deltas {
            say(
                &mut ws,
                json!({"type": "response.output_audio.delta", "response_id": "r1", "item_id": "i1", "delta": audio}),
            );
        }
        say(
            &mut ws,
            json!({"type": "response.done", "response": {"id": "r1", "status": "completed", "usage": {
            "input_token_details": {"audio_tokens": 40, "text_tokens": 500, "cached_tokens": 0},
            "output_token_details": {"audio_tokens": 20, "text_tokens": 6}}}}),
        );
        while next(&mut ws, "never").is_some() {}
        seen
    });
    (url, handle)
}

#[test]
fn the_cohost_answers_through_a_realtime_model_with_the_persons_key() {
    use omuzaki_performer_api::event::PerformerEvent;
    let (url, provider) = fake_realtime("Mochi, say hi to chat!", "Hi everyone! ", 10);
    let mock = Arc::new(MockEngine::new());
    let mic = BusSpec {
        id: BusId::new(MIC_BUS).unwrap(),
        channels: Channels::Mono,
        sources: vec![SourceRef::Device { role: omuzaki_audio_api::ids::DeviceRole::new("mic").unwrap() }],
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![],
    };
    mock.apply(&GraphSpec { buses: vec![mic], ..GraphSpec::default() }).unwrap();
    let engine: Arc<dyn AudioEngine> = Arc::clone(&mock) as _;
    let realtime = json!({ "kind": "openai", "endpoint": url, "key": "openai", "voice": "marin" });
    let factory = factory_full(json!({}), realtime, &[("openai", "sk-test-realtime-key")], Some(engine));
    assert_eq!(factory.sends_to(), None, "a provider on this machine needs no confirmation");
    let member = omuzaki_stage_api::ids::MemberId::new("mochi").unwrap();
    let Built { mut cohost, clock, about, on_stop, .. } =
        factory.build(&Booking::new(member, omuzaki_performer::persona::Persona::named("Mochi"))).unwrap();
    assert_eq!(about["realtime"]["model"], "gpt-realtime");
    assert_eq!(about["realtime"]["local"], true);
    assert!(!about.to_string().contains("sk-test"), "never a key");

    let mut lines = Vec::new();
    let started = Instant::now();
    let done = |lines: &[PerformerEvent], cohost: &mut Cohost| {
        lines.iter().any(|e| matches!(e, PerformerEvent::Speaking { .. }))
            && cohost.show().status(clock.now()).turn.is_none()
    };
    while !done(&lines, &mut cohost) && started.elapsed() < Duration::from_secs(20) {
        // 10 ms of the engine: the microphone's audio goes out, the reply's plays.
        mock.advance(480);
        cohost.step(clock.now());
        lines.extend(cohost.show().drain().into_iter().map(|l| l.event));
        std::thread::yield_now();
    }
    let status = cohost.show().status(clock.now());
    cohost.stop(clock.now());
    drop(cohost);
    if let Some(stop) = on_stop {
        stop();
    }
    let seen = provider.join().unwrap();

    assert_eq!(seen[0], "auth Bearer sk-test-realtime-key");
    assert!(seen[1].contains("Your name is Mochi") && !seen[1].contains("[emote"), "{}", seen[1]);
    assert_eq!(seen[2], "voice \"marin\"");
    assert_eq!(&seen[3..], ["heard the microphone", "asked for a reply"]);
    assert!(lines.iter().any(|e| matches!(e, PerformerEvent::Heard { text: Some(t), .. } if t.contains("say hi"))));
    assert!(
        lines.iter().any(|e| matches!(e, PerformerEvent::Speaking { text: Some(t), .. } if t == "Hi everyone!")),
        "{lines:?}"
    );
    // The reply's bill: 40 audio and 500 text tokens in, 20 audio and 6 text out.
    let (prices, _) = omuzaki_llm_realtime::prices("gpt-realtime");
    let bill = prices.cost(&omuzaki_performer_api::duplex::DuplexUsage::new(40, 500, 0, 20, 6));
    assert_eq!(status.spent_micro_usd, bill.0);
    assert_eq!(status.turns, 1);
}
