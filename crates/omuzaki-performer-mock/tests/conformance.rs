//! Every fake passes the contract's own conformance checks, and the fakes behave as `ai-performer.md` §9 describes.
// Test helpers may unwrap: a panic there is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_anim_api::score::{ClipId, ExpressionId};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_l10n_api::Locale;
use omuzaki_llm_api::stream::{Cancel, StopReason};
use omuzaki_llm_api::usage::{MicroUsd, Usage};
use omuzaki_ops_api::Spend;
use omuzaki_performer_api::brain::{Brain, BrainError, BrainEvent, Polled, TurnInput};
use omuzaki_performer_api::conformance::{self, VoiceRig};
use omuzaki_performer_api::ears::{Ears, Heard};
use omuzaki_performer_api::ids::{ConnectorId, VoiceId};
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::perception::{Glance, GlanceSource, Perception, PerceptionError};
use omuzaki_performer_api::speech::{SpeakHow, SpeechHandle, SpeechState, Voice, VoiceError};
use omuzaki_performer_api::spend::{SpendError, SpendGate};
use omuzaki_performer_api::stage::{ClipInfo, Inventory, Stage};
use omuzaki_performer_api::voice::{CutState, VoiceInfo, VoiceLead, VoiceProvider};
use omuzaki_performer_mock::{
    FakeCatalog, FakeCut, FakePerception, FakeVoice, FakeWallet, LeadCall, RecordingLead, RecordingStage, RestPlanner,
    ScriptedBrain, ScriptedEars,
};
use omuzaki_stage_api::ids::MemberId;

struct Rig {
    clock: Arc<FakeClock>,
    voice: Box<dyn Voice>,
}

impl VoiceRig for Rig {
    fn voice(&mut self) -> &mut dyn Voice {
        self.voice.as_mut()
    }

    fn voice_id(&self) -> VoiceId {
        VoiceId::new("kokoro-82m/af_heart").unwrap()
    }

    fn advance(&mut self, by: Seconds) {
        self.clock.advance(by);
    }
}

fn s(x: f64) -> Seconds {
    Seconds(x)
}

fn mochi() -> MemberId {
    MemberId::new("mochi").unwrap()
}

fn inventory() -> Inventory {
    Inventory::new(
        vec![ExpressionId::new("happy").unwrap()],
        vec![ClipInfo::new(ClipId::new("wave").unwrap(), true, 1.2)],
        vec![],
        vec![],
    )
}

#[test]
fn fake_voice_conforms() {
    let mut make = || -> Box<dyn VoiceRig> {
        let clock = Arc::new(FakeClock::default());
        Box::new(Rig { voice: Box::new(FakeVoice::new(Arc::clone(&clock), s(0.2))), clock })
    };
    conformance::voice(&mut make).unwrap();
}

/// A voice that leaves a pause before every chunk after the first: it queues an unheard filler chunk first.
struct PausingVoice {
    inner: FakeVoice,
    spoke: bool,
}

impl Voice for PausingVoice {
    fn speak(&mut self, sentence: &str, voice: &VoiceId, how: &SpeakHow) -> Result<SpeechHandle, VoiceError> {
        if std::mem::replace(&mut self.spoke, true) {
            self.inner.speak(&"-".repeat(40), voice, how)?;
        }
        self.inner.speak(sentence, voice, how)
    }
}

#[test]
fn the_voice_check_catches_a_pause_between_queued_chunks() {
    let mut make = || -> Box<dyn VoiceRig> {
        let clock = Arc::new(FakeClock::default());
        let inner = FakeVoice::new(Arc::clone(&clock), s(0.2));
        Box::new(Rig { voice: Box::new(PausingVoice { inner, spoke: false }), clock })
    };
    let why = conformance::voice(&mut make).unwrap_err();
    assert!(why.starts_with("chunks play back to back: "), "{why}");
}

