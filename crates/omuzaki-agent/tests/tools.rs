//! The host-local tools (#1371): `ask_person`, `update_plan`, `discard_changes` and `read_resource`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use common::*;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::event::{AssistantEvent, PlanStatus, QuestionId};
use omuzaki_agent_api::message::UserMessage;
use omuzaki_llm_api::message::{ContentBlock, ImageMime};
use rmcp::model::ResourceContents;

fn results_of_request(rig: &Rig, request: usize) -> Vec<ContentBlock> {
    rig.llm.requests()[request]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|b| if let ContentBlock::ToolResult { blocks, .. } = b { Some(blocks.clone()) } else { None })
        .flatten()
        .collect()
}

fn question_of(events: &[AssistantEvent]) -> Vec<(QuestionId, String, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| {
            if let AssistantEvent::Question { id, text, choices, .. } = e {
                Some((*id, text.clone(), choices.clone()))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn ask_person_emits_one_question_and_the_answer_reaches_the_model() {
    let rig = Rig::new(script(vec![
        call_turn("", &[("c1", "ask_person", "{\"question\":\"Which jaw?\",\"choices\":[\"narrow\",\"wide\"]}")]),
        say_turn("ok"),
    ]));
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("the question", || {
        while let Some(e) = rig.rx.try_recv() {
            seen.push(e);
        }
        !question_of(&seen).is_empty()
    });
    let asked = question_of(&seen);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1, "Which jaw?");
    assert_eq!(asked[0].2, ["narrow", "wide"]);
    assert_eq!(rig.llm.requests().len(), 1, "the model waits for the answer");
    rig.host.answer(asked[0].0, "narrow".into()).unwrap();
    assert!(rig.host.answer(asked[0].0, "again".into()).is_err(), "a question is answered once");
    seen.extend(rig.until_done(0.0));
    assert_eq!(question_of(&seen).len(), 1);
    assert_eq!(results_of_request(&rig, 1), vec![ContentBlock::text("<data source=\"ask_person\">narrow</data>")]);
}

#[test]
fn stop_ends_an_open_question_and_no_question_follows() {
    let rig =
        Rig::new(script(vec![call_turn("", &[("c1", "ask_person", "{\"question\":\"Which?\"}")]), say_turn("ok")]));
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("the question", || {
        while let Some(e) = rig.rx.try_recv() {
            seen.push(e);
        }
        !question_of(&seen).is_empty()
    });
    rig.host.stop();
    let rest = rig.until_done(0.0);
    assert!(question_of(&rest).is_empty());
    rig.say("again", 0.0);
    let history = &rig.llm.requests()[1].messages;
    let text = format!("{history:?}");
    assert!(text.contains("stopped by the person"), "{text}");
}

#[test]
fn a_headless_run_answers_with_the_configured_reply() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "ask_person", "{\"question\":\"Which?\"}")]), say_turn("ok")]),
        RigOptions { person_reply: Some("the wide one".into()), ..RigOptions::default() },
        standard_server,
    );
    rig.say("go", 0.0);
    assert_eq!(
        results_of_request(&rig, 1),
        vec![ContentBlock::text("<data source=\"ask_person\">the wide one</data>")]
    );
}

#[test]
fn a_plan_of_twenty_items_emits_one_plan_and_twenty_one_is_refused() {
    let item = "{\"text\":\"step\",\"status\":\"doing\"}";
    let twenty = format!("{{\"items\":[{}]}}", vec![item; 20].join(","));
    let twenty_one = format!("{{\"items\":[{}]}}", vec![item; 21].join(","));
    let rig = Rig::new(script(vec![
        call_turn("", &[("c1", "update_plan", &twenty_one), ("c2", "update_plan", &twenty)]),
        say_turn("ok"),
    ]));
    let events = rig.say("go", 0.0);
    let plans: Vec<_> =
        events.iter().filter_map(|e| if let AssistantEvent::Plan(p) = e { Some(p.len()) } else { None }).collect();
    assert_eq!(plans, [20]);
    let results = results_of_request(&rig, 1);
    assert!(matches!(&results[0], ContentBlock::Text(t) if t.contains("at most 20")), "{results:?}");
    assert_eq!(
        events.iter().filter(|e| matches!(e, AssistantEvent::Plan(p) if p[0].status == PlanStatus::Doing)).count(),
        1
    );
}

#[test]
fn discard_changes_rolls_back_the_open_transaction_and_does_nothing_without_one() {
    let rig = Rig::new(script(vec![
        call_turn("", &[("c1", "face_set_profile", "{}"), ("c2", "discard_changes", "{}")]),
        say_turn("ok"),
    ]));
    rig.say("go", 0.0);
    assert_eq!(rig.server.called("tx_rollback"), 1);
    assert_eq!(rig.server.called("tx_commit"), 0, "a discarded step is not committed");

    let rig = Rig::new(script(vec![call_turn("", &[("c1", "discard_changes", "{}")]), say_turn("ok")]));
    rig.say("go", 0.0);
    assert_eq!(rig.server.called("tx_rollback"), 0);
    assert!(matches!(&results_of_request(&rig, 1)[0], ContentBlock::Text(t) if t.contains("no changes")));
}

#[test]
fn read_resource_returns_text_as_text_and_images_as_images() {
    let rig = Rig::new(script(vec![
        call_turn(
            "",
            &[
                ("c1", "read_resource", "{\"uri\":\"omuzaki://character/main\"}"),
                ("c2", "read_resource", "{\"uri\":\"omuzaki://render/front\"}"),
            ],
        ),
        say_turn("ok"),
    ]));
    {
        use base64::Engine;
        let mut resources = rig.server.resources.lock().unwrap();
        resources.insert(
            "omuzaki://character/main".into(),
            ResourceContents::TextResourceContents {
                uri: "omuzaki://character/main".into(),
                mime_type: None,
                text: "{\"id\":\"main\"}".into(),
                meta: None,
            },
        );
        resources.insert(
            "omuzaki://render/front".into(),
            ResourceContents::BlobResourceContents {
                uri: "omuzaki://render/front".into(),
                mime_type: Some("image/png".into()),
                blob: base64::engine::general_purpose::STANDARD.encode([137u8, 80, 78, 71]),
                meta: None,
            },
        );
    }
    rig.say("go", 0.0);
    let results = results_of_request(&rig, 1);
    assert_eq!(results[0], ContentBlock::text("<data source=\"omuzaki://character/main\">{\"id\":\"main\"}</data>"));
    assert!(matches!(&results[1], ContentBlock::Image { mime: ImageMime::Png, bytes, origin, .. }
        if bytes == &[137, 80, 78, 71] && *origin == omuzaki_llm_api::message::ImageOrigin::Render));
}

#[test]
fn none_of_the_host_tools_opens_the_transaction() {
    let rig = Rig::new(script(vec![
        call_turn(
            "",
            &[
                ("c1", "update_plan", "{\"items\":[]}"),
                ("c2", "discard_changes", "{}"),
                ("c3", "read_resource", "{\"uri\":\"x\"}"),
            ],
        ),
        say_turn("ok"),
    ]));
    rig.say("go", 0.0);
    assert_eq!(rig.server.called("tx_begin"), 0);
}
