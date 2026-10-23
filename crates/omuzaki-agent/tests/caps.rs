//! Tool calls in order and the per-message caps (D2.4, D2.7).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use common::*;
use omuzaki_agent::Caps;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::event::{AssistantEvent, Cap};
use omuzaki_agent_api::message::UserMessage;
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_llm_api::message::{ContentBlock, ToolChoice};
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_api::usage::Usage;
use omuzaki_llm_mock::ScriptTurn;

fn limits(events: &[AssistantEvent]) -> Vec<Cap> {
    events.iter().filter_map(|e| if let AssistantEvent::Limit(c) = e { Some(*c) } else { None }).collect()
}

fn options(caps: Caps) -> RigOptions {
    RigOptions { caps, ..RigOptions::default() }
}

fn last_text(rig: &Rig, request: usize) -> String {
    let message = rig.llm.requests()[request].messages.last().unwrap().clone();
    format!("{:?}", message.content)
}

#[test]
fn three_calls_of_one_turn_run_in_the_emitted_order_one_at_a_time_and_are_answered_in_one_message() {
    let rig = Rig::new(script(vec![
        call_turn(
            "",
            &[
                ("c1", "render_preview", "{\"n\":1}"),
                ("c2", "face_set_profile", "{\"n\":2}"),
                ("c3", "render_preview", "{\"n\":3}"),
            ],
        ),
        say_turn("ok"),
    ]));
    rig.say("go", 0.0);
    let order: Vec<i64> = rig
        .server
        .calls()
        .iter()
        .filter(|(name, _)| name != "tx_begin" && name != "tx_commit")
        .map(|(_, args)| args["n"].as_i64().unwrap())
        .collect();
    assert_eq!(order, [1, 2, 3]);
    assert_eq!(rig.server.log.lock().unwrap().max_in_flight, 1);
    let last = rig.llm.requests()[1].messages.last().unwrap().clone();
    let ids: Vec<String> = last
        .content
        .iter()
        .filter_map(|b| if let ContentBlock::ToolResult { id, .. } = b { Some(id.clone()) } else { None })
        .collect();
    assert_eq!(ids, ["c1", "c2", "c3"]);
}

#[test]
fn the_turn_cap_is_hit_at_exactly_its_number() {
    let caps = Caps { turns: 3, ..Caps::default() };
    let rig = Rig::with(
        script(vec![
            call_turn("", &[("c1", "render_preview", "{}")]),
            call_turn("", &[("c2", "render_preview", "{}")]),
            call_turn("", &[("c3", "render_preview", "{}")]),
            say_turn("Here is what I did."),
        ]),
        options(caps),
        standard_server,
    );
    let events = rig.say("go", 0.0);
    assert_eq!(limits(&events), [Cap::Turns]);
    let requests = rig.llm.requests();
    assert_eq!(requests.len(), 4, "three turns, then the one last turn");
    assert!(requests[..3].iter().all(|r| r.tool_choice == ToolChoice::Auto));
    assert_eq!(requests[3].tool_choice, ToolChoice::None);
    assert!(last_text(&rig, 3).contains("limit reached (3 turns): say what you did and what is left"));
    let limit_at = events.iter().position(|e| matches!(e, AssistantEvent::Limit(_))).unwrap();
    assert_eq!(events[..limit_at].iter().filter(|e| matches!(e, AssistantEvent::ToolResult { .. })).count(), 3);
}

#[test]
fn an_answer_that_ends_on_its_last_allowed_turn_hits_no_cap() {
    let caps = Caps { turns: 2, ..Caps::default() };
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "render_preview", "{}")]), say_turn("done")]),
        options(caps),
        standard_server,
    );
    assert!(limits(&rig.say("go", 0.0)).is_empty());
}

#[test]
fn the_tool_call_cap_skips_the_calls_past_it_and_answers_them() {
    let caps = Caps { tool_calls: 2, ..Caps::default() };
    let rig = Rig::with(
        script(vec![
            call_turn(
                "",
                &[("c1", "render_preview", "{}"), ("c2", "render_preview", "{}"), ("c3", "render_preview", "{}")],
            ),
            say_turn("summary"),
        ]),
        options(caps),
        standard_server,
    );
    let events = rig.say("go", 0.0);
    assert_eq!(limits(&events), [Cap::ToolCalls]);
    assert_eq!(rig.server.called("render_preview"), 2);
    let requests = rig.llm.requests();
    assert_eq!(requests[1].tool_choice, ToolChoice::None);
    let text = last_text(&rig, 1);
    assert!(text.contains("not run") && text.contains("limit reached (2 tool calls)"), "{text}");
}

#[test]
fn the_token_cap_is_hit_when_the_counters_reach_it() {
    let hundred = |tool: &str, id: &str| {
        ScriptTurn::new(vec![
            LlmEvent::start("m"),
            LlmEvent::tool_use_start(id, tool),
            LlmEvent::tool_use_end(id),
            LlmEvent::Usage(Usage::new(60, 40)),
            LlmEvent::Stop(StopReason::ToolUse),
        ])
    };
    let caps = Caps { tokens: 200, ..Caps::default() };
    let rig = Rig::with(
        script(vec![hundred("render_preview", "c1"), hundred("render_preview", "c2"), say_turn("done")]),
        RigOptions { caps, prices: free_prices(), ..RigOptions::default() },
        standard_server,
    );
    let events = rig.say("go", 0.0);
    assert_eq!(limits(&events), [Cap::Tokens]);
    assert_eq!(rig.llm.requests().len(), 3);
}

#[test]
fn the_wall_time_cap_counts_the_models_time_and_not_a_wait_for_the_person() {
    let slow = |id: &str| {
        ScriptTurn::new(vec![
            LlmEvent::start("m"),
            LlmEvent::TextDelta("thinking".into()),
            LlmEvent::tool_use_start(id, "render_preview"),
            LlmEvent::tool_use_end(id),
            LlmEvent::Usage(Usage::new(1, 1)),
            LlmEvent::Stop(StopReason::ToolUse),
        ])
        .with_delay_ms(1_500)
    };
    let caps = Caps { wall_s: 5, ..Caps::default() };
    let rig = Rig::with(script(vec![slow("c1"), say_turn("done")]), options(caps), standard_server);
    let events = rig.say("go", 0.0);
    assert_eq!(limits(&events), [Cap::WallTime]);

    // The same time spent waiting for a person is not counted.
    let ask = "{\"question\":\"Which?\"}";
    let caps = Caps { wall_s: 60, ..Caps::default() };
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "ask_person", ask)]), say_turn("done")]),
        options(caps),
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("the question", || {
        while let Some(e) = rig.rx.try_recv() {
            seen.push(e);
        }
        seen.iter().any(|e| matches!(e, AssistantEvent::Question { .. }))
    });
    rig.clock.advance(Seconds(100.0));
    let id =
        seen.iter().find_map(|e| if let AssistantEvent::Question { id, .. } = e { Some(*id) } else { None }).unwrap();
    rig.host.answer(id, "this one".into()).unwrap();
    let rest = rig.until_done(0.0);
    assert!(limits(&rest).is_empty(), "100 s of waiting did not count toward the 60 s");
    assert!(rig.clock.now().0 >= 100.0);
}
