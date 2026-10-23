//! The realtime turn loop (#2347) on the fake harness: `FakeDuplex` hears and answers, `FakePcmVoice` plays the
//! reply's audio and `RecordingStage` takes the faces, all on one fake clock stepped every 5 ms.
// Test helpers may unwrap: a panic there is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_anim_api::score::{ClipId, ExpressionId};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_llm_api::usage::MicroUsd;
use omuzaki_performer::addressing::Respond;
use omuzaki_performer::persona::Persona;
use omuzaki_performer::realtime::{RealtimeParts, RealtimeShow};
use omuzaki_performer::show::{Performer, Phase, ShowConfig};
use omuzaki_performer_api::duplex::{DuplexEvent, DuplexUsage};
use omuzaki_performer_api::event::PerformerEvent;
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::limits::TURN_SPEECH_S;
use omuzaki_performer_api::spend::SpendGate;
use omuzaki_performer_api::stage::{ClipInfo, Inventory};
use omuzaki_performer_mock::{
    DuplexCall, FakeDuplex, FakePcmVoice, FakeWallet, Played, RecordingStage, realtime_prices, reply_events,
};
use omuzaki_stage_api::ids::MemberId;

const STEP_MS: u64 = 5;
/// The TTS host's feed lead: audio queued now starts this much later.
const LEAD_S: f64 = 0.05;

fn secs(ms: u64) -> Seconds {
    #[allow(clippy::cast_precision_loss)]
    Seconds(ms as f64 / 1000.0)
}

fn mochi() -> Persona {
    Persona::named("Mochi")
}

fn inventory() -> Inventory {
    Inventory::new(
        ["happy", "sad", "surprised", "smug", "thinking"].iter().map(|e| ExpressionId::new(*e).unwrap()).collect(),
        vec![ClipInfo::new(ClipId::new("wave").unwrap(), true, 1.2)],
        vec![],
        vec![],
    )
}

fn usage() -> DuplexUsage {
    DuplexUsage::new(300, 400, 0, 60, 20)
}

struct Rig {
    clock: Arc<FakeClock>,
    duplex: FakeDuplex,
    voice: FakePcmVoice,
    stage: Arc<RecordingStage>,
    wallet: FakeWallet,
    show: RealtimeShow,
    lines: Vec<(u64, PerformerEvent)>,
    now_ms: u64,
}

impl Rig {
    fn new(script: impl FnOnce(FakeDuplex) -> FakeDuplex, respond: Respond, cap: MicroUsd) -> Self {
        let clock = Arc::new(FakeClock::default());
        let duplex = script(FakeDuplex::new(Arc::clone(&clock)));
        let voice = FakePcmVoice::new(Arc::clone(&clock), Seconds(LEAD_S));
        let member = MemberId::new("mochi").unwrap();
        let stage = Arc::new(RecordingStage::new(member.clone(), inventory()));
        let wallet = FakeWallet::new(cap);
        let parts = RealtimeParts::new(Box::new(duplex.clone()), Box::new(voice.clone()), Arc::clone(&stage) as _)
            .spend(SpendGate::new(Arc::new(wallet.clone())));
        let config = ShowConfig::new(member, VoiceId::new("openai/marin").unwrap()).respond(respond);
        let show = RealtimeShow::new(&mochi(), parts, config);
        Self { clock, duplex, voice, stage, wallet, show, lines: Vec::new(), now_ms: 0 }
    }

    fn run_to(&mut self, end_ms: u64) {
        while self.now_ms <= end_ms {
            self.clock.set(secs(self.now_ms));
            self.show.step(secs(self.now_ms));
            self.lines.extend(self.show.drain().into_iter().map(|l| (l.t, l.event)));
            self.now_ms += STEP_MS;
        }
    }

    fn played(&self) -> Vec<Played> {
        self.voice.log()
    }

    fn audio_s(&self) -> f64 {
        self.played().iter().map(|p| p.end().0 - p.start.0).sum()
    }

    /// When the last audible audio ended (a stretch cut before it started never sounds).
    fn last_sound(&self) -> f64 {
        self.played().iter().filter(|p| p.end().0 > p.start.0).map(|p| p.end().0).fold(0.0, f64::max)
    }

