//! The voice-command listener (issue #1322): raw-mic transcripts become `voice.command` events; the AI's voice never
//! does; off by default; only a person turns it on.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss)]

mod common;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_audio_api::tap::VoiceUse;
use omuzaki_audio_api::{BusId, TapPoint};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError, Source, check_payload, find_event_kind};
use omuzaki_ops_api::{
    Actor, ErrorCode, ErrorDetail, Grants, Op, OpCtx, OpError, Plan, Scope, Services, ServicesBuilder,
};
use omuzaki_settings_api::service::Settings;
use omuzaki_settings_mock::MemorySettings;
use omuzaki_speech::SpeechError;
use omuzaki_speech::asr::{Recognizer, Transcript};
use omuzaki_speech::commands::{
    CommandListener, CommandStatus, PhraseList, VoiceCommand, VoiceCommands, VoiceCommandsDisable, VoiceCommandsEnable,
    VoiceCommandsSetPhrases, VoiceCommandsStatus, default_phrases,
};
use omuzaki_speech::host::{HostConfig, Latency, SpeechEvent, SpeechHost};
use serde_json::json;

/// A sink that checks every event against its registered kind, records it, and can be made to refuse as full.
#[derive(Default)]
struct Inbox {
    events: Mutex<Vec<LiveEvent>>,
    full: Mutex<bool>,
    lost: Mutex<u64>,
    arrived: Mutex<Option<Sender<Instant>>>,
}

impl EventSink for Inbox {
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError> {
        check_payload(&event.kind, &event.payload)?;
        if *self.full.lock().unwrap() {
            return Err(PublishError::Full);
        }
        self.events.lock().unwrap().push(event);
        if let Some(tx) = self.arrived.lock().unwrap().as_ref() {
            let _ = tx.send(Instant::now());
        }
        Ok(())
    }

    fn producer_dropped(&self, count: u64) {
        *self.lost.lock().unwrap() += count;
    }
}

impl Inbox {
    fn commands(&self) -> Vec<(String, Seconds)> {
        let events = self.events.lock().unwrap();
        events.iter().map(|e| (e.decode::<VoiceCommand>().unwrap().unwrap().id, e.at)).collect()
    }

    fn notify(&self) -> Receiver<Instant> {
        let (tx, rx) = channel();
        *self.arrived.lock().unwrap() = Some(tx);
        rx
    }
}

fn mic() -> TapPoint {
    TapPoint::Voice(VoiceUse::Listen)
}

fn final_(text: &str, end: f64) -> SpeechEvent {
    SpeechEvent::Final {
        utterance: 0,
        start: end - 1.0,
        end,
        text: text.to_owned(),
        latency: Latency { queue_wait: Duration::ZERO, asr: Duration::ZERO, detect_to_final: Duration::ZERO },
        private: false,
    }
}

fn listener(tap: &TapPoint) -> (VoiceCommands, Arc<Inbox>, CommandListener) {
    let commands = VoiceCommands::new(default_phrases());
    let inbox = Arc::new(Inbox::default());
    let listener = CommandListener::new(commands.clone(), inbox.clone(), tap, Seconds(100.0));
    (commands, inbox, listener)
}

#[test]
fn the_kind_is_registered_with_its_payload() {
    let kind = find_event_kind("voice.command").expect("registered by omuzaki-speech");
    assert!(!kind.droppable, "a person's command is never dropped under load");
    assert!((kind.validate)(&json!({ "id": "wave", "phrase": "wave", "score": 1.0 })).is_ok());
    assert!((kind.validate)(&json!({ "id": "wave" })).is_err());
}

#[test]
fn a_transcript_of_wave_yields_one_event_on_the_runner_clock() {
    let (commands, inbox, mut listener) = listener(&mic());
    commands.set_enabled(true);
    assert_eq!(listener.on_event(&final_("Wave!", 2.5)), 1);
    let events = inbox.events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source, Source::Voice);
    assert_eq!(events[0].at, Seconds(102.5));
    assert_eq!(events[0].payload, json!({ "id": "wave", "phrase": "wave", "score": 1.0 }));
    assert_eq!(commands.status(), CommandStatus::Heard { id: "wave".into(), at: Seconds(102.5) });
    // Two commands in one utterance, in order; small talk gives none.
    assert_eq!(listener.on_event(&final_("smile and take a bow", 4.0)), 2);
    assert_eq!(listener.on_event(&final_("thanks for the follow", 5.0)), 0);
    let ids: Vec<String> = inbox.commands().into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, ["wave", "smile", "bow"]);
    assert_eq!(listener.stats().transcripts, 3);
    assert_eq!(listener.stats().published, 3);
}

#[test]
fn no_transcript_text_reaches_the_event() {
    let (commands, inbox, mut listener) = listener(&mic());
    commands.set_enabled(true);
    listener.on_event(&final_("Sit town, wave please, secret password", 1.0));
    listener.on_event(&final_("Can you wave please", 2.0));
    let events = inbox.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    let text = serde_json::to_string(&*events).unwrap();
    assert!(!text.contains("please") && !text.contains("Can you"), "{text}");
}

