//! Undo a whole agent session as one step in the person's history (#357, design `agent-sessions.md` §6).

use omuzaki_ops_api::{
    EndReason, ErrorCode, Event, PauseReason, Request, Runner, SessionId, SessionState, Sessions, TxId,
};
use serde_json::json;

use super::{agent, agent_request, drain, person, request, rig, second_agent};

fn agent_set(at: &str, value: &str) -> Request {
    agent_request("test.set", json!({"path": at, "value": value}))
}

fn step_tx(rig: &super::Rig, req: Request) -> TxId {
    rig.exec.run(req).unwrap().applied.unwrap().tx
}

/// The design's required test: the person's edit interleaved with the agent's three steps.
#[test]
fn undo_session_skips_what_a_person_changed_since_and_the_persons_undo_brings_it_back() {
    let rig = rig(json!({"a": "a0", "b": "b0"}));
    let s1 = step_tx(&rig, agent_set("a", "a1"));
    let s2 = step_tx(&rig, agent_set("b", "b1"));
    let s3 = step_tx(&rig, agent_set("a", "a3"));
    rig.run("test.set", json!({"path": "b", "value": "bp"})).unwrap();
    let rx = rig.subscribe();

    let undone = rig.exec.undo_session(SessionId(1), &person()).unwrap();
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!("a0")), Some(json!("bp"))));
    assert_eq!(undone.reverted, vec![s3, s1]);
    assert_eq!(undone.skipped.iter().map(|k| k.tx).collect::<Vec<_>>(), vec![s2]);
    let conflict = &undone.skipped[0].error;
    assert_eq!(conflict.code, ErrorCode::Conflict);
    assert!(conflict.message.contains("person alice"), "names who changed it: {conflict}");
    assert!(undone.abandoned.is_empty() && undone.outside.is_empty());

    // SessionUndone, then the new step's commit by the person.
    let events = drain(&rx);
    let (undone_at, committed_at) = (
        events.iter().position(|e| matches!(e, Event::SessionUndone { .. })).unwrap(),
        events.iter().position(|e| matches!(e, Event::TxCommitted { actor, .. } if *actor == person())).unwrap(),
    );
    assert!(undone_at < committed_at, "{events:?}");
    let Event::TxCommitted { tx, label, .. } = &events[committed_at] else { unreachable!() };
    assert_eq!((Some(*tx), label.as_str()), (undone.step, "Undo claude's session (2 steps)"));

    // The session stays paused for the person; once resumed, the agent's own history is empty.
    assert!(matches!(
        rig.exec.session(SessionId(1)).unwrap().state,
        SessionState::Paused { reason: PauseReason::Undo, .. }
    ));
    rig.exec.resume(SessionId(1), &person()).unwrap();
    assert_eq!((rig.agent_undo().unwrap(), rig.agent_redo().unwrap()), (None, None));
    // The person's undo re-applies exactly what was reverted.
    assert_eq!(rig.undo().unwrap().as_deref(), Some("Undo claude's session (2 steps)"));
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!("a3")), Some(json!("bp"))));
    assert_eq!(rig.redo().unwrap().as_deref(), Some("Undo claude's session (2 steps)"));
    assert_eq!(rig.at("a"), Some(json!("a0")));
}

#[test]
fn a_running_session_is_paused_an_ended_one_can_still_be_undone_and_only_its_person_may() {
    let rig = rig(json!({"a": 0, "c": 0}));
    rig.exec.run(agent_request("test.set", json!({"path": "a", "value": 1}))).unwrap();
    assert_eq!(rig.exec.undo_session(SessionId(1), &agent()).unwrap_err().code, ErrorCode::Denied, "an agent");
    let bob = omuzaki_ops_api::Actor::Person { name: "bob".into() };
    assert_eq!(rig.exec.undo_session(SessionId(1), &bob).unwrap_err().code, ErrorCode::Denied, "another person");

    rig.exec.undo_session(SessionId(1), &person()).unwrap();
    assert!(matches!(
        rig.exec.session(SessionId(1)).unwrap().state,
        SessionState::Paused { reason: PauseReason::Undo, .. }
    ));
    assert_eq!(rig.at("a"), Some(json!(0)));
    rig.exec.resume(SessionId(1), &person()).unwrap();

    let other = Request { actor: second_agent(), ..agent_request("test.set", json!({"path": "c", "value": 1})) };
    rig.exec.run(other).unwrap();
    rig.exec.end(SessionId(2), EndReason::Closed).unwrap();
    let undone = rig.exec.undo_session(SessionId(2), &person()).unwrap();
    assert_eq!(undone.reverted.len(), 1);
    assert_eq!(rig.at("c"), Some(json!(0)));
    assert!(matches!(rig.exec.session(SessionId(2)).unwrap().state, SessionState::Ended { .. }), "stays ended");
}

#[test]
fn open_transactions_are_abandoned_and_outside_effects_are_listed() {
    let rig = rig(json!({"a": 0, "x": 0}));
    let explicit = rig.exec.run(agent_request("test.explicit_set", json!({"path": "x", "value": 1}))).unwrap();
    let tx = rig.exec.begin("open", &agent());
    rig.exec.run(Request { tx: Some(tx), ..agent_request("test.set", json!({"path": "a", "value": 1})) }).unwrap();
    let undone = rig.exec.undo_session(SessionId(1), &person()).unwrap();
    assert_eq!(undone.abandoned, vec![tx]);
    assert_eq!(undone.outside, vec![explicit.applied.unwrap().tx]);
    assert_eq!((undone.step, rig.at("a"), rig.at("x")), (None, Some(json!(0)), Some(json!(1))));
    // A person's own edits are not touched, nor another session's.
    rig.exec.run(request("test.set", json!({"path": "a", "value": 5}))).unwrap();
    rig.exec.undo_session(SessionId(2), &person()).unwrap();
    assert_eq!(rig.at("a"), Some(json!(5)));
}