    fn faces(&self) -> Vec<String> {
        self.stage
            .applied()
            .into_iter()
            .filter_map(|a| match a.intent {
                Intent::Emote { expression, .. } => Some(expression.to_string()),
                _ => None,
            })
            .collect()
    }

    fn said(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|(_, e)| match e {
                PerformerEvent::Speaking { text, .. } => text.clone(),
                _ => None,
            })
            .collect()
    }

    fn cancels(&self) -> Vec<(String, Option<String>, u64)> {
        self.duplex
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                DuplexCall::Cancel(_, r, i, ms) => Some((r, i, ms)),
                _ => None,
            })
            .collect()
    }

    fn replies(&self) -> Vec<Option<String>> {
        self.duplex
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                DuplexCall::Reply(_, i) => Some(i),
                _ => None,
            })
            .collect()
    }

    fn has(&self, f: impl Fn(&PerformerEvent) -> bool) -> bool {
        self.lines.iter().any(|(_, e)| f(e))
    }
}

const CAP: MicroUsd = MicroUsd(1_000_000);

#[test]
fn in_always_mode_the_provider_answers_by_itself_and_the_reply_plays_with_faces_and_a_bill() {
    let mut rig = Rig::new(
        |d| {
            d.says(secs(0), secs(1_000), "What's your favourite fold?").answer(reply_events(
                "r1",
                "The crane, obviously! I love it so much. ",
                2.0,
                usage(),
            ))
        },
        Respond::Always,
        CAP,
    );
    rig.run_to(4_000);

    // It switched automatic replies on and never had to ask.
    assert_eq!(rig.duplex.calls().first(), Some(&DuplexCall::Auto(Seconds(0.0), true)));
    assert!(rig.replies().is_empty(), "{:?}", rig.duplex.calls());
    // The reply's two seconds played back to back, from about 360 ms after the person stopped.
    let played = rig.played();
    assert_eq!(played.len(), 20);
    let first = played[0].start.0 - 1.0;
    assert!((0.3..0.45).contains(&first), "first audio {first} s after the person stopped");
    assert!(played.windows(2).all(|w| (w[1].start.0 - w[0].end().0).abs() < 1e-9), "gapless");
    assert!((rig.audio_s() - 2.0).abs() < 1e-6);
    // The transcript reads as the cascaded show's: heard, thinking, the sentences spoken.
    assert!(rig.has(|e| matches!(e, PerformerEvent::Heard { text: Some(t), .. } if t.contains("favourite fold"))));
    assert!(rig.has(|e| matches!(e, PerformerEvent::Thinking { turn: 1, .. })));
    assert_eq!(rig.said(), ["The crane, obviously!", "I love it so much."]);
    // The decision layer read the transcript: the person's question and the character's delight.
    let faces = rig.faces();
    assert!(faces.contains(&"happy".to_owned()), "{faces:?}");
    // The reply reserved its bound and settled at the provider's bill.
    let cost = realtime_prices().cost(&usage());
    assert_eq!(rig.wallet.spent(), cost);
    assert_eq!(rig.show.spent(), cost);
    let status = rig.show.status(secs(4_000));
    assert_eq!((status.phase, status.turns, status.spent_micro_usd), (Phase::Idle, 1, cost.0));
    assert_eq!(status.decisions.provider, "rules");
    assert!(status.decisions.answered >= 2);
}