#[test]
fn the_ai_voice_tap_yields_none() {
    for tap in [
        TapPoint::BusPostFader(BusId::new("ai_voice").unwrap()),
        TapPoint::BusPostChain(BusId::new("soundboard").unwrap()),
        TapPoint::Voice(VoiceUse::Captions),
        TapPoint::Voice(VoiceUse::Mouth),
    ] {
        let (commands, inbox, mut listener) = listener(&tap);
        commands.set_enabled(true);
        assert_eq!(listener.on_event(&final_("wave", 1.0)), 0, "{tap:?}");
        assert!(inbox.events.lock().unwrap().is_empty());
        assert_eq!(listener.stats().not_the_mic, 1);
        assert!(matches!(commands.status(), CommandStatus::Paused { .. }), "{:?}", commands.status());
    }
}

#[test]
fn disabled_yields_none_and_the_mute_pauses() {
    let (commands, inbox, mut listener) = listener(&mic());
    assert_eq!(commands.status(), CommandStatus::Off, "off by default");
    assert_eq!(listener.on_event(&final_("wave", 1.0)), 0);
    assert_eq!(listener.stats().transcripts, 0);
    commands.set_enabled(true);
    listener.on_event(&SpeechEvent::Muted(true));
    assert_eq!(commands.status(), CommandStatus::Paused { why: "the microphone is muted".into() });
    listener.on_event(&SpeechEvent::Muted(false));
    assert_eq!(commands.status(), CommandStatus::Listening);
    commands.set_enabled(false);
    assert_eq!(listener.on_event(&final_("nod", 2.0)), 0);
    assert!(inbox.events.lock().unwrap().is_empty());
}

#[test]
fn a_full_inbox_keeps_the_commands_in_order_and_retries() {
    let (commands, inbox, mut listener) = listener(&mic());
    commands.set_enabled(true);
    *inbox.full.lock().unwrap() = true;
    for (i, text) in ["wave", "nod", "smile"].iter().enumerate() {
        listener.on_event(&final_(text, i as f64));
    }
    assert!(inbox.events.lock().unwrap().is_empty());
    *inbox.full.lock().unwrap() = false;
    listener.on_event(&final_("take a bow", 9.0));
    let ids: Vec<String> = inbox.commands().into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, ["wave", "nod", "smile", "bow"]);
    // Past the retry buffer the oldest are lost and reported.
    *inbox.full.lock().unwrap() = true;
    for i in 0..20 {
        listener.on_event(&final_("wave", 10.0 + f64::from(i)));
    }
    assert_eq!(*inbox.lost.lock().unwrap(), 4);
    assert_eq!(listener.stats().dropped, 4);
}

// ── The ops ─────────────────────────────────────────────────────────────────────────────────────────────────────

fn person() -> Actor {
    Actor::Person { name: "Pat".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "helper".into(), model: None }
}

fn services(commands: &VoiceCommands, settings: Arc<MemorySettings>) -> Services {
    ServicesBuilder::new().insert::<VoiceCommands>(Arc::new(commands.clone())).insert::<dyn Settings>(settings).build()
}

fn run<O: Op>(op: &O, actor: &Actor, services: &Services) -> Result<Plan, OpError> {
    let (doc, clock) = (MemDoc::default(), FakeClock::new(Seconds(0.0)));
    let grants = Grants::of([Scope::Read, Scope::EditSettings]);
    let ctx = OpCtx::new(&doc, actor, &clock).with_services(services).with_grants(&grants);
    op.plan(&ctx)
}

#[test]
fn an_agents_enable_is_denied_and_anyone_may_disable() {
    let commands = VoiceCommands::new(default_phrases());
    let settings = Arc::new(MemorySettings::default());
    let services = services(&commands, Arc::clone(&settings));

    let denied = run(&VoiceCommandsEnable::default(), &agent(), &services).unwrap_err();
    assert_eq!(denied.code, ErrorCode::Denied);
    assert!(matches!(denied.detail.as_deref(), Some(ErrorDetail::PersonOnly { .. })), "{denied:?}");
    assert!(!commands.enabled());

    let enabled = run(&VoiceCommandsEnable::default(), &person(), &services).unwrap();
    assert_eq!(enabled.output, Some(json!({ "enabled": true, "was": false })));
    assert!(commands.enabled());
    assert_eq!(settings.resolve("voice_commands.enabled").unwrap().value, json!(true));

    let status = run(&VoiceCommandsStatus::default(), &agent(), &services).unwrap().output.unwrap();
    assert_eq!(status["status"], json!({ "state": "listening" }));
    assert_eq!(status["commands"].as_array().unwrap().len(), 10);
    assert_eq!(status["languages"], json!(["en", "ja"]));

    run(&VoiceCommandsDisable::default(), &agent(), &services).unwrap();
    assert!(!commands.enabled());
    assert_eq!(settings.resolve("voice_commands.enabled").unwrap().value, json!(false));
}

