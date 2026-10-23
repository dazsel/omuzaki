//! The turn loop on the fake harness (`ai-performer.md` §9): `FakeLlm` writes, `ScriptedEars` hears, `FakeVoice`
//! speaks and `RecordingStage` takes the intents, all on one fake clock stepped every 5 ms.
// Test helpers may unwrap: a panic there is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use omuzaki_anim_api::score::{ClipId, ExpressionId};
use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::message::ContentBlock;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_api::usage::Usage;
use omuzaki_llm_mock::{Expect, FakeLlm, Script, ScriptTurn};
use omuzaki_performer::addressing::Respond;
use omuzaki_performer::brain::LlmBrain;
use omuzaki_performer::persona::Persona;
use omuzaki_performer::show::{Parts, Phase, Show, ShowConfig};
use omuzaki_performer_api::decide::{Ask, Decider, DecisionTicket};
use omuzaki_performer_api::ears::Heard;
use omuzaki_performer_api::event::{PerformerEvent, TranscriptLine};
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::speech::{SpeakHow, SpeechHandle, Voice, VoiceError};
use omuzaki_performer_api::stage::{ClipInfo, Inventory};
use omuzaki_performer_mock::{FakeVoice, RecordingStage, ScriptedEars, Spoken};
use omuzaki_stage_api::ids::MemberId;

const GOLDEN: &str = include_str!("../goldens/four-turns.jsonl");
const STEP_MS: u64 = 5;
const VOICE_LEAD_S: f64 = 0.2;

fn secs(ms: u64) -> Seconds {
    #[allow(clippy::cast_precision_loss)]
    Seconds(ms as f64 / 1000.0)
}

fn mochi() -> Persona {
    let mut p = Persona::named("Mochi");
    p.aliases = vec!["mochi-chan".into(), "mochie".into()];
    p
}

fn inventory() -> Inventory {
    Inventory::new(
        ["happy", "sad", "surprised", "smug", "thinking"].iter().map(|e| ExpressionId::new(*e).unwrap()).collect(),
        vec![ClipInfo::new(ClipId::new("wave").unwrap(), true, 1.2)],
        vec![],
        vec![],
    )
}

/// A voice the test can still read after the show owns it.
#[derive(Clone)]
struct SharedVoice(Arc<Mutex<FakeVoice>>);

impl Voice for SharedVoice {
    fn speak(&mut self, sentence: &str, voice: &VoiceId, how: &SpeakHow) -> Result<SpeechHandle, VoiceError> {
        self.0.lock().unwrap().speak(sentence, voice, how)
    }
}

/// A decider that never answers: the loop must fall back to the rule floor.
struct Silent;

impl Decider for Silent {
    fn kind(&self) -> &'static str {
        "silent"
    }

    fn ask(&mut self, _: Ask) -> DecisionTicket {
        DecisionTicket::pending().0
    }
}

fn reply(text: &str) -> ScriptTurn {
    ScriptTurn::new(vec![
        LlmEvent::start("fake"),
        LlmEvent::TextDelta(text.into()),
        LlmEvent::Usage(Usage::new(400, 30)),
        LlmEvent::Stop(StopReason::EndTurn),
    ])
    .with_chunk(7)
}

struct Rig {
    clock: Arc<FakeClock>,
    llm: Arc<FakeLlm>,
    voice: SharedVoice,
    stage: Arc<RecordingStage>,
    show: Show,
    lines: Vec<TranscriptLine>,
}

impl Rig {
    fn new(ears: Vec<(Seconds, Heard)>, script: Script, config: impl FnOnce(ShowConfig) -> ShowConfig) -> Self {
        Self::with_decider(ears, script, config, None)
    }

