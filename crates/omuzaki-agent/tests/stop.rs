//! Stop (#1336, D4): no output after the click, what is kept, how fast `Done` comes, and what the executor is asked.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use std::sync::Arc;

use common::*;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_llm_api::message::ContentBlock;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_mock::ScriptTurn;
use omuzaki_ops_api::{PauseReason, SessionState, Sessions as _};

/// The events of a turn that calls one tool with a short text: start, text, the call's three, usage and stop.
const FIRST_TURN_EVENTS: usize = 7;

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
        // Once the host is parked at the held stream it has flushed what it has; then read a seeded part of it.
        rig.wait_parked();
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
        // Under load the host can finish before the drain above ran, so Done may already be in `after`.
        let rest =
            if after.iter().any(|e| matches!(e, AssistantEvent::Done)) { Vec::new() } else { rig.until_done(0.0) };
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
    rig.wait_parked();
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
        // `stop` queues synchronously on the caller's thread and the host is idle after `Done`, so there is nothing to wait for.
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
    assert!(entries.iter().any(|e| matches!(e, Entry::Stopped { .. })));
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

/// The events of the first turn of `call_turn("", &[one call])` and of a summary turn, for holding the third stream.
const HEAVY_TURN_EVENTS: usize = 5;
const SUMMARY_TURN_EVENTS: usize = 4;

#[test]
fn stop_after_a_compaction_in_the_same_answer_does_not_panic_and_keeps_what_was_read() {
    let heavy = ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::tool_use_start("c1", "render_preview"),
        LlmEvent::tool_use_end("c1"),
        LlmEvent::Usage(omuzaki_llm_api::usage::Usage::new(15_001, 10)),
        LlmEvent::Stop(StopReason::ToolUse),
    ]);
    let rig = Rig::with(
        script(vec![heavy, say_turn("SUMMARY"), long_text(20)]),
        RigOptions {
            window: Some(20_000),
            prices: free_prices(),
            hold_after: Some(HEAVY_TURN_EVENTS + SUMMARY_TURN_EVENTS + 3),
            ..RigOptions::default()
        },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    rig.wait_parked();
    rig.host.stop();
    let events = rig.until_done(0.0);
    assert!(matches!(events.last(), Some(AssistantEvent::Done)));
    assert_eq!(rig.sessions.stop_calls(), 1);
}

#[test]
fn a_commit_the_executor_refuses_claims_no_step_and_shows_the_error() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(
                tool("tx_commit", "tx.commit", "builtin", false),
                Outcome::failure("the project changed; cannot commit"),
            );
            server
        },
    );
    let events = rig.say("go", 0.0);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AssistantEvent::Error(HostError::Mcp(text)) if text.contains("cannot commit")))
    );
    assert!(rig.sink.entries().iter().all(|e| !matches!(e, Entry::Step { .. })), "no undo step was made");
    assert_eq!(
        events.last(),
        Some(&AssistantEvent::Unclosed),
        "the executor still holds it, so the answer is not Done"
    );
    assert!(!events.contains(&AssistantEvent::Done), "{events:?}");
}

#[test]
fn stop_pressed_while_the_answer_commits_still_stops_it() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(tool("tx_commit", "tx.commit", "builtin", false), Outcome::text("committed").held(&gate));
            server
        },
    );
    rig.host.send(UserMessage::new("make it round")).unwrap();
    Rig::wait_for("the commit", || rig.server.called("tx_commit") == 1);
    rig.host.stop();
    let events = rig.until_done(0.0);
    assert!(events.iter().any(|e| matches!(e, AssistantEvent::Paused)));
    assert_eq!(rig.sessions.stop_calls(), 1, "the executor, not the host, closes the step");
    assert!(rig.sink.entries().iter().any(|e| matches!(e, Entry::Stopped { .. })));
}

