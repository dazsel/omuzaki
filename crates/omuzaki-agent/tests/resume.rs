//! Continuing a stored conversation (#1395): the model's history rebuilt from the stored entries.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use std::sync::Arc;

use common::*;
use omuzaki_agent::sink::EntrySink;
use omuzaki_agent::store::ConversationStore;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_core::time::FakeClock;
use omuzaki_llm_api::message::{ContentBlock, Message, Role};
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_mock::ScriptTurn;
use serde_json::json;

fn continued(history: Vec<Entry>, turns: Vec<ScriptTurn>) -> Rig {
    Rig::with(script(turns), RigOptions { history, ..RigOptions::default() }, standard_server)
}

/// The messages a run ended with: the last request plus the assistant's final message.
fn final_history(rig: &Rig, last_answer: &str) -> Vec<Message> {
    let mut messages = rig.llm.requests().last().unwrap().messages.clone();
    messages.push(Message::new(Role::Assistant, vec![ContentBlock::text(last_answer)]));
    messages
}

#[test]
fn a_continued_conversation_rebuilds_the_hosts_own_history_and_puts_the_new_message_last() {
    let first = Rig::new(script(vec![
        call_turn("Looking.", &[("c1", "render_preview", "{}"), ("c2", "face_set_profile", "{\"profile\":\"egg\"}")]),
        call_turn("Narrowing.", &[("c3", "face_set_profile", "{\"profile\":\"oval\"}")]),
        say_turn("Here she is."),
    ]));
    first.say("make her face more oval", 0.0);
    let ended_with = final_history(&first, "Here she is.");
    let entries = first.sink.entries();

    let second = continued(entries, vec![say_turn("Sure.")]);
    second.say("and the nose", 0.0);
    let request = &second.llm.requests()[0];
    let (new, rebuilt) = request.messages.split_last().unwrap();
    assert_eq!(rebuilt, &ended_with[..]);
    assert_eq!(new.role, Role::User);
    assert!(matches!(new.content.last(), Some(ContentBlock::Text(t)) if t == "and the nose"));
    let transcript = second.host.transcript();
    assert!(
        matches!(&transcript[0], Entry::Session { .. }) && transcript.len() > 5,
        "the view starts with the stored entries"
    );
}

#[test]
fn a_thinking_block_survives_a_store_write_and_read_byte_for_byte() {
    let thinking = json!({"type": "thinking", "signature": "sig==", "thinking": "the jaw reads round"});
    let events = vec![
        LlmEvent::start("m"),
        LlmEvent::opaque("anthropic", thinking.clone()),
        LlmEvent::TextDelta("Done.".into()),
        usage(1, 1),
        LlmEvent::Stop(StopReason::EndTurn),
    ];
    let first = Rig::new(script(vec![ScriptTurn::new(events)]));
    first.say("go", 0.0);

    let dir = tempfile::tempdir().unwrap();
    let store = ConversationStore::open(dir.path(), "0123456789abcdef0123456789abcdef", Arc::new(FakeClock::default()));
    let id = store.create("m").unwrap();
    let sink = store.sink(id);
    for entry in first.sink.entries() {
        sink.append(&entry).unwrap();
    }
    let stored = store.read(id).unwrap();

    let second = continued(stored, vec![say_turn("ok")]);
    second.say("again", 0.0);
    let history = &second.llm.requests()[0].messages;
    assert_eq!(history[1].content[0], ContentBlock::opaque("anthropic", thinking));
}

#[test]
fn history_starts_at_the_last_compaction() {
    let entries = vec![
        Entry::user("old", vec![]),
        Entry::assistant(vec![ContentBlock::text("old answer")]),
        Entry::compacted("SUMMARY: the face was made oval"),
        Entry::user("middle", vec![]),
        Entry::assistant(vec![ContentBlock::text("middle answer")]),
    ];
    let rig = continued(entries, vec![say_turn("ok")]);
    rig.say("next", 0.0);
    let messages = &rig.llm.requests()[0].messages;
    let all = format!("{messages:?}");
    assert!(!all.contains("old answer") && !all.contains("\"old\""), "{all}");
    assert!(all.contains("SUMMARY: the face was made oval") && all.contains("middle answer"));
}

#[test]
fn a_log_cut_after_a_tool_use_gets_one_synthetic_error_and_the_next_request_is_accepted() {
    let entries = vec![
        Entry::user("go", vec![]),
        Entry::assistant(vec![ContentBlock::tool_use("c1", "render_preview", json!({}))]),
        Entry::tool_call("c1", "render_preview", "Render preview"),
    ];
    let rig = continued(entries, vec![say_turn("ok")]);
    let events = rig.say("carry on", 0.0);
    assert!(matches!(events.last(), Some(AssistantEvent::Done)));
    let messages = &rig.llm.requests()[0].messages;
    let errors: Vec<&ContentBlock> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|b| matches!(b, ContentBlock::ToolResult { is_error: true, .. }))
        .collect();
    assert_eq!(errors.len(), 1);
    assert!(format!("{errors:?}").contains("interrupted: the studio closed"));
}

#[test]
fn a_stopped_answer_rebuilds_without_a_dangling_tool_use() {
    let first = Rig::with(
        script(vec![
            call_turn("Trying.", &[("c1", "render_preview", "{}"), ("c2", "render_preview", "{}")]),
            say_turn("never"),
        ]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            let gate = Arc::new(tokio::sync::Notify::new());
            server.add_fixed(tool("render_preview", "Render preview", "query", true), Outcome::text("r").held(&gate));
            server
        },
    );
    first.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the call", || first.server.called("render_preview") == 1);
    first.host.stop();
    first.until_done(0.0);
    let second = continued(first.sink.entries(), vec![say_turn("ok")]);
    second.say("again", 0.0);
    let messages = &second.llm.requests()[0].messages;
    let uses = messages.iter().flat_map(|m| &m.content).filter(|b| matches!(b, ContentBlock::ToolUse { .. })).count();
    let results =
        messages.iter().flat_map(|m| &m.content).filter(|b| matches!(b, ContentBlock::ToolResult { .. })).count();
    assert_eq!((uses, results), (2, 2));
}

#[test]
fn a_revoked_conversation_can_be_continued_with_a_fresh_session() {
    let entries = vec![
        Entry::user("go", vec![]),
        Entry::assistant(vec![ContentBlock::text("working")]),
        Entry::session(
            omuzaki_ops_api::SessionId(9),
            omuzaki_agent_api::entry::SessionNote::Ended(omuzaki_ops_api::EndReason::Revoked {
                by: person(),
                credential: true,
            }),
        ),
    ];
    let rig = continued(entries, vec![say_turn("back")]);
    let events = rig.say("continue", 0.0);
    assert_eq!(text_of(&events), "back");
}