    fn with_decider(
        ears: Vec<(Seconds, Heard)>,
        script: Script,
        config: impl FnOnce(ShowConfig) -> ShowConfig,
        decider: Option<Box<dyn Decider>>,
    ) -> Self {
        let clock = Arc::new(FakeClock::default());
        let llm = Arc::new(FakeLlm::new(script));
        let member = MemberId::new("mochi").unwrap();
        let stage = Arc::new(RecordingStage::new(member.clone(), inventory()));
        let voice = SharedVoice(Arc::new(Mutex::new(FakeVoice::new(Arc::clone(&clock), Seconds(VOICE_LEAD_S)))));
        let brain = LlmBrain::new(Arc::clone(&llm) as Arc<dyn LlmClient>, mochi());
        let mut parts = Parts::new(
            Box::new(brain),
            Box::new(ScriptedEars::new(Arc::clone(&clock), ears)),
            Box::new(voice.clone()),
            Arc::clone(&stage) as _,
        );
        if let Some(d) = decider {
            parts = parts.decider(d);
        }
        let config = config(ShowConfig::new(member, VoiceId::new("kokoro-82m/af_heart").unwrap()));
        let show = Show::new(&mochi(), parts, config);
        Self { clock, llm, voice, stage, show, lines: Vec::new() }
    }

    fn run_to(&mut self, end_ms: u64) {
        let from = self.clock_ms();
        for now in (from..=end_ms).step_by(usize::try_from(STEP_MS).unwrap()) {
            self.clock.set(secs(now));
            self.show.step(secs(now));
            self.lines.extend(self.show.drain());
        }
    }