#[test]
fn done_is_never_sent_while_the_stopped_step_is_still_open() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), long_text(20)]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.sessions.set_stop(StopBehaviour::Pending { close_after: 100.0 }, 0.0);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the edit", || rig.server.called("face_set_profile") == 1);
    rig.host.stop();
    let mut done = false;
    for _ in 0..7_000 {
        rig.clock.advance(Seconds(0.01));
        std::thread::sleep(std::time::Duration::from_millis(1));
        while let Some(event) = rig.rx.try_recv() {
            done |= matches!(event, AssistantEvent::Done);
        }
    }
    assert!(!done, "70 s on, the step is still open, so there is no Done");
    for _ in 0..4_000 {
        rig.clock.advance(Seconds(0.01));
        std::thread::sleep(std::time::Duration::from_millis(1));
        while let Some(event) = rig.rx.try_recv() {
            done |= matches!(event, AssistantEvent::Done);
        }
        if done {
            break;
        }
    }
    assert!(done, "Done follows the close");
}

#[test]
fn text_a_transcript_showed_is_not_cut_by_stop() {
    let rig = Rig::with(
        script(vec![long_text(20)]),
        RigOptions { hold_after: Some(8), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    rig.wait_parked();
    // A panel opened late never reads the deltas; it shows the transcript.
    let shown = assistant_text(&rig.host.transcript());
    assert!(!shown.is_empty());
    rig.host.stop();
    assert_eq!(assistant_text(&rig.host.transcript()), shown);
    rig.until_done(0.0);
}

#[test]
fn a_charge_the_executor_refuses_ends_the_answer_as_session_ended_without_a_step() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("last words")]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the edit", || rig.server.called("face_set_profile") == 1);
    Rig::wait_for("the held stream", || {
        rig.gate.delivered.load(std::sync::atomic::Ordering::Acquire) >= FIRST_TURN_EVENTS + 2
    });
    rig.sessions.revoke();
    rig.gate.open();
    let events = rig.until_done(0.0);
    assert!(events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::SessionEnded))), "{events:?}");
    let charges = rig.sink.entries().iter().filter(|e| matches!(e, Entry::Charge { .. })).count();
    assert_eq!(charges, 1, "only the first turn's charge was accepted");
    assert!(rig.sink.entries().iter().all(|e| !matches!(e, Entry::Step { .. })));
}

