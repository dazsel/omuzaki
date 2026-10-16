//! The answer's transaction (#1111): which calls open it, which join it, and when it commits.

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
use omuzaki_llm_api::message::ContentBlock;

fn names(rig: &Rig) -> Vec<String> {
    rig.server.calls().into_iter().map(|(name, _)| name).collect()
}

#[test]
fn an_edit_opens_the_transaction_with_the_label_and_cause_and_the_answer_commits_it_once() {
    let first_line = "x".repeat(70);
    let rig = Rig::new(script(vec![
        call_turn("", &[("c1", "face_set_profile", "{\"profile\":\"a\"}")]),
        call_turn("", &[("c2", "face_set_profile", "{\"profile\":\"b\"}")]),
        say_turn("done"),
    ]));
    rig.say(&format!("{first_line}\nsecond line"), 0.0);
    let calls = rig.server.calls();
    let begin = calls.iter().find(|(n, _)| n == "tx_begin").unwrap();
    assert_eq!(begin.1["label"], format!("Assistant: {}", "x".repeat(60)));
    assert_eq!(begin.1["cause"], first_line);
    assert_eq!(rig.server.called("tx_begin"), 1, "the second edit joins");
    assert_eq!(rig.server.called("tx_commit"), 1);
    let order = names(&rig);
    assert_eq!(order, ["tx_begin", "face_set_profile", "face_set_profile", "tx_commit"]);
    assert_eq!(rig.server.host_log_reads(), 1, "the host reads the log's head once per answer, and no more");
}

#[test]
fn queries_builtins_and_host_tools_never_open_one_and_a_job_is_sent_outside() {
    let rig = Rig::new(script(vec![
        call_turn(
            "",
            &[
                ("c1", "render_preview", "{}"),
                ("c2", "history_undo", "{}"),
                ("c3", "export_video", "{}"),
                ("c4", "update_plan", "{\"items\":[]}"),
            ],
        ),
        say_turn("done"),
    ]));
    rig.say("go", 0.0);
    assert_eq!(rig.server.called("tx_begin"), 0);
    assert_eq!(rig.server.called("tx_commit"), 0);
    assert_eq!(names(&rig), ["render_preview", "history_undo", "export_video"]);
}

#[test]
fn a_driver_job_opens_the_transaction_when_it_is_first_and_the_host_commits_only_at_the_end() {
    let rig = Rig::new(script(vec![call_turn("", &[("c1", "script_run", "{}")]), say_turn("done")]));
    rig.say("run it", 0.0);
    assert_eq!(names(&rig), ["tx_begin", "script_run", "tx_commit"]);
}

#[test]
fn undo_with_the_transaction_open_is_answered_by_the_host_and_never_reaches_the_server() {
    let rig = Rig::new(script(vec![
        call_turn("", &[("c1", "face_set_profile", "{}"), ("c2", "history_undo", "{}")]),
        say_turn("ok"),
    ]));
    rig.say("go", 0.0);
    assert_eq!(rig.server.called("history_undo"), 0);
    let second = &rig.llm.requests()[1];
    let results: Vec<String> = second
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|b| {
            if let ContentBlock::ToolResult { id, blocks, .. } = b { Some(format!("{id}:{blocks:?}")) } else { None }
        })
        .collect();
    assert!(results[1].contains("use `discard_changes`"), "{results:?}");
}

#[test]
fn undo_with_no_transaction_open_goes_through() {
    let rig = Rig::new(script(vec![call_turn("", &[("c1", "history_undo", "{}")]), say_turn("ok")]));
    rig.say("go", 0.0);
    assert_eq!(rig.server.called("history_undo"), 1);
}

#[test]
fn a_tool_without_a_kind_is_an_edit_and_with_a_read_only_hint_a_query() {
    let rig = Rig::with(
        script(vec![
            call_turn("", &[("c1", "old_query", "{}")]),
            call_turn("", &[("c2", "old_edit", "{}")]),
            say_turn("ok"),
        ]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(kindless("old_query", true), Outcome::text("q"));
            server.add_fixed(kindless("old_edit", false), Outcome::text("e"));
            server
        },
    );
    rig.say("go", 0.0);
    assert_eq!(names(&rig), ["old_query", "tx_begin", "old_edit", "tx_commit"]);
}

#[test]
fn the_model_is_not_offered_the_transaction_tools() {
    let rig = Rig::new(script(vec![say_turn("hi")]));
    rig.say("go", 0.0);
    let offered: Vec<String> = rig.llm.requests()[0].tools.iter().map(|t| t.name.clone()).collect();
    assert!(offered.iter().all(|n| !n.starts_with("tx_")), "{offered:?}");
    assert!(offered.contains(&"discard_changes".to_owned()));
    let _ = (UserMessage::new(""), AssistantEvent::Done, &rig.host as &dyn Assistant);
}

