//! The context builder and compaction (#1387): a byte-stable prefix, the per-message context block, and what happens
//! when the window fills.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use std::sync::Arc;

use base64::Engine as _;
use common::*;
use omuzaki_agent::Language;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_llm_api::LlmClient as _;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, ImageOrigin, Role, ToolChoice};
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_api::usage::Usage;
use omuzaki_llm_mock::{Injected, ScriptTurn};
use rmcp::model::ResourceContents;

fn window(w: u64) -> RigOptions {
    RigOptions { window: Some(w), prices: free_prices(), ..RigOptions::default() }
}

/// A turn that calls a query tool and reports `prompt` + 10 tokens used.
fn heavy_turn(id: &str, prompt: u64) -> ScriptTurn {
    ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::tool_use_start(id, "render_preview"),
        LlmEvent::tool_use_end(id),
        LlmEvent::Usage(Usage::new(prompt, 10)),
        LlmEvent::Stop(StopReason::ToolUse),
    ])
}

fn texts(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
}

fn prefix_json(request: &omuzaki_llm_api::message::TurnRequest) -> String {
    serde_json::to_string(&(&request.system, &request.tools)).unwrap()
}

#[test]
fn the_system_and_tools_prefix_is_byte_identical_across_a_hundred_turns() {
    let mut turns: Vec<ScriptTurn> =
        (0..99).map(|i| call_turn("", &[(&format!("c{i}"), "render_preview", "{}")])).collect();
    turns.push(say_turn("done"));
    let caps = omuzaki_agent::Caps { turns: 200, tool_calls: 200, ..Default::default() };
    let rig = Rig::with(script(turns), RigOptions { caps, ..RigOptions::default() }, standard_server);
    rig.say("go", 0.0);
    let requests = rig.llm.requests();
    assert_eq!(requests.len(), 100);
    let first = prefix_json(&requests[0]);
    assert!(requests.iter().all(|r| prefix_json(r) == first));
    assert!(requests.iter().all(|r| r.cache.conversation));
    let system = &requests[0].system;
    assert_eq!(system.len(), 3);
    assert!(system[2].cache_after && !system[0].cache_after && !system[1].cache_after);
    assert_eq!(system[1].text, "Server instructions.");
}

#[test]
fn the_tools_are_sorted_have_no_transaction_tool_and_include_the_four_host_tools() {
    let rig = Rig::new(script(vec![say_turn("hi")]));
    rig.say("go", 0.0);
    let names: Vec<String> = rig.llm.requests()[0].tools.iter().map(|t| t.name.clone()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert!(names.iter().all(|n| !n.starts_with("tx_")));
    for host in ["ask_person", "update_plan", "discard_changes", "read_resource"] {
        assert!(names.contains(&host.to_owned()), "{host}");
    }
    assert!(names.contains(&"render_preview".to_owned()));
}

#[test]
fn the_language_line_follows_the_setting() {
    let rig = Rig::with(
        script(vec![say_turn("hi")]),
        RigOptions { language: Some(Language::new("German", "de")), ..RigOptions::default() },
        standard_server,
    );
    rig.say("go", 0.0);
    let facts = &rig.llm.requests()[0].system[2].text;
    assert!(facts.contains("Reply in German (de); tool names, arguments and ids stay English"), "{facts}");
    assert!(
        facts.contains("Model: m.") && facts.contains("30 turns, 60 tool calls, 600 s of wall time and 400000 tokens")
    );
}

fn server_with_context(sessions: &Arc<FakeSessions>) -> ScriptedServer {
    let server = standard_server(sessions);
    let described = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    server.add(tool("project_describe", "Project describe", "query", true), move |_, n| {
        let _ = &described;
        Outcome::text(if n < 2 { "Project: two characters." } else { "Project: three characters." })
    });
    server.add(tool("session_info", "Session info", "builtin", true), |_, n| {
        let mut out = Outcome::text("session");
        if n == 1 {
            out.result.structured_content =
                Some(serde_json::json!({"since_your_last_call": {"changed": "head width, by sam"}}));
        }
        out
    });
    server.resources.lock().unwrap().insert(
        "omuzaki://ui/context".into(),
        ResourceContents::TextResourceContents {
            uri: "omuzaki://ui/context".into(),
            mime_type: None,
            text: "selection: head".into(),
            meta: None,
        },
    );
    server
}

fn user_message_blocks(rig: &Rig, request: usize) -> Vec<ContentBlock> {
    let messages = &rig.llm.requests()[request].messages;
    messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, ContentBlock::Text(_))))
        .unwrap()
        .content
        .clone()
}