#[test]
fn stop_after_a_compaction_cuts_by_the_text_read_since_the_summary_not_the_whole_answer() {
    let heavy = ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta("AAAAAAAAAA".into()),
        LlmEvent::tool_use_start("c1", "render_preview"),
        LlmEvent::tool_use_end("c1"),
        LlmEvent::Usage(omuzaki_llm_api::usage::Usage::new(15_001, 10)),
        LlmEvent::Stop(StopReason::ToolUse),
    ]);
    let after = ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta("BBBBB".into()),
        LlmEvent::TextDelta("CCCCC".into()),
        usage(5, 5),
        LlmEvent::Stop(StopReason::EndTurn),
    ]);
    let held = 6 + SUMMARY_TURN_EVENTS + 3;
    let rig = Rig::with(
        script(vec![heavy, say_turn("SUMMARY"), after, say_turn("ok")]),
        RigOptions { window: Some(20_000), prices: free_prices(), hold_after: Some(held), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    rig.wait_parked();
    // The person has read the ten bytes before the summary and "BBBBB" after it, not "CCCCC".
    let mut read = 0;
    while read < 15 {
        match rig.rx.try_recv() {
            Some(AssistantEvent::TextDelta(t)) => read += t.len(),
            Some(_) => {}
            None => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }
    rig.host.stop();
    rig.until_done(0.0);
    rig.gate.open();
    rig.say("again", 0.0);
    let requests = rig.llm.requests();
    let kept: String = requests[3]
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
        .collect();
    assert!(kept.contains("BBBBB") && !kept.contains("CCCCC"), "{kept:?}");
}

#[test]
fn compacting_does_not_count_unread_text_as_displayed() {
    let heavy = ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta("AAAAAAAAAA".into()),
        LlmEvent::tool_use_start("c1", "render_preview"),
        LlmEvent::tool_use_end("c1"),
        LlmEvent::Usage(omuzaki_llm_api::usage::Usage::new(15_001, 10)),
        LlmEvent::Stop(StopReason::ToolUse),
    ]);
    let held = 6 + SUMMARY_TURN_EVENTS + 2;
    let rig = Rig::with(
        script(vec![heavy, say_turn("SUMMARY"), long_text(20)]),
        RigOptions { window: Some(20_000), prices: free_prices(), hold_after: Some(held), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    rig.wait_parked();
    // The person read nothing: the compaction's own look at the transcript showed them nothing.
    rig.host.stop();
    rig.until_done(0.0);
    let shown = assistant_text(&rig.host.transcript());
    assert!(!shown.contains("AAAAAAAAAA"), "text nobody read is cut: {shown:?}");
}

#[test]
fn stop_pressed_while_the_compaction_stores_its_render_publishes_nothing() {
    let heavy = ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta("AAAAAAAAAA".into()),
        LlmEvent::tool_use_start("c1", "render_preview"),
        LlmEvent::tool_use_end("c1"),
        LlmEvent::Usage(omuzaki_llm_api::usage::Usage::new(15_001, 10)),
        LlmEvent::Stop(StopReason::ToolUse),
    ]);
    let rig = Rig::with(
        script(vec![heavy, say_turn("SUMMARY"), say_turn("ok")]),
        RigOptions {
            window: Some(20_000),
            prices: free_prices(),
            // The tool result's render is stored first and passes; the compaction's own store is the one held.
            hold_blobs: true,
            hold_blobs_after: 1,
            ..RigOptions::default()
        },
        |s| {
            let server = standard_server(s);
            server
                .add_fixed(tool("render_preview", "Render preview", "query", true), Outcome::image(&[137, 80, 78, 71]));
            server
        },
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the render being stored", || rig.sink.blob_waiting.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(rig.sink.blob_calls.load(std::sync::atomic::Ordering::SeqCst), 2, "the held store is the compaction's");
    assert_eq!(rig.llm.requests().len(), 2, "the summary turn has run");
    rig.host.stop();
    rig.sink.hold_blobs.store(false, std::sync::atomic::Ordering::SeqCst);
    rig.until_done(0.0);
    assert!(
        !rig.sink.entries().iter().any(|e| matches!(e, Entry::Compacted { .. })),
        "a compaction Stop interrupted is not published"
    );
    assert_eq!(rig.sessions.stop_calls(), 1);
}

#[test]
fn stop_pressed_while_the_step_is_written_still_stops_the_executor_and_records_the_cutoff() {
    let rig = Rig::new(script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]));
    rig.sink.hold_steps.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.host.send(UserMessage::new("make it round")).unwrap();
    Rig::wait_for("the step being written", || rig.sink.step_waiting.load(std::sync::atomic::Ordering::SeqCst));
    rig.host.stop();
    rig.sink.hold_steps.store(false, std::sync::atomic::Ordering::SeqCst);
    let events = rig.until_done(0.0);
    assert!(events.iter().any(|e| matches!(e, AssistantEvent::Paused)), "{events:?}");
    assert_eq!(rig.sessions.stop_calls(), 1, "the executor was asked to stop");
    assert!(rig.sink.entries().iter().any(|e| matches!(e, Entry::Stopped { .. })), "and the cutoff was recorded");
    let steps = rig.sink.entries().iter().filter(|e| matches!(e, Entry::Step { .. })).count();
    assert_eq!(steps, 1, "the step is written once");
}

#[test]
fn a_pending_stop_that_reverts_every_edit_records_no_undo_step() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), long_text(20)]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.sessions.set_stop(StopBehaviour::Pending { close_after: 1.0 }, 0.0);
    rig.sessions.set_pending_stop_reverts();
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the transaction", || {
        rig.server.called("tx_begin") == 1 && rig.server.called("face_set_profile") == 1
    });
    rig.host.stop();
    let events = rig.until_done(0.1);
    assert!(matches!(events.last(), Some(AssistantEvent::Done)));
    assert!(
        rig.sink.entries().iter().all(|e| !matches!(e, Entry::Step { .. })),
        "nothing was kept, so there is no step to undo"
    );
}

#[test]
fn a_commit_that_lands_while_stop_cancels_the_call_still_records_its_step() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            let sessions = Arc::clone(sessions);
            // The executor applies the commit, and the answer to the host is still on its way when Stop arrives.
            server.add(tool("tx_commit", "tx.commit", "builtin", false), {
                let gate = Arc::clone(&gate);
                move |_, _| {
                    sessions.commit_open_tx();
                    Outcome::text("committed").held(&gate)
                }
            });
            server
        },
    );
    rig.host.send(UserMessage::new("make it round")).unwrap();
    Rig::wait_for("the commit", || rig.server.called("tx_commit") == 1);
    rig.host.stop();
    let events = rig.until_done(0.0);
    assert_eq!(events.last(), Some(&AssistantEvent::Done), "the executor holds nothing: closed, outcome to come");
    rig.settle_steps(1);
    let steps = rig.steps();
    assert_eq!(steps.len(), 1, "the undo step the executor made is recorded, once the log shows it: {steps:?}");
}

