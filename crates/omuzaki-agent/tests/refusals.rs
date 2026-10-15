//! What each refusal does (#1111, D2.6): waiting for an approval, a paused session, a rate limit, and the refusals that
//! go to the model unchanged.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use std::sync::Arc;

use common::*;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_core::time::Clock;
use omuzaki_llm_api::message::ContentBlock;
use omuzaki_ops_api::{Actor, ApprovalId, ErrorDetail, Need, PauseReason, SessionState};

fn last_results(rig: &Rig, request: usize) -> Vec<String> {
    rig.llm.requests()[request]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult { blocks, .. } => Some(
                blocks
                    .iter()
                    .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.clone()) } else { None })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

fn queued() -> ErrorDetail {
    ErrorDetail::Queued { approval: ApprovalId(7), need: Need::Person { reason: "r".into() } }
}

/// A server whose `face_set_profile` answers from a list, one per call.
fn edit_server(answers: Vec<Outcome>) -> impl FnOnce(&Arc<FakeSessions>) -> ScriptedServer {
    move |sessions| {
        let server = standard_server(sessions);
        server.add(tool("face_set_profile", "Face profile", "edit", false), move |_, n| {
            answers.get(n).or_else(|| answers.last()).cloned().unwrap_or_else(|| Outcome::text("applied"))
        });
        server
    }
}

#[test]
fn a_queued_approval_waits_and_gives_the_model_the_resolution_as_the_calls_result() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        edit_server(vec![Outcome::refusal("waiting", &queued())]),
    );
    rig.sessions.queue_approval(7);
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("Waiting", || {
        while let Some(e) = rig.rx.try_recv() {
            seen.push(e);
        }
        seen.iter().any(|e| matches!(e, AssistantEvent::Waiting { approval, .. } if approval.0 == 7))
    });
    Rig::wait_for("the wait", || rig.server.called("approval_wait") >= 1);
    assert_eq!(rig.server.called("face_set_profile"), 1, "nothing is re-sent while the approval is pending");
    rig.sessions.resolve_approvals();
    rig.until_done(0.0);
    assert_eq!(last_results(&rig, 1), ["<data source=\"face_set_profile\">approval applied</data>"]);
    assert_eq!(rig.server.called("face_set_profile"), 1);
}

#[test]
fn a_rejection_reaches_the_model_with_its_comment() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        |sessions| {
            let server = edit_server(vec![Outcome::refusal("waiting", &queued())])(sessions);
            server.add_fixed(
                tool("approval_wait", "approval.wait", "builtin", false),
                Outcome::text("the person rejected this: 'too wide'"),
            );
            server
        },
    );
    rig.sessions.queue_approval(7);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the wait", || rig.server.called("approval_wait") >= 1);
    rig.sessions.resolve_approvals();
    rig.until_done(0.0);
    assert_eq!(
        last_results(&rig, 1),
        ["<data source=\"face_set_profile\">the person rejected this: 'too wide'</data>"]
    );
}

#[test]
fn a_paused_session_is_polled_and_the_call_is_sent_again_exactly_once() {
    let paused = ErrorDetail::Paused { by: Actor::System, reason: PauseReason::Person };
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            let session = Arc::clone(sessions);
            server.add(tool("face_set_profile", "Face profile", "edit", false), move |_, n| {
                if n == 0 {
                    session.set_state(SessionState::Paused { by: person(), reason: PauseReason::Person });
                    Outcome::refusal("waiting", &paused)
                } else {
                    Outcome::text("applied")
                }
            });
            server
        },
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("Paused", || {
        while let Some(e) = rig.rx.try_recv() {
            seen.push(e);
        }
        seen.iter().any(|e| matches!(e, AssistantEvent::Paused))
    });
    assert_eq!(rig.server.called("face_set_profile"), 1);
    rig.sessions.set_state(SessionState::Active);
    let rest = rig.until_done(0.25);
    assert!(rest.iter().any(|e| matches!(e, AssistantEvent::Resumed)));
    assert_eq!(rig.server.called("face_set_profile"), 2);
    assert_eq!(last_results(&rig, 1), ["<data source=\"face_set_profile\">applied</data>"]);
}

fn limited(seconds: f32) -> Outcome {
    Outcome::refusal("limited", &ErrorDetail::RetryAfter { seconds, limit: "edits_per_minute=600".into() })
}

#[test]
fn a_rate_limit_waits_the_stated_time_once_and_sends_again() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        edit_server(vec![limited(3.0), Outcome::text("applied")]),
    );
    rig.say("go", 0.5);
    assert_eq!(rig.server.called("face_set_profile"), 2);
    assert!(rig.clock.now().0 >= 3.0);
    assert_eq!(last_results(&rig, 1), ["<data source=\"face_set_profile\">applied</data>"]);
}

#[test]
fn a_second_limit_goes_to_the_model_and_a_long_wait_is_cut_to_ten_seconds() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        edit_server(vec![limited(60.0), limited(60.0)]),
    );
    rig.say("go", 1.0);
    assert_eq!(rig.server.called("face_set_profile"), 2);
    let waited = rig.clock.now().0;
    assert!((10.0..20.0).contains(&waited), "{waited}");
    assert!(last_results(&rig, 1)[0].contains("failed [limited]"));
}

#[test]
fn denied_and_conflict_arrive_unchanged() {
    for (code, detail) in
        [("denied", ErrorDetail::PersonOnly { reason: "only you".into() }), ("conflict", ErrorDetail::NoSession)]
    {
        let outcome = Outcome::refusal(code, &detail);
        let rig = Rig::with(
            script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
            RigOptions::default(),
            edit_server(vec![outcome]),
        );
        rig.say("go", 0.0);
        assert_eq!(rig.server.called("face_set_profile"), 1, "{code} is not retried");
        assert_eq!(last_results(&rig, 1), [format!("<data source=\"face_set_profile\">x failed [{code}]</data>")]);
    }
}