#[test]
fn each_message_carries_a_context_block_with_the_summary_in_full_once_and_again_only_when_it_changes() {
    let rig = Rig::with(
        script(vec![say_turn("a"), say_turn("b"), say_turn("c")]),
        RigOptions::default(),
        server_with_context,
    );
    rig.say("first", 0.0);
    rig.say("second", 0.0);
    rig.say("third", 0.0);
    let first = texts(&user_message_blocks(&rig, 0));
    assert!(first.contains("<data source=\"omuzaki://ui/context\">selection: head</data>"), "{first}");
    assert!(first.contains("<data source=\"project_describe\">Project: two characters.</data>"));
    assert!(first.ends_with("first"), "the context comes before the person's text: {first}");
    let second = texts(&rig.llm.requests()[1].messages[2].content);
    assert!(!second.contains("Project:"), "unchanged, so not repeated: {second}");
    assert!(second.contains("head width, by sam"), "the heads-up: {second}");
    let third = texts(&rig.llm.requests()[2].messages[4].content);
    assert!(third.contains("Project: three characters."), "changed, so repeated: {third}");
}

#[test]
fn the_stored_user_entry_has_only_what_the_person_typed() {
    let rig = Rig::with(script(vec![say_turn("a")]), RigOptions::default(), server_with_context);
    rig.say("first", 0.0);
    let stored =
        rig.sink.entries().into_iter().find_map(|e| if let Entry::User { text, .. } = e { Some(text) } else { None });
    assert_eq!(stored.as_deref(), Some("first"));
}

#[test]
fn a_viewport_screenshot_is_a_render_never_a_camera_frame() {
    let rig = Rig::with(script(vec![say_turn("a")]), RigOptions::default(), |sessions| {
        let server = standard_server(sessions);
        server.resources.lock().unwrap().insert(
            "omuzaki://ui/context".into(),
            ResourceContents::BlobResourceContents {
                uri: "omuzaki://ui/context".into(),
                mime_type: Some("image/png".into()),
                blob: base64::engine::general_purpose::STANDARD.encode([137u8, 80, 78, 71]),
                meta: None,
            },
        );
        server
    });
    rig.say("go", 0.0);
    let blocks = user_message_blocks(&rig, 0);
    let origins: Vec<ImageOrigin> = blocks
        .iter()
        .filter_map(|b| if let ContentBlock::Image { origin, .. } = b { Some(*origin) } else { None })
        .collect();
    assert_eq!(origins, [ImageOrigin::Render]);
}

#[test]
fn a_result_that_arrived_after_stop_appears_once_in_the_next_message() {
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
    rig.say("next", 0.0);
    rig.say("and again", 0.0);
    let note = "[your call `render_preview` finished after the stop: applied]";
    assert!(
        texts(&user_message_blocks(&rig, 1)).contains("<data source=\"late_result\">"),
        "server-derived text is data"
    );
    assert!(texts(&user_message_blocks(&rig, 1)).contains(note));
    assert!(!texts(&user_message_blocks(&rig, 2)).contains(note), "once");
}

#[test]
fn a_prompt_past_three_quarters_of_the_window_is_compacted_before_the_next_turn() {
    for (prompt, compacts) in [(14_990, false), (14_991, true)] {
        let rig = Rig::with(
            script(vec![heavy_turn("c1", prompt), say_turn("SUMMARY of the work"), say_turn("done")]),
            window(20_000),
            standard_server,
        );
        let events = rig.say("make her face more oval", 0.0);
        let requests = rig.llm.requests();
        if !compacts {
            assert_eq!(requests.len(), 2, "{prompt}");
            continue;
        }
        assert_eq!(requests.len(), 3);
        let summary = &requests[1];
        assert_eq!(summary.tool_choice, ToolChoice::None);
        assert_eq!(summary.max_output, 2_000);
        assert_eq!(summary.messages.len(), 1, "the old history is not in the summary turn");
        let after = &requests[2];
        assert_eq!(after.messages.len(), 1);
        let text = texts(&after.messages[0].content);
        assert!(text.contains("SUMMARY of the work") && text.ends_with("make her face more oval"), "{text}");
        assert_eq!(prefix_json(summary), prefix_json(after), "the same prefix");
        assert!(!text_of(&events).contains("SUMMARY"), "the summary is not shown as the answer");
        assert!(
            rig.sink
                .entries()
                .iter()
                .any(|e| matches!(e, Entry::Compacted { message, .. } if message.contains("SUMMARY")))
        );
        assert_eq!(rig.sessions.charges().len(), 3, "the summary turn is charged like any turn");
    }
}

fn too_long() -> ScriptTurn {
    ScriptTurn::new(vec![LlmEvent::start("m")]).with_error(Injected::new(0, LlmError::context_too_long(20_000)))
}

#[test]
fn a_too_long_prompt_compacts_and_retries_once_and_a_second_failure_is_context_full() {
    let rig = Rig::with(script(vec![too_long(), say_turn("SUMMARY"), say_turn("done")]), window(20_000), |s| {
        standard_server(s)
    });
    let events = rig.say("go", 0.0);
    assert_eq!(text_of(&events), "done");
    let requests = rig.llm.requests();
    assert_eq!(requests.len(), 3);
    assert!(texts(&requests[1].messages[0].content).len() <= 8_000, "the summary input is at most 40 % of the window");

    let rig = Rig::with(script(vec![too_long(), say_turn("SUMMARY"), too_long()]), window(20_000), standard_server);
    let events = rig.say("go", 0.0);
    assert!(events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::ContextFull))), "{events:?}");
}