#[test]
fn a_pending_stop_is_a_step_only_if_the_log_names_its_transaction() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), long_text(20)]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    // The stop reverts every edit, so its transaction never commits; meanwhile a person grants an approval the session
    // had retained, which makes a step of its own and raises the session's count.
    rig.sessions.set_stop(StopBehaviour::Pending { close_after: 1.0 }, 0.0);
    rig.sessions.set_pending_stop_reverts();
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the transaction", || {
        rig.server.called("tx_begin") == 1 && rig.server.called("face_set_profile") == 1
    });
    rig.host.stop();
    rig.sessions.bump_steps();
    let events = rig.until_done(0.1);
    assert!(matches!(events.last(), Some(AssistantEvent::Done)));
    assert!(
        rig.sink.entries().iter().all(|e| !matches!(e, Entry::Step { .. })),
        "a higher step count is not this transaction's commit"
    );
}

#[test]
fn a_stop_the_executor_refuses_is_shown_ends_unclosed_at_once_and_the_next_message_closes_it() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), long_text(20)]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.sessions.set_stop(StopBehaviour::Refused, 0.0);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the transaction", || {
        rig.server.called("tx_begin") == 1 && rig.server.called("face_set_profile") == 1
    });
    rig.host.stop();
    rig.gate.open();
    let events = rig.until_done(0.0);
    let refused = events
        .iter()
        .position(|e| matches!(e, AssistantEvent::Error(HostError::Mcp(t)) if t.contains("Stop was refused")))
        .expect("the refusal is shown");
    // Stop's ending makes no call that could wait on the server: the transaction is Unclosed at once, never Done.
    assert!(refused < events.len() - 1 && events.last() == Some(&AssistantEvent::Unclosed), "{events:?}");
    assert_eq!(rig.server.called("tx_commit") + rig.server.called("tx_rollback"), 0);
    assert!(rig.sessions.open_tx().is_some(), "the executor still holds it");
    assert_eq!(rig.say("again", 0.0).last(), Some(&AssistantEvent::Done));
    assert!(rig.sessions.open_tx().is_none(), "the next message closed it first");
    assert_eq!(rig.sink.entries().iter().filter(|e| matches!(e, Entry::Step { .. })).count(), 1);
}

#[test]
fn a_refused_stop_whose_transaction_cannot_be_closed_ends_unclosed_and_the_next_message_closes_it_first() {
    let refuse = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = Arc::clone(&refuse);
    let rig = Rig::with(
        script(vec![call_turn("WHY", &[("c1", "face_set_profile", "{}")]), long_text(20), say_turn("fine")]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        move |sessions| {
            let server = standard_server(sessions);
            let (commit, rollback) = (Arc::clone(sessions), Arc::clone(sessions));
            let (f, g) = (Arc::clone(&flag), Arc::clone(&flag));
            server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, _| {
                if f.load(std::sync::atomic::Ordering::SeqCst) {
                    return Outcome::failure("cannot commit now");
                }
                commit.commit_open_tx();
                Outcome::text("tx committed")
            });
            server.add(tool("tx_rollback", "tx.rollback", "builtin", false), move |_, _| {
                if g.load(std::sync::atomic::Ordering::SeqCst) {
                    return Outcome::failure("cannot roll back now");
                }
                rollback.rollback_open_tx();
                Outcome::text("tx rolled back")
            });
            server
        },
    );
    rig.sessions.set_stop(StopBehaviour::Refused, 0.0);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the transaction", || {
        rig.server.called("tx_begin") == 1 && rig.server.called("face_set_profile") == 1
    });
    rig.host.stop();
    rig.gate.open();
    let events = rig.until_done(0.0);
    // The executor refused the Stop: the answer ends in the failed state, never `Done`.
    assert_eq!(events.last(), Some(&AssistantEvent::Unclosed), "{events:?}");
    assert!(!events.contains(&AssistantEvent::Done), "{events:?}");
    for said in ["Stop was refused", "still open"] {
        assert!(
            events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::Mcp(t)) if t.contains(said))),
            "the person is told {said:?}: {events:?}"
        );
    }
    assert!(rig.sessions.open_tx().is_some(), "the executor still holds it");
    assert!(rig.steps().is_empty());

    // The next message tries the commit, then the rollback; both refused, it does not run and ends Unclosed too.
    let events = rig.say("still", 0.0);
    assert_eq!(events.last(), Some(&AssistantEvent::Unclosed), "{events:?}");
    for said in ["cannot commit now", "cannot roll back now"] {
        assert!(
            events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::Mcp(t)) if t.contains(said))),
            "the person is told {said:?}: {events:?}"
        );
    }
    assert_eq!((rig.server.called("tx_commit"), rig.server.called("tx_rollback")), (1, 1));

    refuse.store(false, std::sync::atomic::Ordering::SeqCst);
    let calls_before = rig.server.calls().len();
    let events = rig.say("again", 0.0);
    assert_eq!(events.last(), Some(&AssistantEvent::Done), "{events:?}");
    let second: Vec<String> = rig.server.calls()[calls_before..].iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(second.first().map(String::as_str), Some("tx_commit"), "closed before anything else: {second:?}");
    assert!(rig.sessions.open_tx().is_none(), "the next message closed it first");
    let steps = rig.steps();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps[0].1.ends_with("(stopped)") && steps[0].2.contains("WHY"), "its own label and rationale: {steps:?}");
}

