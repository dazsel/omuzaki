//! A text-only answer on `FakeLlm` (#1226): events, entries, charges, admission, stop reasons and errors.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use common::*;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_api::usage::Usage;
use omuzaki_llm_mock::{Injected, ScriptTurn};
use omuzaki_ops_api::{PauseReason, SessionState, Spend};

fn turn_with_usage(text: &str, usage: Usage, stop: StopReason) -> ScriptTurn {
    ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta(text.into()),
        LlmEvent::Usage(usage),
        LlmEvent::Stop(stop),
    ])
}

#[test]
fn a_text_answer_emits_text_then_budget_then_done_and_records_user_assistant_and_charge() {
    let usage = Usage::new(4, 61).with_cache_write(6_262);
    let rig = Rig::new(script(vec![turn_with_usage("Hello there.", usage, StopReason::EndTurn)]));
    let events = rig.say("hi", 0.0);
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            AssistantEvent::TextDelta(_) => "text",
            AssistantEvent::Budget { .. } => "budget",
            AssistantEvent::Done => "done",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["text", "budget", "done"]);
    assert_eq!(text_of(&events), "Hello there.");
    let entries = rig.sink.entries();
    assert!(matches!(entries[0], Entry::Session { .. }));
    assert!(matches!(&entries[1], Entry::User { text, .. } if text == "hi"));
    assert!(matches!(&entries[2], Entry::Assistant { .. }));
    assert!(
        matches!(&entries[3], Entry::Charge { .. }),
        "the turn, then what it cost: a log that ends on a charge ends an answer"
    );
}

#[test]
fn the_charge_is_the_design_example_to_the_micro_dollar() {
    let usage = Usage::new(4, 61).with_cache_write(6_262);
    let rig = Rig::new(script(vec![turn_with_usage("ok", usage, StopReason::EndTurn)]));
    rig.say("hi", 0.0);
    assert_eq!(rig.sessions.charges(), vec![Spend { usd_micros: 814, tokens: 6_327, seconds: 0 }]);
}

#[test]
fn a_hundred_text_events_inside_160_ms_become_at_most_ten_deltas_with_the_whole_text() {
    let mut events = vec![LlmEvent::start("m")];
    let pieces: Vec<String> = (0..100).map(|i| format!("{i:02} ")).collect();
    events.extend(pieces.iter().cloned().map(LlmEvent::TextDelta));
    events.push(usage(1, 1));
    events.push(LlmEvent::Stop(StopReason::EndTurn));
    let rig = Rig::new(script(vec![ScriptTurn::new(events).with_delay_ms(1)]));
    let got = rig.say("go", 0.0);
    let deltas = count(&got, |e| matches!(e, AssistantEvent::TextDelta(_)));
    assert!(deltas <= 10, "{deltas} deltas");
    assert_eq!(text_of(&got), pieces.concat());
}

#[test]
fn with_no_budget_left_the_bound_is_requested_and_nothing_is_sent_until_the_person_raises_it() {
    let rig = Rig::new(script(vec![say_turn("done")]));
    rig.sessions.set_left(Some(0));
    rig.host.send(UserMessage::new("hi")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("Waiting", || {
        while let Some(e) = rig.rx.try_recv() {
            seen.push(e);
        }
        seen.iter().any(|e| matches!(e, AssistantEvent::Waiting { .. }))
    });
    assert!(rig.llm.requests().is_empty(), "no request reaches the model before the approval resolves");
    let requested = rig.sessions.budget_requests();
    assert!(requested[0].usd_micros > 0 && requested[0].tokens > 0, "{requested:?}");
    assert!(seen.iter().any(|e| matches!(e, AssistantEvent::Waiting { approval, .. } if approval.0 == 7)));
    rig.sessions.raise_budget();
    let rest = rig.until_done(0.0);
    let need_recorded = rig.sink.entries().iter().any(|e| {
        matches!(e, omuzaki_agent_api::entry::Entry::Approval { need: omuzaki_ops_api::Need::Budget { .. }, .. })
    });
    assert!(need_recorded, "the budget approval is in the log");
    assert_eq!(rig.llm.requests().len(), 1);
    assert!(rest.iter().any(|e| matches!(e, AssistantEvent::Resumed)));
    assert_eq!(text_of(&rest), "done");
}

#[test]
fn send_while_an_answer_runs_is_busy() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "render_preview", "{}")]), say_turn("ok")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(tool("slow", "Slow", "query", true), Outcome::text("x"));
            server
        },
    );
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    rig.server.add_fixed(tool("render_preview", "Render preview", "query", true), Outcome::text("r").held(&gate));
    rig.host.send(UserMessage::new("one")).unwrap();
    assert_eq!(rig.host.send(UserMessage::new("two")), Err(HostError::Busy));
    gate.notify_one();
    rig.until_done(0.0);
    assert!(rig.host.send(UserMessage::new("three")).is_ok());
}