#[test]
fn when_the_mandatory_part_cannot_fit_nothing_is_sent_and_the_answer_ends_with_context_full() {
    let rig = Rig::with(script(vec![heavy_turn("c1", 15_001), say_turn("SUMMARY")]), window(20_000), standard_server);
    let huge = "x".repeat(19_000);
    let events = rig.say(&huge, 0.0);
    assert!(events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::ContextFull))), "{events:?}");
    assert_eq!(rig.llm.requests().len(), 2, "the summary turn ran; the rebuilt request was never sent");
}

#[test]
fn a_model_with_a_small_window_is_unsupported_for_the_assistant() {
    let rig = Rig::with(script(vec![say_turn("hi")]), window(9_999), standard_server);
    let events = rig.say("go", 0.0);
    let unsupported =
        events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::Llm(LlmError::Unsupported(_)))));
    assert!(unsupported, "{events:?}");
    assert!(rig.llm.requests().is_empty());
}

#[test]
fn a_prefix_of_forty_percent_or_more_of_the_window_is_unsupported() {
    // The prefix is measured from a first run on a roomy window, then the window is set just under 2.5 times it.
    let probe = Rig::new(script(vec![say_turn("hi")]));
    probe.say("go", 0.0);
    let first = &probe.llm.requests()[0];
    let prefix_only = omuzaki_llm_api::message::TurnRequest::new(vec![], 2_048)
        .with_system(first.system.clone())
        .with_tools(first.tools.clone());
    let info = probe.llm.info().clone();
    let prefix = omuzaki_llm_api::usage::Ledger::new().prompt_estimate(&prefix_only, &info);
    let window_tokens = (prefix * 5 / 2).max(10_000);
    assert!(prefix * 5 >= window_tokens * 2 || window_tokens == 10_000);
    let tight = prefix * 5 / 2 - 1;
    if tight >= 10_000 {
        let rig = Rig::with(script(vec![say_turn("hi")]), window(tight), standard_server);
        let events = rig.say("go", 0.0);
        assert!(
            events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::Llm(LlmError::Unsupported(_))))),
            "{events:?}"
        );
        let roomy = Rig::with(script(vec![say_turn("hi")]), window(tight + 2), standard_server);
        assert!(!roomy.say("go", 0.0).iter().any(|e| matches!(e, AssistantEvent::Error(_))));
    } else {
        assert!(prefix < 4_000, "a prefix under 4,000 tokens never trips the 40 % rule at 10,000");
    }
}

#[test]
fn the_previous_answers_plan_is_not_carried_into_the_next_answers_compaction() {
    let item = "{\"items\":[{\"text\":\"STALE-PLAN-ITEM\",\"status\":\"doing\"}]}";
    let rig = Rig::with(
        script(vec![
            call_turn("", &[("c1", "update_plan", item)]),
            say_turn("first done"),
            heavy_turn("c2", 15_001),
            say_turn("SUMMARY"),
            say_turn("second done"),
        ]),
        window(20_000),
        standard_server,
    );
    rig.say("first", 0.0);
    rig.say("second", 0.0);
    let summary_input = texts(&rig.llm.requests()[3].messages[0].content);
    assert!(!summary_input.contains("STALE-PLAN-ITEM"), "{summary_input}");
}

#[test]
fn a_render_the_compaction_keeps_is_stored_so_a_resume_can_put_it_back() {
    let rig = Rig::with(
        script(vec![heavy_turn("c1", 15_001), say_turn("SUMMARY"), say_turn("done")]),
        window(20_000),
        |sessions| {
            let server = standard_server(sessions);
            server
                .add_fixed(tool("render_preview", "Render preview", "query", true), Outcome::image(&[137, 80, 78, 71]));
            server
        },
    );
    rig.say("go", 0.0);
    let rebuilt = &rig.llm.requests()[2].messages[0];
    assert!(rebuilt.content.iter().any(|b| matches!(b, ContentBlock::Image { .. })), "the render was kept");
    let stored = rig
        .sink
        .entries()
        .into_iter()
        .find_map(|e| if let Entry::Compacted { render, .. } = e { render } else { None });
    assert!(stored.is_some(), "and named in the compaction entry");
}

#[test]
fn a_render_that_cannot_be_stored_fails_the_compaction_and_leaves_the_history_alone() {
    let options = RigOptions { fail_blobs: true, ..window(20_000) };
    let rig = Rig::with(script(vec![heavy_turn("c1", 15_001), say_turn("SUMMARY"), say_turn("done")]), options, |s| {
        let server = standard_server(s);
        server.add_fixed(tool("render_preview", "Render preview", "query", true), Outcome::image(&[137, 80, 78, 71]));
        server
    });
    let events = rig.say("go", 0.0);
    assert!(events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::Store(_)))), "{events:?}");
    assert!(
        !rig.sink.entries().iter().any(|e| matches!(e, Entry::Compacted { .. })),
        "nothing was compacted, so a resume still sees the whole history"
    );
}