#[test]
fn fake_voice_times_chunks_at_sixty_ms_per_char_and_cuts_within_a_block() {
    let clock = Arc::new(FakeClock::default());
    let mut voice = FakeVoice::new(Arc::clone(&clock), s(0.0));
    let id = VoiceId::new("kokoro-82m/af_heart").unwrap();
    let a = voice.speak("Hi chat!", &id, &SpeakHow::default()).unwrap();
    let b = voice.speak("Fold it and see!", &id, &SpeakHow::default().private()).unwrap();
    assert_eq!(a.state(), SpeechState::playing(s(0.0), s(0.48)));
    assert_eq!(b.state(), SpeechState::ready(s(0.96)));
    clock.set(s(0.48 + 0.3));
    assert_eq!(b.state(), SpeechState::playing(s(0.48), s(0.96)));
    b.cut();
    assert!(matches!(b.state(), SpeechState::Playing { .. }), "the cut lands at the next block, not before");
    clock.advance(s(0.005));
    assert_eq!(b.state(), SpeechState::cut(s(0.785), 5));
    let log = voice.log();
    assert_eq!((log[1].how.private, log[1].cut), (true, Some((s(0.785), 5))));
}

#[test]
fn scripted_ears_conform_and_release_on_time() {
    let clock = Arc::new(FakeClock::default());
    let script = vec![
        (s(1.0), Heard::onset(s(1.0))),
        (s(1.1), Heard::held(s(1.1))),
        (s(2.0), Heard::final_text(0, "Mochi, hi", s(1.0), s(2.0))),
        (s(3.0), Heard::Muted(true)),
        (s(4.0), Heard::Muted(false)),
    ];
    let events: Vec<Heard> = script.iter().map(|(_, h)| h.clone()).collect();
    conformance::heard_sequence(&events).unwrap();
    let mut ears = ScriptedEars::new(Arc::clone(&clock), script);
    assert_eq!(ears.next(), None);
    clock.set(s(1.5));
    assert_eq!((ears.next(), ears.next(), ears.next()), (Some(Heard::onset(s(1.0))), Some(Heard::held(s(1.1))), None));
    clock.set(s(10.0));
    let mut rest = Vec::new();
    while let Some(h) = ears.next() {
        rest.push(h);
    }
    assert_eq!((rest.len(), ears.pending()), (3, 0));
}

#[test]
fn the_sequence_check_catches_broken_streams() {
    let bad = [
        vec![Heard::held(s(1.0))],
        vec![Heard::onset(s(2.0)), Heard::rejected(s(1.0))],
        vec![Heard::Muted(true), Heard::onset(s(1.0))],
        vec![Heard::onset(s(1.0)), Heard::held(s(1.1)), Heard::held(s(1.2))],
        vec![
            Heard::onset(s(1.0)),
            Heard::final_text(3, "a", s(1.0), s(2.0)),
            Heard::onset(s(3.0)),
            Heard::final_text(2, "b", s(3.0), s(4.0)),
        ],
    ];
    for events in bad {
        assert!(conformance::heard_sequence(&events).is_err(), "{events:?}");
    }
}

#[test]
fn recording_stage_conforms_and_records() {
    let stage = RecordingStage::new(mochi(), inventory());
    conformance::stage(&stage, &mochi()).unwrap();
    stage.release(&mochi(), s(0.0));
    let wave = Intent::gesture(ClipId::new("wave").unwrap(), 1.0);
    stage.apply(&mochi(), &wave, s(1.0)).unwrap();
    stage.apply(&mochi(), &wave, s(3.0)).unwrap();
    assert!(stage.apply(&mochi(), &Intent::play_clip(ClipId::new("backflip").unwrap()), s(1.0)).is_err());
    stage.release(&mochi(), s(2.0));
    let applied = stage.applied();
    assert_eq!((applied.len(), applied[0].at), (1, s(1.0)));
    assert_eq!(stage.released().last(), Some(&(mochi(), s(2.0))));
}