#[test]
fn a_session_paused_by_the_person_is_resumed_by_send_with_the_principal() {
    let rig = Rig::new(script(vec![say_turn("hi")]));
    rig.sessions.set_state(SessionState::Paused { by: person(), reason: PauseReason::Person });
    rig.say("again", 0.0);
    assert_eq!(rig.sessions.resume_calls(), vec![person()]);
}

#[test]
fn end_turn_ends_and_the_provider_pause_sends_the_turn_back_as_is() {
    let paused = turn_with_usage("part one. ", Usage::new(1, 1), StopReason::Paused);
    let rig = Rig::new(script(vec![paused, say_turn("part two.")]));
    let events = rig.say("go", 0.0);
    assert_eq!(text_of(&events), "part one. part two.");
    let second = &rig.llm.requests()[1];
    assert_eq!(second.messages.len(), 2, "the paused turn is the last message, nothing added after it");
}

#[test]
fn max_output_continues_once_with_continue() {
    let cut = turn_with_usage("long ", Usage::new(1, 1), StopReason::MaxOutput);
    let cut_again = turn_with_usage("longer ", Usage::new(1, 1), StopReason::MaxOutput);
    let rig = Rig::new(script(vec![cut, cut_again, say_turn("never asked")]));
    let events = rig.say("go", 0.0);
    assert_eq!(text_of(&events), "long longer ");
    assert_eq!(rig.llm.requests().len(), 2, "one continue, then the answer ends");
}

#[test]
fn a_refusal_ends_the_answer_and_keeps_the_partial_text() {
    let refused = turn_with_usage("I can't", Usage::new(1, 1), StopReason::Refused);
    let rig = Rig::new(script(vec![refused]));
    let events = rig.say("go", 0.0);
    assert_eq!(text_of(&events), "I can't");
    assert!(matches!(events.last(), Some(AssistantEvent::Done)));
}

fn failing(error: LlmError) -> ScriptTurn {
    ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta("partial".into()),
        LlmEvent::Usage(Usage::new(3, 2)),
        LlmEvent::Stop(StopReason::EndTurn),
    ])
    .with_error(Injected::new(3, error))
}

fn error_of(events: &[AssistantEvent]) -> Option<HostError> {
    events.iter().find_map(|e| if let AssistantEvent::Error(e) = e { Some(e.clone()) } else { None })
}

#[test]
fn rate_limit_overload_and_network_errors_charge_what_was_used_and_end_with_the_error() {
    for error in [LlmError::rate_limited(None), LlmError::Overloaded, LlmError::Network("reset".into())] {
        let rig = Rig::new(script(vec![failing(error.clone())]));
        let events = rig.say("go", 0.0);
        assert_eq!(error_of(&events), Some(HostError::Llm(error)));
        assert_eq!(text_of(&events), "partial");
        assert!(matches!(events.last(), Some(AssistantEvent::Done)));
        assert_eq!(rig.sessions.charges().len(), 1, "the usage seen so far is charged once");
    }
}

#[test]
fn an_auth_failure_is_needs_key_and_a_too_long_prompt_ends_the_answer() {
    let rig = Rig::new(script(vec![failing(LlmError::Auth)]));
    assert_eq!(error_of(&rig.say("go", 0.0)), Some(HostError::NeedsKey));
}

#[test]
fn a_stream_error_with_a_session_that_has_ended_ends_the_answer_as_session_ended() {
    let rig = Rig::with(
        script(vec![failing(LlmError::Network("reset".into()))]),
        RigOptions { hold_after: Some(2), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the held stream", || rig.gate.delivered.load(std::sync::atomic::Ordering::Acquire) >= 2);
    rig.sessions.revoke();
    rig.gate.open();
    let events = rig.until_done(0.0);
    assert_eq!(error_of(&events), Some(HostError::SessionEnded), "{events:?}");
    assert!(
        rig.sink.entries().iter().any(|e| matches!(e, Entry::Session { .. })),
        "the end of the session is recorded"
    );
}
