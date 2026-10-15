//! Stop (#1336, D4): no output after the click, what is kept, how fast `Done` comes, and what the executor is asked.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use std::sync::Arc;

use common::*;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_llm_api::message::ContentBlock;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_mock::ScriptTurn;
use omuzaki_ops_api::{PauseReason, SessionState, Sessions as _};

/// The events of a turn that calls one tool with a short text: start, text, the call's three, usage and stop.
const FIRST_TURN_EVENTS: usize = 7;

/// A turn of `n` text events of ten milliseconds each.
fn long_text(n: usize) -> ScriptTurn {
    let mut events = vec![LlmEvent::start("m")];
    events.extend((0..n).map(|i| LlmEvent::TextDelta(format!("w{i} "))));
    events.push(usage(5, 5));
    events.push(LlmEvent::Stop(StopReason::EndTurn));
    ScriptTurn::new(events).with_delay_ms(20)
}

fn assistant_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .filter_map(|e| if let Entry::Assistant { blocks, .. } = e { Some(blocks) } else { None })
        .flatten()
        .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
        .collect()
}

fn is_output(event: &AssistantEvent) -> bool {
    matches!(
        event,
        AssistantEvent::TextDelta(_)
            | AssistantEvent::ToolCall { .. }
            | AssistantEvent::Question { .. }
            | AssistantEvent::Plan(_)
    )
}

/// A small deterministic generator, so a failing interleaving can be replayed from its seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        usize::try_from((self.0 >> 33) % below as u64).unwrap_or(0)
    }
}

#[test]
fn after_stop_returns_nothing_new_is_readable_across_a_thousand_seeded_interleavings() {
    let mut rng = Lcg(0x5eed);
    for seed in 0..1_000 {
        let hold = 3 + rng.next(14);
        let rig = Rig::with(
            script(vec![long_text(20)]),
            RigOptions { hold_after: Some(hold), ..RigOptions::default() },
            standard_server,
        );
        rig.host.send(UserMessage::new("go")).unwrap();
        Rig::wait_for("the held stream", || rig.gate.delivered.load(std::sync::atomic::Ordering::Acquire) >= hold);
        // Let the host flush what it has, then read a seeded part of it.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let mut read = String::new();
        let queued_to_read = rng.next(hold);
        for _ in 0..queued_to_read {
            if let Some(AssistantEvent::TextDelta(t)) = rig.rx.try_recv() {
                read.push_str(&t);
            }
        }
        rig.host.stop();
        let after: Vec<AssistantEvent> = std::iter::from_fn(|| rig.rx.try_recv()).collect();
        assert!(after.iter().all(|e| !is_output(e)), "seed {seed}: output after stop: {after:?}");
        assert_eq!(assistant_text(&rig.host.transcript()), read, "seed {seed}: the transcript is what was read");
        let rest = rig.until_done(0.0);
        let all: Vec<_> = after.into_iter().chain(rest).collect();
        assert!(all.iter().any(|e| matches!(e, AssistantEvent::Paused)), "seed {seed}");
        assert!(matches!(all.last(), Some(AssistantEvent::Done)), "seed {seed}");
        assert_eq!(assistant_text(&rig.sink.entries()), read, "seed {seed}: the store agrees");
    }
}