#[test]
fn the_step_is_recorded_with_the_transaction_and_what_the_model_wrote() {
    let rig = Rig::new(script(vec![
        call_turn("I will narrow the jaw.", &[("c1", "face_set_profile", "{}")]),
        say_turn("Done."),
    ]));
    rig.say("make her face more oval", 0.0);
    let step = rig.sink.entries().into_iter().find_map(|e| match e {
        omuzaki_agent_api::entry::Entry::Step { tx, label, rationale, .. } => Some((tx, label, rationale)),
        _ => None,
    });
    let (tx, label, rationale) = step.expect("a step entry");
    assert_eq!(tx.0, 31);
    assert_eq!(label, "Assistant: make her face more oval");
    assert!(rationale.contains("narrow the jaw") && rationale.contains("Done."), "{rationale}");
}

#[test]
fn a_transaction_the_executor_would_not_commit_is_kept_and_committed_by_the_next_answer() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done"), say_turn("fine")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            let sessions = Arc::clone(sessions);
            // The first commit is refused; the executor keeps the transaction open, as it would.
            server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, call| {
                if call == 0 {
                    return Outcome::failure("the project changed; cannot commit");
                }
                sessions.commit_open_tx();
                Outcome::text("tx committed")
            });
            server
        },
    );
    let first = rig.say("make it round", 0.0);
    assert!(matches!(first.last(), Some(AssistantEvent::Done)));
    assert!(rig.sink.entries().iter().all(|e| !matches!(e, Entry::Step { .. })), "no step yet");
    assert!(rig.sessions.open_tx().is_some(), "the transaction is still open");

    rig.say("thanks", 0.0);
    assert_eq!(rig.server.called("tx_commit"), 2, "the next answer tried again");
    assert!(rig.sessions.open_tx().is_none());
    assert_eq!(rig.sink.entries().iter().filter(|e| matches!(e, Entry::Step { .. })).count(), 1);
}

#[test]
fn the_next_message_never_joins_a_transaction_the_last_one_left_open() {
    let rig = Rig::with(
        script(vec![
            call_turn("", &[("c1", "face_set_profile", "{}")]),
            say_turn("done"),
            call_turn("", &[("c2", "face_set_profile", "{}")]),
            say_turn("done again"),
        ]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            let sessions = Arc::clone(sessions);
            server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, call| {
                if call == 0 {
                    return Outcome::failure("the project changed; cannot commit");
                }
                sessions.commit_open_tx();
                Outcome::text("tx committed")
            });
            server
        },
    );
    rig.say("make it round", 0.0);
    rig.say("and wider", 0.0);
    let steps: Vec<(String, String)> = rig
        .sink
        .entries()
        .into_iter()
        .filter_map(|e| if let Entry::Step { label, rationale, .. } = e { Some((label, rationale)) } else { None })
        .collect();
    assert_eq!(steps.len(), 2, "one step per message: {steps:?}");
    assert_eq!(steps[0].1, "done", "the recovered step keeps the rationale of the answer that made it");
    assert_eq!(steps[1].1, "done again");
    assert!(steps[0].0.contains("make it round") && steps[1].0.contains("and wider"), "{steps:?}");
    assert_eq!(rig.server.called("tx_begin"), 2, "the second message opened its own transaction");
    assert!(rig.sessions.open_tx().is_none());
}

#[test]
fn a_transaction_that_cannot_be_committed_twice_is_discarded_and_the_person_is_told() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done"), say_turn("fine")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(tool("tx_commit", "tx.commit", "builtin", false), Outcome::failure("cannot commit"));
            server
        },
    );
    rig.say("make it round", 0.0);
    let events = rig.say("thanks", 0.0);
    assert!(
        events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::Mcp(t)) if t.contains("discarded"))),
        "{events:?}"
    );
    assert_eq!(rig.server.called("tx_rollback"), 1);
    assert!(rig.sessions.open_tx().is_none());
}

#[test]
fn the_host_reads_the_op_log_once_per_answer_and_the_model_calls_are_untouched() {
    let rig = Rig::new(script(vec![say_turn("one"), say_turn("two")]));
    rig.say("first", 0.0);
    rig.say("second", 0.0);
    assert_eq!(rig.server.host_log_reads(), 2, "one read of the log's head for each answer");
    assert!(rig.server.calls().iter().all(|(name, _)| name != "history_log"), "the model never called it");
}