#[test]
fn a_message_sent_while_the_changes_are_still_unclosed_does_not_run_and_ends_unclosed_again() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done"), say_turn("never")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(tool("tx_commit", "tx.commit", "builtin", false), Outcome::failure("cannot commit"));
            server
                .add_fixed(tool("tx_rollback", "tx.rollback", "builtin", false), Outcome::failure("cannot roll back"));
            server
        },
    );
    assert_eq!(rig.say("make it round", 0.0).last(), Some(&AssistantEvent::Unclosed));
    let requests = rig.llm.requests().len();
    let events = rig.say("and wider", 0.0);
    assert_eq!(events.last(), Some(&AssistantEvent::Unclosed), "{events:?}");
    assert_eq!(rig.llm.requests().len(), requests, "the message did not run");
    assert!(rig.sessions.open_tx().is_some());
    // The message is kept, as the person sent it, and a continued conversation reads it back the same way.
    let entries = rig.sink.entries();
    assert!(entries.iter().any(|e| matches!(e, Entry::User { text, .. } if text == "and wider")));
    let (resumed, _) = store_replay(&entries);
    assert_eq!(resumed.entries, rig.host.transcript());
}

#[test]
fn stop_pressed_while_the_compacted_entry_is_appended_leaves_the_log_and_the_history_agreeing() {
    let heavy = ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta("AAAAAAAAAA".into()),
        LlmEvent::tool_use_start("c1", "render_preview"),
        LlmEvent::tool_use_end("c1"),
        LlmEvent::Usage(omuzaki_llm_api::usage::Usage::new(15_001, 10)),
        LlmEvent::Stop(StopReason::ToolUse),
    ]);
    let rig = Rig::with(
        script(vec![heavy, say_turn("SUMMARY-TEXT"), say_turn("ok")]),
        RigOptions { window: Some(20_000), prices: free_prices(), ..RigOptions::default() },
        standard_server,
    );
    rig.sink.hold_compacted.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the entry being appended", || rig.sink.compacted_waiting.load(std::sync::atomic::Ordering::SeqCst));
    rig.host.stop();
    rig.sink.hold_compacted.store(false, std::sync::atomic::Ordering::SeqCst);
    rig.until_done(0.0);
    let kinds: Vec<&str> = rig
        .sink
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Compacted { .. } => Some("compacted"),
            Entry::Stopped { .. } => Some("stopped"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["compacted", "stopped"], "both are in the log, in order");
    rig.say("again", 0.0);
    let first = &rig.llm.requests()[2].messages[0];
    let text: String = first
        .content
        .iter()
        .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
        .collect();
    assert!(text.contains("SUMMARY-TEXT"), "the history is the compacted one the log names: {text:?}");
}

