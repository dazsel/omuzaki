//! Listening with the face (#2453): while the person talks, the performer reacts to their partial words about once a
//! second and nods on the backchannel cues of their voice, on its own character, never while it speaks itself, and
//! never from private words. On the fake harness: `ScriptedEars`, `FakeVoice` and `RecordingStage`, one fake clock
//! stepped every 5 ms.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_anim_api::score::ExpressionId;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_mock::{FakeLlm, Script};
use omuzaki_performer::brain::LlmBrain;
use omuzaki_performer::persona::Persona;
use omuzaki_performer::show::{Parts, Show, ShowConfig};
use omuzaki_performer_api::ears::Heard;
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::limits::{BACKCHANNEL_NOD, REACTION_APEX};
use omuzaki_performer_api::stage::Inventory;
use omuzaki_performer_mock::{Applied, FakeVoice, RecordingStage, ScriptedEars};
use omuzaki_stage_api::ids::MemberId;

const STEP_MS: u64 = 5;

fn secs(ms: u64) -> Seconds {
    #[allow(clippy::cast_precision_loss)]
    Seconds(ms as f64 / 1000.0)
}

fn inventory() -> Inventory {
    Inventory::new(
        ["happy", "sad", "surprised", "thinking"].iter().map(|e| ExpressionId::new(*e).unwrap()).collect(),
        vec![],
        vec![],
        vec![],
    )
}

/// Runs a show over `heard` to `end_ms` and returns what reached the stage.
fn run(heard: Vec<(Seconds, Heard)>, end_ms: u64) -> Vec<Applied> {
    let clock = Arc::new(FakeClock::default());
    let llm = Arc::new(FakeLlm::new(Script::new("fake")));
    let member = MemberId::new("mochi").unwrap();
    let stage = Arc::new(RecordingStage::new(member.clone(), inventory()));
    let parts = Parts::new(
        Box::new(LlmBrain::new(llm as Arc<dyn LlmClient>, Persona::named("Mochi"))),
        Box::new(ScriptedEars::new(Arc::clone(&clock), heard)),
        Box::new(FakeVoice::new(Arc::clone(&clock), Seconds(0.2))),
        Arc::clone(&stage) as _,
    );
    let mut show = Show::new(
        &Persona::named("Mochi"),
        parts,
        ShowConfig::new(member, VoiceId::new("kokoro-82m/af_heart").unwrap()),
    );
    for now in (0..=end_ms).step_by(usize::try_from(STEP_MS).unwrap()) {
        clock.set(secs(now));
        show.step(secs(now));
        let _ = show.drain();
    }
    stage.applied()
}

fn faces(applied: &[Applied]) -> Vec<(f64, String, f32)> {
    applied
        .iter()
        .filter_map(|a| match &a.intent {
            Intent::Emote { expression, intensity, .. } => Some((a.at.0, expression.to_string(), *intensity)),
            _ => None,
        })
        .collect()
}

fn nods(applied: &[Applied]) -> Vec<(f64, f32)> {
    applied
        .iter()
        .filter_map(|a| match &a.intent {
            Intent::Nod { strength, .. } => Some((a.at.0, *strength)),
            _ => None,
        })
        .collect()
}

/// A person who talks for six seconds about a happy day, with partials every 400 ms and cues every 1.5 s, then
/// stops. Nobody names the performer, so it never takes a turn: everything it does is listening.
fn monologue(private: bool) -> Vec<(Seconds, Heard)> {
    let words =
        ["So today", "So today I finally", "So today I finally won", "So today I finally won the cup, I'm so happy"];
    let mut out = vec![(secs(500), Heard::onset(secs(500))), (secs(600), Heard::held(secs(600)))];
    for i in 0..14u64 {
        let at = 900 + i * 400;
        let text = words[usize::try_from(i).unwrap().min(words.len() - 1)];
        let partial = Heard::partial(0, text, secs(at));
        out.push((secs(at), if private { partial.private() } else { partial }));
    }
    for i in 0..4u64 {
        let at = 1200 + i * 1500;
        out.push((secs(at), Heard::backchannel(secs(at))));
    }
    let end = Heard::final_text(0, words[words.len() - 1], secs(500), secs(6600));
    out.push((secs(6700), if private { end.private() } else { end }));
    out.sort_by(|a, b| a.0.0.total_cmp(&b.0.0));
    out
}

#[test]
fn it_reacts_to_partial_words_about_once_a_second_while_the_person_talks() {
    let applied = run(monologue(false), 8000);
    let faces = faces(&applied);
    println!("faces: {faces:?}");
    let while_talking: Vec<_> = faces.iter().filter(|f| f.0 < 6.6).collect();
    assert!(while_talking.len() >= 2, "{faces:?}");
    // The first face comes on the first partial's step: the rule floor answers at once.
    assert!((while_talking[0].0 - 0.9).abs() < 0.011, "{faces:?}");
    // The happy words reach the face before the person has finished.
    assert!(while_talking.iter().any(|f| f.1 == "happy"), "{faces:?}");
    // At most one listening face a second, and every one under the apex cap.
    for pair in while_talking.windows(2) {
        assert!(pair[1].0 - pair[0].0 >= 0.999, "{faces:?}");
    }
    assert!(faces.iter().all(|f| f.2 <= REACTION_APEX + 1e-6), "{faces:?}");
}

#[test]
fn it_nods_on_backchannel_cues_while_the_person_talks() {
    let applied = run(monologue(false), 8000);
    let nods = nods(&applied);
    println!("nods: {nods:?}");
    // Cues every 1.5 s, nods at most every `NOD_GAP_S` (2 s): the first, third and so on, on the cue's own step.
    let at: Vec<f64> = nods.iter().map(|n| n.0).collect();
    assert_eq!(at.len(), 2, "{nods:?}");
    assert!((at[0] - 1.2).abs() < 0.011 && (at[1] - 4.2).abs() < 0.011, "{nods:?}");
    assert!(nods.iter().all(|n| (n.1 - BACKCHANNEL_NOD).abs() < 1e-6));
}

#[test]
fn private_words_never_reach_the_listening_face() {
    let applied = run(monologue(true), 8000);
    let faces = faces(&applied);
    assert!(faces.iter().all(|f| f.0 >= 6.6), "a face before the private final: {faces:?}");
}

#[test]
fn cues_outside_speech_do_nothing() {
    // A partial and a cue with no utterance open (a stray event after the final).
    let heard = vec![(secs(100), Heard::partial(0, "hello", secs(100))), (secs(200), Heard::backchannel(secs(200)))];
    let applied = run(heard, 1000);
    assert!(applied.is_empty(), "{applied:?}");
}