#[test]
fn phrases_go_to_the_persons_settings_and_an_agent_needs_their_yes() {
    let commands = VoiceCommands::new(default_phrases());
    let settings = Arc::new(MemorySettings::default());
    let services = services(&commands, Arc::clone(&settings));
    let list = PhraseList::from_toml("[[command]]\nid = \"hype\"\nsay = [\"lets go\"]").unwrap();
    let op = VoiceCommandsSetPhrases::new(list.clone());

    let asked = run(&op, &agent(), &services).unwrap_err();
    assert!(matches!(asked.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })), "{asked:?}");
    assert_eq!(commands.phrases().command.len(), 20);

    run(&op, &person(), &services).unwrap();
    assert_eq!(*commands.phrases(), list);
    let stored: PhraseList = serde_json::from_value(settings.resolve("voice_commands.phrases").unwrap().value).unwrap();
    assert_eq!(stored, list);

    let bad = PhraseList::new(vec![omuzaki_speech::commands::Command::new("Bad Id", &["x"], "en")]);
    assert_eq!(run(&VoiceCommandsSetPhrases::new(bad), &person(), &services).unwrap_err().code, ErrorCode::Invalid);
}

// ── On a running host ───────────────────────────────────────────────────────────────────────────────────────────

/// Says one scripted line per utterance and counts the calls.
struct Script {
    lines: Vec<&'static str>,
    calls: Arc<Mutex<usize>>,
}

impl Recognizer for Script {
    fn transcribe(&mut self, _audio: &[f32], _language: Option<&str>) -> Result<Transcript, SpeechError> {
        let mut calls = self.calls.lock().unwrap();
        let text = self.lines.get(*calls).copied().unwrap_or("");
        *calls += 1;
        Ok(Transcript { text: text.to_owned(), elapsed: Duration::ZERO })
    }
}

fn utterance() -> Vec<f32> {
    let tone: Vec<f32> = (0..8_000).map(|i| 0.2 * (i as f32 * 0.07).sin()).collect();
    common::cat(&[&vec![0.0; 3_200], &tone, &vec![0.0; 16_000]])
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::yield_now();
    }
}

#[test]
fn on_a_host_whisper_runs_only_while_commands_are_on_and_the_ears_still_hear() {
    let calls = Arc::new(Mutex::new(0));
    let script = Script { lines: vec!["wave please"], calls: Arc::clone(&calls) };
    let (mut host, mut input) = SpeechHost::start(
        &HostConfig::default(),
        Box::new(common::EnergyVad { delay: Duration::ZERO }),
        Box::new(script),
    )
    .unwrap();
    let (commands, inbox, listener) = listener(&mic());
    let arrived = inbox.notify();
    let thread = listener.spawn(&host).unwrap();

    // Off: the utterance is heard but not transcribed, and the host's own queue still gets every event.
    common::feed_all(&mut input, &utterance());
    until("the utterance to end", || host.stats().utterances_skipped == 1);
    assert_eq!(*calls.lock().unwrap(), 0);
    let own = common::drain(&host);
    assert!(own.iter().any(|e| matches!(e, SpeechEvent::SpeechEnded { .. })), "{own:?}");

    // On: the thread claims transcripts, and the next utterance becomes a command.
    commands.set_enabled(true);
    until("the listener to claim transcripts", || host.transcripts_wanted());
    common::feed_all(&mut input, &utterance());
    let at = arrived.recv_timeout(Duration::from_secs(10)).unwrap();
    let finals = common::drain(&host);
    assert!(finals.iter().any(|e| matches!(e, SpeechEvent::Final { text, .. } if text == "wave please")));
    assert!(at.elapsed() < Duration::from_secs(1));
    assert_eq!(inbox.commands().len(), 1);
    assert_eq!(inbox.commands()[0].0, "wave");

    // Off again: the claim goes.
    commands.set_enabled(false);
    until("the listener to drop its claim", || !host.transcripts_wanted());
    let listener = thread.stop().unwrap();
    assert_eq!(listener.stats().published, 1);
    host.stop();
}

#[test]
fn the_code_adds_well_under_100_ms_from_the_end_of_an_utterance() {
    let (commands, _inbox, mut listener) = listener(&mic());
    commands.set_enabled(true);
    let lines = ["wave", "nod your head please", "thanks chat", "手を振ってください", "smile then take a bow"];
    let mut worst = Duration::ZERO;
    for round in 0..200 {
        let event = final_(lines[round % lines.len()], f64::from(u32::try_from(round).unwrap()));
        let start = Instant::now();
        listener.on_event(&event);
        worst = worst.max(start.elapsed());
    }
    eprintln!("the slowest Final to publish: {worst:?}");
    assert!(worst < Duration::from_millis(100), "{worst:?}");
}