#[test]
fn a_pending_stop_that_commits_is_recorded_as_a_step_by_its_transaction() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), long_text(20)]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.sessions.set_stop(StopBehaviour::Pending { close_after: 1.0 }, 0.0);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the transaction", || {
        rig.server.called("tx_begin") == 1 && rig.server.called("face_set_profile") == 1
    });
    rig.host.stop();
    rig.until_done(0.1);
    rig.settle_steps(1);
    let labels: Vec<String> = rig.steps().into_iter().map(|(_, label, _)| label).collect();
    assert_eq!(labels.len(), 1, "{labels:?}");
    assert!(labels[0].contains("(stopped)"), "{labels:?}");
}

#[test]
fn the_summary_is_built_only_from_text_the_person_has_read() {
    let heavy = ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta("AAAAAAAAAA".into()),
        LlmEvent::tool_use_start("c1", "render_preview"),
        LlmEvent::tool_use_end("c1"),
        LlmEvent::Usage(omuzaki_llm_api::usage::Usage::new(15_001, 10)),
        LlmEvent::Stop(StopReason::ToolUse),
    ]);
    let held = 6 + SUMMARY_TURN_EVENTS + 2;
    let rig = Rig::with(
        script(vec![heavy, say_turn("SUMMARY-TEXT"), long_text(20)]),
        RigOptions { window: Some(20_000), prices: free_prices(), hold_after: Some(held), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    // The stream is held after the summary turn, and nobody has read the queue: none of "AAAAAAAAAA" was shown.
    Rig::wait_for("the held stream", || rig.gate.delivered.load(std::sync::atomic::Ordering::Acquire) >= held);
    let summary_request = request_text(&rig.llm.requests()[1]);
    assert!(!summary_request.contains("AAAAAAAAAA"), "unread text is not folded into the summary: {summary_request}");
    rig.gate.open();
    rig.until_done(0.0);
    let logged: String = rig
        .sink
        .entries()
        .iter()
        .filter_map(|e| if let Entry::Compacted { message, .. } = e { Some(message.clone()) } else { None })
        .collect();
    assert!(!logged.contains("AAAAAAAAAA"), "nor does the log carry it: {logged}");
}

#[test]
fn a_commit_the_log_shows_late_does_not_delay_done_and_becomes_its_step_when_the_log_shows_it() {
    let rig = Rig::with(
        script(vec![call_turn("WHY-ONE", &[("c1", "face_set_profile", "{}")]), long_text(20), say_turn("fine")]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.sessions.set_stop(StopBehaviour::Pending { close_after: 0.0 }, 0.0);
    rig.sessions.set_log_lag(true);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the edit", || rig.server.called("face_set_profile") == 1);
    let clicked = rig.clock.now().0;
    let reads = rig.server.host_log_reads();
    rig.host.stop();
    // The clock never moves and the log never shows the commit: had the host waited on either, there would be no end.
    let events = rig.until_done(0.0);
    assert_eq!(events.last(), Some(&AssistantEvent::Done), "{events:?}");
    assert!(rig.clock.now().0 - clicked <= 0.1, "Done within D4's 100 ms with nothing joined");
    // With the clock still, the idle host has not come to its first read either, so this counts only Stop's ending.
    assert_eq!(rig.server.host_log_reads(), reads, "no read of the log before Done");
    assert!(rig.steps().is_empty(), "the log has not shown it yet");
    rig.sessions.release_log();
    rig.settle_steps(1);
    let steps = rig.steps();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps[0].1.contains("(stopped)") && steps[0].2.contains("WHY-ONE"), "{steps:?}");
}

#[test]
fn an_outcome_the_log_shows_only_after_the_next_message_began_is_still_recorded() {
    let rig = Rig::with(
        script(vec![call_turn("WHY-ONE", &[("c1", "face_set_profile", "{}")]), long_text(20), say_turn("fine")]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.sessions.set_stop(StopBehaviour::Pending { close_after: 0.0 }, 0.0);
    rig.sessions.set_log_lag(true);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the edit", || rig.server.called("face_set_profile") == 1);
    rig.host.stop();
    rig.until_done(0.0);
    rig.gate.open();
    // The next message starts while the log still lags: its read succeeds and does not show the commit, which must
    // not drop it.
    let reads = rig.server.host_log_reads();
    rig.say("again", 0.0);
    assert!(rig.server.host_log_reads() > reads, "the next message asked the log");
    assert!(rig.steps().is_empty());
    rig.sessions.release_log();
    rig.settle_steps(1);
    let steps = rig.steps();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps[0].2.contains("WHY-ONE"), "the rationale of the answer that made it: {steps:?}");
}

#[test]
fn a_stop_while_the_previous_answers_changes_are_closed_cuts_only_the_new_answer() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let commits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (g, n) = (Arc::clone(&gate), Arc::clone(&commits));
    let rig = Rig::with(
        script(vec![call_turn("RATIONALE-ONE ", &[("c1", "face_set_profile", "{}")]), say_turn("ANSWER-ONE-TEXT")]),
        RigOptions::default(),
        move |sessions| {
            let server = standard_server(sessions);
            let commit = Arc::clone(sessions);
            server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, _| {
                if n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    return Outcome::failure("cannot commit now");
                }
                // The second commit is the next message's close of the first answer's changes: applied, its reply held
                // (MCP waiting on the journal) until Stop abandons it.
                commit.commit_open_tx();
                Outcome::text("tx committed").held(&g)
            });
            server
        },
    );
    assert_eq!(rig.say("one", 0.0).last(), Some(&AssistantEvent::Unclosed));
    let shown_before = assistant_text(&rig.host.transcript());
    assert!(shown_before.contains("ANSWER-ONE-TEXT"));
    rig.host.send(UserMessage::new("two")).unwrap();
    Rig::wait_for("the close of the first answer's changes", || commits.load(std::sync::atomic::Ordering::SeqCst) == 2);
    rig.host.stop();
    let events = rig.until_done(0.0);
    assert_eq!(events.last(), Some(&AssistantEvent::Done), "{events:?}");
    // Stop abandoned the held commit; the executor had applied it, so the step comes from the log.
    rig.settle_steps(1);

    let entries = rig.sink.entries();
    let user_two = entries.iter().position(|e| matches!(e, Entry::User { text, .. } if text == "two")).unwrap();
    let stopped = entries.iter().position(|e| matches!(e, Entry::Stopped { .. })).unwrap();
    assert!(user_two < stopped, "the cutoff follows the message it belongs to");
    assert_eq!(entries[user_two].answer_id(), entries[stopped].answer_id(), "and names it");
    assert!(entries[stopped].answer_id().is_some());
    // The first answer's text survives everywhere it is shown again.
    assert_eq!(assistant_text(&rig.host.transcript()), shown_before, "the live transcript");
    let (resumed, markdown) = store_replay(&entries);
    assert_eq!(assistant_text(&resumed.entries), shown_before, "the resumed conversation");
    assert!(!resumed.interrupted);
    assert!(markdown.contains("ANSWER-ONE-TEXT"), "the export: {markdown}");
    let continued = Rig::with(
        script(vec![say_turn("ok")]),
        RigOptions { history: entries, ..RigOptions::default() },
        standard_server,
    );
    continued.say("three", 0.0);
    assert!(request_text(&continued.llm.requests()[0]).contains("ANSWER-ONE-TEXT"), "a continued history");
    // And the step is the first answer's, with its rationale.
    let steps = rig.steps();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps[0].2.contains("RATIONALE-ONE"), "{steps:?}");
}

#[test]
fn a_change_by_others_after_the_answer_began_reaches_the_model_once_and_older_ones_never() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), long_text(20), say_turn("fine")]),
        RigOptions { hold_after: Some(FIRST_TURN_EVENTS + 2), ..RigOptions::default() },
        |sessions| {
            // Before the session opened: not this session's news.
            sessions.other_change("old.change");
            sessions.open_caught_up();
            standard_server(sessions)
        },
    );
    rig.sessions.set_stop(StopBehaviour::Pending { close_after: 0.0 }, 0.0);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the edit", || rig.server.called("face_set_profile") == 1);
    rig.wait_parked();
    rig.sessions.other_change("new.change");
    rig.host.stop();
    rig.until_done(0.0);
    // The stopped step's outcome is read from the log while the host is idle: that read reaches "new.change".
    rig.settle_steps(1);
    rig.gate.open();
    rig.say("again", 0.0);
    let context = request_text(rig.llm.requests().last().unwrap());
    assert_eq!(context.matches("new.change").count(), 1, "told once: {context}");
    assert!(!context.contains("old.change"), "older changes are not this session's: {context}");
}
