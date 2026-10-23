//! A session paused elsewhere, undone, or revoked while an answer runs (#1336, D4).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use std::sync::Arc;

use common::*;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::{Entry, SessionNote};
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_ops_api::{EndReason, PauseReason, SessionState};

#[test]
fn a_pause_mid_answer_keeps_the_transaction_open_and_resume_continues_the_answer() {
    for reason in [PauseReason::Person, PauseReason::Undo] {
        let rig = Rig::with(
            script(vec![
                call_turn("", &[("c1", "face_set_profile", "{}")]),
                call_turn("", &[("c2", "face_set_profile", "{}")]),
                say_turn("done"),
            ]),
            RigOptions::default(),
            |sessions| {
                let server = standard_server(sessions);
                let session = Arc::clone(sessions);
                server.add(tool("face_set_profile", "Face profile", "edit", false), move |_, n| {
                    if n == 0 {
                        // Someone pauses the session while the first edit is being made.
                        session.set_state(SessionState::Paused { by: person(), reason });
                    }
                    Outcome::text("applied")
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
        assert_eq!(rig.server.called("face_set_profile"), 1, "nothing more runs while paused");
        assert_eq!(rig.server.called("tx_commit") + rig.server.called("tx_rollback"), 0, "the transaction stays open");
        assert_eq!(rig.llm.requests().len(), 1, "no new request is sent while paused");
        rig.sessions.set_state(SessionState::Active);
        let rest = rig.until_done(0.25);
        assert!(rest.iter().any(|e| matches!(e, AssistantEvent::Resumed)), "{reason:?}");
        assert_eq!(rig.server.called("face_set_profile"), 2);
        assert_eq!(rig.server.called("tx_begin"), 1);
        assert_eq!(rig.server.called("tx_commit"), 1);
        assert_eq!(rig.sessions.pause_calls(), 0);
    }
}

#[test]
fn a_revoke_mid_answer_says_session_ended_and_refuses_every_later_send() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "render_preview", "{}")]), say_turn("never")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            server.add_fixed(tool("render_preview", "Render preview", "query", true), Outcome::text("r").held(&gate));
            server
        },
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the call", || rig.server.called("render_preview") == 1);
    rig.sessions.revoke();
    gate.notify_one();
    let events = rig.until_done(0.0);
    assert!(events.iter().any(|e| matches!(e, AssistantEvent::Error(HostError::SessionEnded))));
    assert!(matches!(events.last(), Some(AssistantEvent::Done)));
    assert_eq!(rig.llm.requests().len(), 1, "the model is not asked again");
    assert_eq!(rig.host.send(UserMessage::new("again")), Err(HostError::SessionEnded));
    let ended = rig
        .sink
        .entries()
        .into_iter()
        .any(|e| matches!(e, Entry::Session { note: SessionNote::Ended(EndReason::Revoked { .. }), .. }));
    assert!(ended, "the revocation is recorded");
    assert_eq!(rig.server.called("tx_commit"), 0, "the executor rolled the transaction back with the session");
}

#[test]
fn prompts_of_a_revoked_session_are_session_ended() {
    let rig = Rig::new(script(vec![]));
    rig.sessions.revoke();
    assert_eq!(rig.host.prompt("any", std::collections::BTreeMap::new()), Err(HostError::SessionEnded));
    assert_eq!(rig.host.prompts(), omuzaki_agent_api::prompts::PromptsState::Failed(HostError::SessionEnded));
}