#[test]
fn the_next_request_carries_only_the_text_that_was_read() {
    let rig = Rig::with(
        script(vec![long_text(20), say_turn("ok")]),
        RigOptions { hold_after: Some(6), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the held stream", || rig.gate.delivered.load(std::sync::atomic::Ordering::Acquire) >= 6);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let mut read = String::new();
    if let Some(AssistantEvent::TextDelta(t)) = rig.rx.try_recv() {
        read.push_str(&t);
    }
    rig.host.stop();
    rig.until_done(0.0);
    rig.gate.open();
    rig.say("again", 0.0);
    let history = &rig.llm.requests()[1].messages;
    let kept: String = history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
        .collect();
    assert!(kept.contains(&read) && !kept.contains("w19"), "{kept:?}");
}

#[test]
fn done_comes_with_no_clock_time_when_the_lock_is_free_and_after_the_hold_when_it_is_not() {
    for hold in [0.0, 0.2] {
        let rig = Rig::with(
            script(vec![long_text(20)]),
            RigOptions { hold_after: Some(5), ..RigOptions::default() },
            standard_server,
        );
        rig.sessions.set_stop(StopBehaviour::Committed, hold);
        rig.host.send(UserMessage::new("go")).unwrap();
        Rig::wait_for("the held stream", || rig.gate.delivered.load(std::sync::atomic::Ordering::Acquire) >= 5);
        let clicked = rig.clock.now().0;
        rig.host.stop();
        let mut done_at = None;
        let mut paused_seen = false;
        for _ in 0..20_000 {
            while let Some(event) = rig.rx.try_recv() {
                paused_seen |= matches!(event, AssistantEvent::Paused);
                if matches!(event, AssistantEvent::Done) {
                    done_at = Some(rig.clock.now().0);
                }
            }
            if done_at.is_some() {
                break;
            }
            rig.clock.advance(Seconds(0.01));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let waited = done_at.expect("Done") - clicked;
        assert!(paused_seen);
        assert!(waited <= hold + 0.1, "hold {hold}: Done after {waited} s of clock time");
        assert!(waited >= hold, "hold {hold}: Done after {waited} s");
    }
}

#[test]
fn a_pending_step_emits_paused_at_once_polls_every_50_ms_and_done_only_when_the_step_closes() {
    for seconds in [3_u32, 10] {
        let close_after = f64::from(seconds);
        let rig = Rig::with(
            script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), long_text(20)]),
            RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
            standard_server,
        );
        rig.sessions.set_stop(StopBehaviour::Pending { close_after }, 0.0);
        rig.host.send(UserMessage::new("go")).unwrap();
        // The edit opens the transaction; then the second turn streams and is held.
        Rig::wait_for("the transaction", || {
            rig.server.called("tx_begin") == 1 && rig.server.called("face_set_profile") == 1
        });
        rig.host.stop();
        let polls_before = rig.sessions.session_polls();
        let started = rig.clock.now().0;
        let mut events = Vec::new();
        let mut done_at = None;
        for _ in 0..40_000 {
            while let Some(event) = rig.rx.try_recv() {
                if matches!(event, AssistantEvent::Done) {
                    done_at = Some(rig.clock.now().0);
                }
                events.push(event);
            }
            if done_at.is_some() {
                break;
            }
            rig.clock.advance(Seconds(0.01));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let waited = done_at.expect("Done") - started;
        assert!(waited >= close_after, "Done after {waited} s, before the step closed at {close_after}");
        assert!(waited < close_after + 0.2, "Done after {waited} s");
        assert!(events.iter().any(|e| matches!(e, AssistantEvent::Paused)));
        let polls = rig.sessions.session_polls() - polls_before;
        let expected = usize::try_from(seconds * 20).unwrap();
        assert!(polls * 10 >= expected * 8 && polls <= expected + 25, "{polls} polls for {close_after} s");
        assert_eq!(rig.sessions.stop_calls(), 1);
        assert_eq!(rig.server.called("tx_commit"), 0, "the executor commits, not the host");
    }
}

#[test]
fn the_host_asks_the_executor_once_never_pauses_or_commits_and_a_second_stop_changes_nothing() {
    for state in [
        SessionState::Active,
        SessionState::Paused { by: person(), reason: PauseReason::Person },
        SessionState::Paused { by: person(), reason: PauseReason::Budget },
    ] {
        let rig = Rig::with(
            script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), long_text(20)]),
            RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
            standard_server,
        );
        rig.host.send(UserMessage::new("go")).unwrap();
        Rig::wait_for("the edit", || rig.server.called("face_set_profile") == 1);
        rig.sessions.set_state(state.clone());
        rig.host.stop();
        rig.host.stop();
        let events = rig.until_done(0.0);
        rig.host.stop();
        assert_eq!(rig.sessions.stop_calls(), 1, "{state:?}");
        assert_eq!(rig.sessions.stop_principals(), vec![person()]);
        assert_eq!(rig.sessions.pause_calls(), 0);
        assert_eq!(rig.server.called("tx_commit") + rig.server.called("tx_rollback"), 0);
        assert_eq!(events.iter().filter(|e| matches!(e, AssistantEvent::Paused)).count(), 1, "{state:?}");
        assert_eq!(events.iter().filter(|e| matches!(e, AssistantEvent::Done)).count(), 1);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(rig.rx.try_recv().is_none(), "a second stop emits nothing");
    }
}