#[test]
fn fake_catalog_conforms_and_the_check_catches_strays() {
    let local = VoiceProvider::Local;
    let hosted = VoiceProvider::Hosted(ConnectorId::new("tts-cloud").unwrap());
    let voice = |id: &str| VoiceInfo::new(VoiceId::new(id).unwrap(), vec![Locale::EN]);
    let catalog = FakeCatalog::new()
        .with(&local, vec![voice("kokoro-82m/af_heart"), voice("kokoro-82m/bf_emma")])
        .with(&hosted, vec![voice("tts-cloud/Rachel")]);
    conformance::catalog(&catalog, &local).unwrap();
    conformance::catalog(&catalog, &hosted).unwrap();
    let stray = FakeCatalog::new().with(&hosted, vec![voice("other-cloud/Rachel")]);
    assert!(conformance::catalog(&stray, &hosted).is_err());
    let twice = FakeCatalog::new().with(&local, vec![voice("k/a"), voice("k/a")]);
    assert!(conformance::catalog(&twice, &local).is_err());
}

#[test]
fn rest_planner_conforms() {
    conformance::planner(&|| Box::new(RestPlanner)).unwrap();
}

#[test]
fn scripted_brain_releases_events_on_the_clock() {
    let clock = Arc::new(FakeClock::default());
    let turn = vec![
        (s(0.59), BrainEvent::Text("Hi chat!".into())),
        (s(0.6), BrainEvent::done(Usage::new(10, 3), StopReason::EndTurn)),
    ];
    let mut brain = ScriptedBrain::new(Arc::clone(&clock), vec![turn]);
    let cancel = Cancel::new();
    let mut stream = brain.turn(TurnInput::new(1, inventory()).heard("Mochi, hi"), &cancel).unwrap();
    assert_eq!(stream.try_next(), Ok(Polled::Pending));
    clock.set(s(0.6));
    assert_eq!(stream.try_next(), Ok(Polled::Event(BrainEvent::Text("Hi chat!".into()))));
    assert!(matches!(stream.try_next(), Ok(Polled::Event(BrainEvent::Done { .. }))));
    assert_eq!(stream.try_next(), Ok(Polled::End));
    assert_eq!(brain.inputs()[0].heard.as_deref(), Some("Mochi, hi"));
    assert!(matches!(brain.turn(TurnInput::new(2, inventory()), &cancel), Err(BrainError::Llm(_))));
}

#[test]
fn fake_perception_and_lead_record_what_they_get() {
    let glance = Glance::new(GlanceSource::Screen("Game".into()), s(1.0), 640, 360, vec![0xFF, 0xD8]).unwrap();
    let mut eyes = FakePerception::new(vec![Ok(glance.clone()), Err(PerceptionError::SourceGone("Game".into()))]);
    assert_eq!(eyes.glance(), Ok(Some(glance)));
    assert!(eyes.glance().is_err());
    assert_eq!(eyes.glance(), Ok(None));

    let cut = FakeCut::new();
    let mut lead = RecordingLead::new();
    lead.attach(cut.probe());
    lead.push(s(0.9), 24_000, &[0.0; 240], true);
    cut.set(CutState::cut(s(1.0)));
    lead.cut(s(1.0));
    assert_eq!(
        lead.calls(),
        vec![
            LeadCall::Attach(CutState::Live),
            LeadCall::Push { at: s(0.9), rate: 24_000, len: 240, private: true },
            LeadCall::Cut(s(1.0)),
        ]
    );
    assert_eq!(cut.probe().state(), CutState::cut(s(1.0)));
}

#[test]
fn the_gate_over_a_fake_wallet_holds_the_cap() {
    let wallet = FakeWallet::new(MicroUsd(100));
    let gate = SpendGate::new(Arc::new(wallet.clone()));
    let llm = gate.reserve(MicroUsd(80)).unwrap();
    assert!(matches!(gate.reserve(MicroUsd(30)), Err(SpendError::Exhausted { .. })));
    llm.settle(Spend { usd_micros: 42, tokens: 900, seconds: 0 }).unwrap();
    assert_eq!((wallet.spent(), wallet.charges()[0].tokens), (MicroUsd(42), 900));
    assert!(gate.reserve(MicroUsd(30)).is_ok_and(|r| r.forfeit().is_ok()));
    assert_eq!(wallet.spent(), MicroUsd(72));
}