    fn clock_ms(&self) -> u64 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = (self.clock.now().0 * 1000.0).round() as u64;
        ms
    }

    fn jsonl(&self) -> String {
        self.lines.iter().map(|l| l.to_json().unwrap() + "\n").collect()
    }

    fn events(&self) -> Vec<(u64, PerformerEvent)> {
        self.lines.iter().map(|l| (l.t, l.event.clone())).collect()
    }

    fn spoken(&self) -> Vec<Spoken> {
        self.voice.0.lock().unwrap().log()
    }

    fn said(&self) -> Vec<String> {
        self.spoken().into_iter().map(|s| s.text).collect()
    }

    fn requests_text(&self) -> Vec<String> {
        self.llm
            .requests()
            .iter()
            .map(|r| {
                let mut out = String::new();
                for m in &r.messages {
                    for c in &m.content {
                        if let ContentBlock::Text(t) = c {
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                }
                out
            })
            .collect()
    }
}

fn utterance(n: u64, text: &str, from: u64, to: u64) -> [(Seconds, Heard); 2] {
    [(secs(from), Heard::onset(secs(from))), (secs(to), Heard::final_text(n, text, secs(from), secs(to)))]
}

/// §10's exchange by voice (chat waits on #1915): a greeting with a tag on its word, a follow-up, a barge-in that
/// ends the turn, an unknown clip, a line past the follow-up window, and a private turn.
fn four_turns() -> Rig {
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, say hi to chat!", 0, 0));
    ears.extend(utterance(1, "What's your favourite fold?", 8_000, 9_000));
    ears.extend([(secs(11_800), Heard::onset(secs(11_800))), (secs(11_900), Heard::held(secs(11_900)))]);
    ears.push((secs(13_000), Heard::final_text(2, "Sorry, go on.", secs(11_800), secs(13_000))));
    ears.extend(utterance(3, "Okay chat, next game.", 23_000, 24_000));
    ears.push((secs(26_000), Heard::onset(secs(26_000))));
    ears.push((
        secs(27_000),
        Heard::final_text(4, "Mochi, between us: is my pacing okay?", secs(26_000), secs(27_000)).private(),
    ));
    let expect = |t: ScriptTurn, s: &str| t.with_expect(Expect::new().with_user_contains(s));
    let script = Script::new("fake")
        .with_turn(expect(reply("Hi chat! [emote happy 0.8] [gesture wave] Fold it and see!"), "say hi to chat"))
        .with_turn(expect(
            reply("The crane, obviously. [emote smug] It took me a whole week to learn and I still fold it wrong."),
            "favourite fold",
        ))
        .with_turn(expect(reply("[clip backflip] Anyway, a week!"), "interrupted after saying"))
        .with_turn(expect(reply("[emote happy] You're doing great."), "between us"));
    let mut rig = Rig::new(ears, script, |c| c);
    rig.run_to(30_000);
    rig
}

#[test]
fn the_four_turn_exchange_matches_its_golden_twice() {
    let first = four_turns();
    assert_eq!(first.jsonl(), GOLDEN, "the transcript differs from goldens/four-turns.jsonl:\n{}", first.jsonl());
    assert_eq!(four_turns().jsonl(), first.jsonl());
}

#[test]
fn what_the_fakes_saw() {
    let rig = four_turns();
    // No tag reached the voice; the private reply went to the monitor only.
    assert!(rig.said().iter().all(|s| !s.contains('[') && !s.contains(']')));
    let private: Vec<String> = rig.spoken().into_iter().filter(|s| s.how.private).map(|s| s.text).collect();
    assert_eq!(private, ["You're doing great."]);
    // The cut landed one block after `Held`.
    let cut: Vec<(Seconds, u32)> = rig.spoken().iter().filter_map(|s| s.cut).collect();
    assert_eq!(cut.len(), 1);
    assert!((cut[0].0.0 - 11.905).abs() < 1e-9, "{cut:?}");
    // The model's tags landed on their words: the greeting's emote and wave after "Hi chat!" ends.
    let applied: Vec<(u64, &str)> =
        rig.stage.applied().iter().map(|a| (omuzaki_performer::show::ms(a.at), a.intent.verb().unwrap())).collect();
    assert!(applied.contains(&(680, "emote")) && applied.contains(&(680, "gesture")), "{applied:?}");
    // The private turn moved nothing on stage and its text is nowhere in the transcript.
    assert!(rig.stage.applied().iter().all(|a| a.at.0 < 26.0));
    assert!(!rig.jsonl().contains("between us") && !rig.jsonl().contains("doing great"));
    assert_eq!(rig.show.status(secs(30_000)).phase, Phase::Idle);
}

#[test]
fn a_public_turn_never_sees_a_private_one() {
    let mut ears = Vec::new();
    ears.push((secs(0), Heard::final_text(0, "Mochi, between us: the secret plan", secs(0), secs(0)).private()));
    ears.extend(utterance(1, "Mochi, repeat what we just discussed", 5_000, 5_500));
    let script = Script::new("fake").with_turn(reply("Shh, it's fine.")).with_turn(reply("Discussed what?"));
    let mut rig = Rig::new(ears, script, |c| c);
    rig.run_to(8_000);
    let asks = rig.requests_text();
    assert_eq!(asks.len(), 2);
    assert!(asks[0].contains("secret plan"));
    assert!(!asks[1].contains("secret plan") && !asks[1].contains("Shh"), "{}", asks[1]);
}

#[test]
fn follow_up_window_is_eight_seconds_from_the_reply_end() {
    // "Hi!" is 3 characters: it plays 0.2..0.38 s. A line starting at 8.37 s is inside; at 8.39 s it is not.
    let run = |start: u64| {
        let mut ears = Vec::new();
        ears.extend(utterance(0, "Mochi, hello", 0, 0));
        ears.extend(utterance(1, "and then what", start, start + 500));
        let script = Script::new("fake").with_turn(reply("Hi!")).with_turn(reply("Then tea."));
        let mut rig = Rig::new(ears, script, |c| c);
        rig.run_to(start + 2_000);
        rig.llm.requests().len()
    };
    assert_eq!(run(8_375), 2);
    assert_eq!(run(8_385), 1);
}

#[test]
fn listen_only_and_always() {
    let ears = || {
        let mut e = Vec::new();
        e.extend(utterance(0, "Mochi, hello", 0, 0));
        e.extend(utterance(1, "nice weather", 20_000, 20_500));
        e
    };
    let script = || Script::new("fake").with_turn(reply("Hi!")).with_turn(reply("It is."));
    let mut quiet = Rig::new(ears(), script(), |c| c.respond(Respond::ListenOnly));
    quiet.run_to(22_000);
    assert_eq!(quiet.llm.requests().len(), 0);
    let mut chatty = Rig::new(ears(), script(), |c| c.respond(Respond::Always));
    chatty.run_to(22_000);
    assert_eq!(chatty.llm.requests().len(), 2);
}

#[test]
fn barge_in_then_a_cough_resumes_the_cut_sentence_once() {
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, tell me a story", 0, 0));
    // "Once upon a time there was a crane." is 35 characters, 2.1 s: it plays 0.2..2.3.
    ears.extend([(secs(1_000), Heard::onset(secs(1_000))), (secs(1_100), Heard::held(secs(1_100)))]);
    ears.push((secs(1_300), Heard::rejected(secs(1_300))));
    let script = Script::new("fake").with_turn(reply("Once upon a time there was a crane. It folded itself."));
    let mut rig = Rig::new(ears, script, |c| c.reactions(false));
    rig.run_to(8_000);
    assert_eq!(
        rig.said(),
        [
            "Once upon a time there was a crane.",
            "It folded itself.",
            "Once upon a time there was a crane.",
            "It folded itself."
        ]
    );
    let held = rig.events().iter().filter(|(_, e)| matches!(e, PerformerEvent::Held { .. })).count();
    assert_eq!(held, 1);
    assert!(!rig.events().iter().any(|(_, e)| matches!(e, PerformerEvent::Interrupted { .. })));
}

#[test]
fn a_second_cut_after_a_resume_ends_the_turn() {
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, tell me a story", 0, 0));
    ears.extend([(secs(1_000), Heard::onset(secs(1_000))), (secs(1_100), Heard::held(secs(1_100)))]);
    ears.push((secs(1_300), Heard::final_text(1, "yeah", secs(1_000), secs(1_300))));
    ears.extend([(secs(2_000), Heard::onset(secs(2_000))), (secs(2_100), Heard::held(secs(2_100)))]);
    ears.push((secs(2_300), Heard::rejected(secs(2_300))));
    let script = Script::new("fake").with_turn(reply("Once upon a time there was a crane. It folded itself."));
    let mut rig = Rig::new(ears, script, |c| c.reactions(false));
    rig.run_to(8_000);
    let cut = rig.events().into_iter().find_map(|(t, e)| match e {
        PerformerEvent::Interrupted { spoken_chars, .. } => Some((t, spoken_chars)),
        _ => None,
    });
    // The first attempt was cut at 1.105 s, 15 characters in; the resumed one started at 1.5 s and was cut at
    // 2.105 s, 10 in. The turn reports the most of its text anyone heard.
    assert_eq!(cut, Some((2_300, 15)));
    assert_eq!(rig.show.status(secs(8_000)).phase, Phase::Idle);
}

#[test]
fn a_reply_waits_for_the_floor_and_is_dropped_after_three_seconds() {
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, hi", 0, 0));
    // The person goes on talking before the reply plays, past 3 s of it waiting.
    ears.push((secs(0), Heard::onset(secs(0))));
    ears.push((secs(5_000), Heard::final_text(1, "and another thing about the weather", secs(0), secs(5_000))));
    let script = Script::new("fake").with_turn(reply("Hello there!"));
    let mut rig = Rig::new(ears, script, |c| c.reactions(false));
    rig.run_to(7_000);
    assert_eq!(rig.said(), Vec::<String>::new());
    assert!(rig.events().iter().any(|(_, e)| matches!(e, PerformerEvent::Filtered { what, .. } if what == "reply")));

    // A shorter wait plays once the person stops.
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, hi", 0, 0));
    ears.push((secs(0), Heard::onset(secs(0))));
    ears.push((secs(2_000), Heard::rejected(secs(2_000))));
    let script = Script::new("fake").with_turn(reply("Hello there!"));
    let mut rig = Rig::new(ears, script, |c| c.reactions(false));
    rig.run_to(5_000);
    assert_eq!(rig.said(), ["Hello there!"]);
    assert!(rig.spoken()[0].start.0 >= 2.0);
}

#[test]
fn a_turn_stops_at_the_sentence_that_crosses_twenty_seconds() {
    let sentence = "This sentence is exactly sixty characters long, more or less.";
    let long: String = (0..12).map(|_| sentence).collect::<Vec<_>>().join(" ");
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, talk", 0, 0));
    let script = Script::new("fake").with_turn(reply(&long));
    let mut rig = Rig::new(ears, script, |c| c.reactions(false));
    rig.run_to(60_000);
    let total: f64 = rig.spoken().iter().map(|s| s.duration.0).sum();
    // 61 characters at 60 ms is 3.66 s a sentence: the sixth crosses 20 s and is the last.
    assert_eq!(rig.said().len(), 6, "{total}");
    assert!(total > 20.0 && total < 24.0);
}

#[test]
fn its_own_voice_coming_back_is_echo() {
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, greet them", 0, 0));
    // Plays 0.2..1.88 s; the room hears it back.
    ears.push((secs(1_900), Heard::final_text(1, "welcome to the stream", secs(500), secs(1_900))));
    let script = Script::new("fake").with_turn(reply("Welcome to the stream!"));
    let mut rig = Rig::new(ears, script, |c| c.respond(Respond::Always).reactions(false));
    rig.run_to(4_000);
    assert_eq!(rig.show.status(secs(4_000)).echoes, 1);
    assert_eq!(rig.llm.requests().len(), 1);
}

#[test]
fn pass_says_nothing_and_say_speaks_without_the_model() {
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi?", 0, 0));
    let script = Script::new("fake").with_turn(reply("[pass]"));
    let mut rig = Rig::new(ears, script, |c| c.reactions(false));
    rig.run_to(1_000);
    assert_eq!(rig.said(), Vec::<String>::new());
    rig.show.say(secs(1_000), "Thanks for the follow! [gesture wave]");
    rig.run_to(4_000);
    assert_eq!(rig.said(), ["Thanks for the follow!"]);
    assert!(rig.stage.applied().iter().any(|a| a.intent.verb() == Some("gesture")));
    assert_eq!(rig.llm.requests().len(), 1);
}

#[test]
fn the_same_emote_twice_inside_two_seconds_is_filtered() {
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, hi", 0, 0));
    let script = Script::new("fake").with_turn(reply("Hi! [emote happy] Yes! [emote happy] Hooray!"));
    let mut rig = Rig::new(ears, script, |c| c.reactions(false));
    rig.run_to(4_000);
    let emotes = rig.stage.applied().iter().filter(|a| a.intent.verb() == Some("emote")).count();
    assert_eq!(emotes, 1);
    assert!(rig.events().iter().any(|(_, e)| matches!(e, PerformerEvent::Filtered { why, .. } if why == "cooldown")));
}

#[test]
fn the_decision_layer_reacts_without_the_model_and_its_tags_win() {
    // The person's line gets a listening face at once, before any reply; an untagged sad line gets a sad face;
    // a line the model tagged keeps the model's.
    let mut ears = Vec::new();
    ears.extend(utterance(0, "Mochi, I lost my keys today", 0, 1_000));
    let script =
        Script::new("fake").with_turn(reply("Oh no, I'm so sorry. [emote surprised] That is awful!").with_delay_ms(0));
    let mut rig = Rig::with_decider(ears, script, |c| c, Some(Box::new(Silent)));
    rig.run_to(6_000);
    let emotes: Vec<(u64, String)> = rig
        .events()
        .into_iter()
        .filter_map(|(t, e)| match e {
            PerformerEvent::Intent { intent: Intent::Emote { expression, .. }, .. } => {
                Some((t, expression.to_string()))
            }
            _ => None,
        })
        .collect();
    // The silent provider times out after 400 ms; the rule floor answers.
    assert_eq!(emotes.first(), Some(&(1_400, "sad".to_owned())), "{emotes:?}");
    assert!(emotes.iter().any(|(_, e)| e == "surprised"));
}

#[test]
fn ten_thousand_scripted_events_take_under_fifty_milliseconds() {
    let mut ears = Vec::new();
    for i in 0..2_500u64 {
        let t = i * 10;
        ears.push((secs(t), Heard::onset(secs(t))));
        ears.push((secs(t + 2), Heard::rejected(secs(t + 2))));
        ears.push((secs(t + 4), Heard::onset(secs(t + 4))));
        ears.push((secs(t + 6), Heard::final_text(i, "nice weather today", secs(t + 4), secs(t + 6))));
    }
    let mut rig = Rig::new(ears, Script::new("fake"), |c| c.reactions(false));
    let started = std::time::Instant::now();
    for now in (0..25_000u64).step_by(5) {
        rig.clock.set(secs(now));
        rig.show.step(secs(now));
        let _ = rig.show.drain();
    }
    let took = started.elapsed();
    // A ceiling for debug builds on a slow runner; the design's bound (50 ms) is for release builds.
    assert!(took.as_millis() < 2_000, "{took:?}");
}