#[test]
fn stop_commits_the_step_with_stopped_in_its_label_and_charges_what_was_used() {
    let rig = Rig::with(
        script(vec![call_turn("I will edit.", &[("c1", "face_set_profile", "{}")]), long_text(20)]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("make it round")).unwrap();
    Rig::wait_for("the edit", || rig.server.called("face_set_profile") == 1);
    rig.host.stop();
    rig.until_done(0.0);
    let entries = rig.sink.entries();
    let step = entries
        .iter()
        .find_map(|e| if let Entry::Step { tx, label, .. } = e { Some((tx.0, label.clone())) } else { None });
    assert_eq!(step, Some((31, "Assistant: make it round (stopped)".to_owned())));
    assert!(entries.iter().any(|e| matches!(e, Entry::Stopped)));
    assert!(!rig.sessions.charges().is_empty());
}

#[test]
fn a_call_in_flight_is_answered_as_stopped_and_not_waited_for() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "render_preview", "{}"), ("c2", "render_preview", "{}")]), say_turn("ok")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(
                tool("render_preview", "Render preview", "query", true),
                Outcome::text("rendered").held(&gate),
            );
            server
        },
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the call", || rig.server.called("render_preview") == 1);
    rig.host.stop();
    let events = rig.until_done(0.0);
    assert!(matches!(events.last(), Some(AssistantEvent::Done)));
    assert_eq!(rig.server.called("render_preview"), 1, "the second call never ran");
    gate.notify_waiters();
    rig.say("again", 0.0);
    let request = &rig.llm.requests()[1];
    let results: Vec<String> = request
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { id, blocks, is_error, .. } => Some(format!("{id} {is_error} {blocks:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(results[0].contains("stopped by the person before its result arrived"), "{results:?}");
    assert!(results[1].contains("not run"), "{results:?}");
}

#[test]
fn stop_while_waiting_leaves_the_approval_queued() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("ok")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            let queued = omuzaki_ops_api::ErrorDetail::Queued {
                approval: omuzaki_ops_api::ApprovalId(7),
                need: omuzaki_ops_api::Need::Person { reason: "r".into() },
            };
            server.add_fixed(
                tool("face_set_profile", "Face profile", "edit", false),
                Outcome::refusal("waiting", &queued),
            );
            server
        },
    );
    rig.sessions.queue_approval(7);
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("Waiting", || {
        while let Some(e) = rig.rx.try_recv() {
            seen.push(e);
        }
        seen.iter().any(|e| matches!(e, AssistantEvent::Waiting { .. }))
    });
    rig.host.stop();
    let rest = rig.until_done(0.0);
    assert!(matches!(rest.last(), Some(AssistantEvent::Done)));
    assert_eq!(rig.sessions.approvals().len(), 1, "the approval stays queued for the person");
}

#[test]
fn a_result_that_arrives_after_stop_is_recorded_for_the_next_message() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "render_preview", "{}")]), say_turn("ok"), say_turn("ok again")]),
        RigOptions { late_server: true, ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("the call", || {
        while let Some(e) = rig.rx.try_recv() {
            seen.push(e);
        }
        seen.iter().any(|e| matches!(e, AssistantEvent::ToolCall { .. }))
    });
    rig.host.stop();
    rig.until_done(0.0);
    Rig::wait_for("the late result", || {
        rig.sink.entries().iter().any(|e| matches!(e, Entry::ToolResult { id, .. } if id == "c1:late"))
    });
}

#[test]
fn dropping_the_host_with_an_active_session_ends_it_closed_once() {
    let rig = Rig::new(script(vec![say_turn("hi")]));
    rig.say("go", 0.0);
    let sessions = Arc::clone(&rig.sessions);
    drop(rig);
    assert_eq!(sessions.end_calls(), vec![omuzaki_ops_api::EndReason::Closed]);
}