#[test]
fn talking_over_a_reply_cuts_it_at_once_and_tells_the_provider_what_was_heard() {
    let mut rig = Rig::new(
        |d| {
            d.says(secs(0), secs(1_000), "Tell me about folding.")
                .answer(reply_events("r1", "Folding is a long story that starts with one square. ", 10.0, usage()))
                .says(secs(2_500), secs(3_200), "Wait, hold on.")
        },
        Respond::Always,
        CAP,
    );
    rig.run_to(5_000);

    // Audio started at 1.36 s; the person spoke at 2.5 s, so about 1140 ms was heard.
    let start = rig.played()[0].start.0;
    let heard_ms = ((2.5 - start) * 1000.0).round();
    let cancels = rig.cancels();
    assert_eq!(cancels.len(), 1, "{cancels:?}");
    let (reply, item, ms) = &cancels[0];
    assert_eq!((reply.as_str(), item.as_deref()), ("r1", Some("r1-item")));
    #[allow(clippy::cast_precision_loss)]
    let off = (*ms as f64 - heard_ms).abs();
    assert!(off <= 10.0, "told {ms} ms, heard {heard_ms} ms");
    // Silence within one audio block, and nothing after.
    let end = rig.last_sound();
    assert!(end <= 2.5 + 0.0051, "audio ran to {end}");
    assert!(rig.has(|e| matches!(e, PerformerEvent::Held { turn: 1, .. })));
    assert!(rig.has(|e| matches!(e, PerformerEvent::Interrupted { turn: 1, .. })));
    assert_eq!(rig.stage.released().len(), 1);
    // It was cut while still streaming: it bills what the provider said it used, not its whole bound.
    let half = DuplexUsage::new(300, 400, 0, 30, 10);
    assert_eq!(rig.wallet.spent(), realtime_prices().cost(&half));
    assert_eq!(rig.wallet.charges().len(), 1);
}

#[test]
fn in_addressed_mode_it_asks_for_a_reply_only_when_named_or_followed_up() {
    let mut rig = Rig::new(
        |d| {
            d.says(secs(0), secs(800), "Okay chat, next game.")
                .says(secs(2_000), secs(3_000), "Mochi, what do you think?")
                .answer(reply_events("r1", "I think it's great. ", 1.0, usage()))
                .says(secs(6_000), secs(7_000), "Really?")
                .answer(reply_events("r2", "Really really. ", 0.5, usage()))
                .says(secs(20_000), secs(21_000), "Anyway.")
        },
        Respond::Addressed,
        CAP,
    );
    rig.run_to(23_000);

    assert!(rig.duplex.calls().contains(&DuplexCall::Auto(Seconds(0.0), false)));
    assert!(!rig.duplex.auto());
    // Named at 3.1 s, followed up at 7.1 s (within the window); neither unnamed line far from a reply.
    assert_eq!(rig.replies(), [None, None]);
    assert!(rig.has(|e| matches!(e, PerformerEvent::Heard { addressed: Some(a), .. } if a == "no: no name")));
    assert!(rig.has(|e| matches!(e, PerformerEvent::Heard { addressed: Some(a), .. } if a.starts_with("follow-up"))));
    assert_eq!(rig.said(), ["I think it's great.", "Really really."]);
}

#[test]
fn a_reply_stops_at_the_turn_limit() {
    let long = TURN_SPEECH_S + 5.0;
    let mut rig = Rig::new(
        |d| {
            d.says(secs(0), secs(1_000), "Tell me everything.").answer(reply_events(
                "r1",
                "Once upon a time. ",
                long,
                usage(),
            ))
        },
        Respond::Always,
        CAP,
    );
    rig.run_to(30_000);
    assert!(rig.audio_s() <= TURN_SPEECH_S + 1e-6, "played {}", rig.audio_s());
    assert!(rig.audio_s() >= TURN_SPEECH_S - 0.1 - 1e-6);
    let cancels = rig.cancels();
    assert_eq!(cancels.len(), 1);
    assert!((19_900..=20_000).contains(&cancels[0].2), "{cancels:?}");
    assert!(rig.has(|e| matches!(e, PerformerEvent::Filtered { why, .. } if why == "over the talk-time limit")));
    assert_eq!(rig.show.status(secs(30_000)).phase, Phase::Idle);
}

