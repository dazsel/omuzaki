//! The host's MCP prompt cache, `refresh_prompts` and `prompt` (#1134, design §7).

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::*;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_agent_api::prompts::{PromptArgInfo, PromptInfo, PromptsState};
use omuzaki_agent_api::Assistant;
use omuzaki_core::time::{Clock, Seconds};
use rmcp::model::{Prompt, PromptArgument};

fn prompt(name: &str, title: Option<&str>) -> Prompt {
    let arguments = vec![PromptArgument::new("character").with_description("Which character goes live").with_required(true)];
    let p = Prompt::new(name, Some("A guided start"), Some(arguments));
    match title {
        Some(t) => p.with_title(t),
        None => p,
    }
}

fn rig_with_prompts(prompts: Vec<(Prompt, &str)>) -> Rig {
    let rig = Rig::new(script(vec![say_turn("ok")]));
    *rig.server.prompts.lock().unwrap() = prompts.into_iter().map(|(p, t)| (p, t.to_owned())).collect();
    // The first fetch happened at start, before the table was filled: ask again.
    rig.host.refresh_prompts();
    rig
}

fn ready(rig: &Rig) -> Vec<PromptInfo> {
    rig.wait_for("Ready", || matches!(rig.host.prompts(), PromptsState::Ready(_)));
    match rig.host.prompts() {
        PromptsState::Ready(list) => list,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_list_is_cached_sorted_and_a_second_read_makes_no_mcp_call() {
    let rig = rig_with_prompts(vec![(prompt("b_second", None), "x"), (prompt("a_first", Some("Set up my first stream")), "y")]);
    let list = ready(&rig);
    assert_eq!(list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["a_first", "b_second"]);
    assert_eq!(list[0].title, "Set up my first stream");
    assert_eq!(list[1].title, "b_second", "the title falls back to the name");
    assert_eq!(list[0].arguments, vec![PromptArgInfo::new("character", "Which character goes live", true)]);
    let lists = rig.server.log.lock().unwrap().prompt_lists;
    let _ = (rig.host.prompts(), rig.host.prompts());
    assert_eq!(rig.server.log.lock().unwrap().prompt_lists, lists);
}

#[test]
fn a_list_changed_notification_shows_in_the_next_read() {
    let rig = rig_with_prompts(vec![(prompt("one", None), "x")]);
    assert_eq!(ready(&rig).len(), 1);
    rig.server.prompts.lock().unwrap().push((prompt("two", None), "y".into()));
    let peer = rig.server_peer.lock().unwrap().clone().expect("server peer");
    rig.rt.block_on(peer.notify_prompt_list_changed()).unwrap();
    rig.wait_for("the new list", || matches!(rig.host.prompts(), PromptsState::Ready(l) if l.len() == 2));
}

#[test]
fn a_failed_fetch_is_cached_and_refresh_gives_loading_then_ready_with_no_answer_run() {
    let rig = Rig::new(script(vec![]));
    *rig.server.prompt_failure.lock().unwrap() = Some("boom".into());
    rig.host.refresh_prompts();
    rig.wait_for("Failed", || matches!(rig.host.prompts(), PromptsState::Failed(HostError::Mcp(_))));
    *rig.server.prompt_failure.lock().unwrap() = None;
    rig.server.prompts.lock().unwrap().push((prompt("one", None), "x".into()));
    rig.host.refresh_prompts();
    assert_eq!(rig.host.prompts(), PromptsState::Loading, "Loading at once");
    assert_eq!(ready(&rig).len(), 1);
    assert!(rig.llm.requests().is_empty() && rig.sink.entries().iter().all(|e| !matches!(e, omuzaki_agent_api::entry::Entry::User { .. })));
}

#[test]
fn the_list_is_fetched_again_after_each_answer() {
    let rig = rig_with_prompts(vec![(prompt("one", None), "x")]);
    ready(&rig);
    let before = rig.server.log.lock().unwrap().prompt_lists;
    rig.say("hi", 0.0);
    rig.wait_for("the refetch", || rig.server.log.lock().unwrap().prompt_lists > before);
}

#[test]
fn arguments_reach_the_server_unchanged_and_the_text_comes_back() {
    let rig = rig_with_prompts(vec![(prompt("set_up", None), "Set up Mira's stream.")]);
    let args = BTreeMap::from([("character".to_owned(), "  Mira ".to_owned())]);
    assert_eq!(rig.host.prompt("set_up", args), Ok("Set up Mira's stream.".to_owned()));
    let gets = rig.server.log.lock().unwrap().prompt_gets.clone();
    assert_eq!(gets, vec![("set_up".to_owned(), serde_json::json!({"character": "  Mira "}))]);
    assert!(rig.llm.requests().is_empty(), "a prompt sends nothing to the model");
    assert!(rig.sink.entries().iter().all(|e| !matches!(e, omuzaki_agent_api::entry::Entry::User { .. })));
}

#[test]
fn sixteen_kib_passes_and_one_more_byte_fails() {
    let rig = rig_with_prompts(vec![(prompt("fits", None), &"a".repeat(16_384)), (prompt("too_long", None), &"a".repeat(16_385))]);
    assert_eq!(rig.host.prompt("fits", BTreeMap::new()).map(|t| t.len()), Ok(16_384));
    assert_eq!(rig.host.prompt("too_long", BTreeMap::new()), Err(HostError::Mcp("prompt text too long".into())));
}

#[test]
fn the_servers_own_refusal_comes_back_as_mcp_text() {
    let rig = rig_with_prompts(vec![]);
    let error = rig.host.prompt("nope", BTreeMap::new()).unwrap_err();
    assert!(matches!(&error, HostError::Mcp(text) if text.contains("unknown prompt nope")), "{error:?}");
}

#[test]
fn a_two_second_stall_is_a_timeout() {
    let rig = rig_with_prompts(vec![(prompt("slow", None), "x")]);
    let gate = Arc::new(tokio::sync::Notify::new());
    *rig.server.prompt_gate.lock().unwrap() = Some(Arc::clone(&gate));
    let clock = Arc::clone(&rig.clock);
    let mover = std::thread::spawn(move || {
        for _ in 0..500 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            clock.advance(Seconds(0.01));
            if clock.now().0 > 3.0 {
                break;
            }
        }
    });
    assert_eq!(rig.host.prompt("slow", BTreeMap::new()), Err(HostError::Mcp("timed out".into())));
    let waited = rig.clock.now().0;
    assert!((2.0..2.5).contains(&waited), "{waited}");
    mover.join().unwrap();
    gate.notify_waiters();
}

#[test]
fn prompts_can_be_read_while_an_answer_runs() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "render_preview", "{}")]), say_turn("ok")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(tool("render_preview", "Render preview", "query", true), Outcome::text("r").held(&gate));
            server
        },
    );
    rig.server.prompts.lock().unwrap().push((prompt("p", None), "text".into()));
    rig.host.send(UserMessage::new("go")).unwrap();
    rig.wait_for("the call", || rig.server.called("render_preview") == 1);
    assert_eq!(rig.host.prompt("p", BTreeMap::new()), Ok("text".to_owned()));
    gate.notify_one();
    rig.until_done(0.0);
}