#[test]
fn a_reply_the_spending_cap_cannot_cover_is_cancelled_and_automatic_replies_stop() {
    // Less than one reply's bound.
    let mut rig = Rig::new(
        |d| {
            d.says(secs(0), secs(1_000), "Hello?").answer(reply_events("r1", "Hi there! ", 1.0, usage())).says(
                secs(4_000),
                secs(5_000),
                "Mochi?",
            )
        },
        Respond::Always,
        MicroUsd(1_000),
    );
    rig.run_to(7_000);
    assert_eq!(rig.played(), Vec::<Played>::new());
    assert_eq!(rig.cancels().len(), 1);
    assert!(rig.has(|e| matches!(e, PerformerEvent::Filtered { what, .. } if what == "reply")));
    assert!(rig.duplex.calls().contains(&DuplexCall::Auto(secs(1_300), false)), "{:?}", rig.duplex.calls());
    // Even named, it doesn't ask for another.
    assert_eq!(rig.replies(), Vec::<Option<String>>::new());
    assert!(rig.has(|e| matches!(e, PerformerEvent::Heard { addressed: Some(a), .. } if a.contains("spending cap"))));
    assert_eq!(rig.wallet.charges().len(), 0);
}

#[test]
fn a_private_line_gets_a_private_reply_with_no_text_in_the_transcript() {
    let mut rig = Rig::new(
        |d| {
            d.script(secs(0), DuplexEvent::SpeechStarted)
                .script(secs(1_000), DuplexEvent::SpeechStopped)
                .script(secs(1_100), DuplexEvent::heard_private("Mochi, is my pacing okay?"))
                .answer(reply_events("r1", "Your pacing is fine. ", 1.0, usage()))
        },
        Respond::Addressed,
        CAP,
    );
    rig.run_to(4_000);
    assert_eq!(rig.replies(), [None]);
    let played = rig.played();
    assert!(!played.is_empty() && played.iter().all(|p| p.private));
    assert!(rig.has(|e| matches!(e, PerformerEvent::Heard { text: None, private: true, .. })));
    assert!(rig.has(|e| matches!(e, PerformerEvent::Thinking { private: true, .. })));
    assert!(rig.has(|e| matches!(e, PerformerEvent::Speaking { text: None, private: true, .. })));
    assert_eq!(rig.said(), Vec::<String>::new());
    assert!(rig.faces().is_empty(), "a private turn shows nothing on stage");
}

#[test]
fn say_acts_its_tags_and_asks_the_model_to_speak_the_line() {
    let mut rig =
        Rig::new(|d| d.answer(reply_events("r1", "Welcome back, everyone! ", 1.0, usage())), Respond::Addressed, CAP);
    rig.run_to(100);
    rig.show.say(secs(100), "[emote happy] Welcome back, everyone!");
    rig.lines.extend(rig.show.drain().into_iter().map(|l| (l.t, l.event)));
    rig.run_to(2_000);
    assert_eq!(rig.faces().first().map(String::as_str), Some("happy"));
    let replies = rig.replies();
    assert_eq!(replies.len(), 1);
    let line = replies[0].clone().unwrap();
    assert!(line.ends_with(": Welcome back, everyone!") && !line.contains('['), "{line}");
    assert_eq!(rig.said(), ["Welcome back, everyone!"]);
}

#[test]
fn stop_cuts_the_reply_and_closes_the_session() {
    let mut rig = Rig::new(
        |d| {
            d.says(secs(0), secs(1_000), "Tell me a story.")
                .answer(reply_events("r1", "Once upon a time there was a crane. ", 3.0, usage()))
                .says(secs(5_000), secs(6_000), "Hello?")
        },
        Respond::Always,
        CAP,
    );
    // Stopped while the reply still streams.
    rig.run_to(1_600);
    rig.show.stop(secs(1_600));
    rig.run_to(7_000);
    let calls = rig.duplex.calls();
    assert!(matches!(calls.last(), Some(DuplexCall::Close(_))), "{calls:?}");
    assert!(calls.contains(&DuplexCall::Auto(secs(1_600), false)));
    assert_eq!(rig.cancels().len(), 1);
    let end = rig.last_sound();
    assert!(end <= 1.6 + 0.0051, "audio ran to {end}");
    // Nothing after the stop: the later line is never heard.
    assert!(!rig.has(|e| matches!(e, PerformerEvent::Heard { text: Some(t), .. } if t == "Hello?")));
    // The bill never came, so the reply's whole bound was charged.
    assert_eq!(rig.wallet.charges().len(), 1);
    assert_eq!(rig.show.status(secs(7_000)).phase, Phase::Idle);
}
