//! Undo per principal (#245, design `agent-sessions.md` §5.4): a person's undo takes the newest step among their own
//! and their agents' histories, pausing a running agent first; a new commit by any of them clears the person's redo.
//! Automation sessions are standing rules, not assistants, so they stay out (`automation.md` D7).

use omuzaki_ops_api::{
    Event, Grants, PauseReason, Request, Runner, SessionId, SessionSpec, SessionState, Sessions, Transport,
};
use serde_json::json;

use super::{SET, agent, drain, person, rig, session_spec};

#[test]
fn a_persons_undo_reaches_their_agents_newer_step_first_and_pauses_it() {
    let rig = rig(json!({"x": 0, "y": 0, "z": 0}));
    let other = rig.bobs_agent();
    rig.set("x", json!(1)); // X, alice
    rig.agent_set("y", json!(1)); // Y, alice's agent
    rig.set_as(&other, "z", json!(1)); // bob's agent, newest of all
    let rx = rig.subscribe();

    assert_eq!(rig.undo().unwrap().as_deref(), Some(SET));
    assert_eq!((rig.at("x"), rig.at("y"), rig.at("z")), (Some(json!(1)), Some(json!(0)), Some(json!(1))));
    assert!(matches!(
        rig.exec.session(SessionId(1)).unwrap().state,
        SessionState::Paused { reason: PauseReason::Undo, .. }
    ));
    let events = drain(&rx);
    let undone = events.iter().find_map(|e| match e {
        Event::Undone { actor, owner, .. } => Some((actor.clone(), owner.clone())),
        _ => None,
    });
    assert_eq!(undone, Some((person(), agent())), "done by alice, owned by the agent");

    assert_eq!(rig.undo().unwrap().as_deref(), Some(SET));
    assert_eq!(rig.at("x"), Some(json!(0)), "then her own");
    assert_eq!(rig.undo().unwrap(), None, "never bob's agent's step");
    assert_eq!(rig.at("z"), Some(json!(1)));

    assert!(rig.redo().unwrap().is_some());
    assert_eq!(rig.at("x"), Some(json!(1)), "redo restores X first");
    assert!(rig.redo().unwrap().is_some());
    assert_eq!(rig.at("y"), Some(json!(1)), "then Y");
}

#[test]
fn the_agents_own_undo_still_reaches_only_its_steps() {
    let rig = rig(json!({"x": 0, "y": 0}));
    rig.agent_set("y", json!(1));
    rig.set("x", json!(1));
    assert_eq!(rig.agent_undo().unwrap().as_deref(), Some(SET));
    assert_eq!((rig.at("x"), rig.at("y")), (Some(json!(1)), Some(json!(0))));
    assert_eq!(rig.agent_undo().unwrap(), None);
}

#[test]
fn an_agents_commit_clears_its_persons_redo_once_resumed() {
    let rig = rig(json!({"x": 0, "y": 0}));
    rig.agent_set("y", json!(1));
    rig.undo().unwrap();
    assert!(matches!(rig.exec.session(SessionId(1)).unwrap().state, SessionState::Paused { .. }));
    rig.exec.resume(SessionId(1), &person()).unwrap();
    rig.agent_set("x", json!(1));
    assert_eq!(rig.redo().unwrap(), None, "the agent's new step ended alice's redo");
    assert_eq!(rig.at("y"), Some(json!(0)));
}

#[test]
fn an_automation_sessions_steps_are_not_the_persons_to_undo() {
    let rig = rig(json!({"scene": 0}));
    let show = rig.exec.open(SessionSpec { transport: Transport::Automation, ..session_spec("show") }).unwrap();
    rig.set_as(&show.actor, "scene", json!(1));
    assert_eq!(rig.undo().unwrap(), None);
    assert_eq!(rig.at("scene"), Some(json!(1)));
    assert_eq!(rig.exec.undo(&show.actor, &Grants::agent_default()).unwrap().as_deref(), Some(SET));
}

#[test]
fn a_refused_undo_of_an_agents_step_keeps_it() {
    let rig = rig(json!({"p": 0}));
    let req = Request {
        actor: agent(),
        grants: Grants::agent_default().with(omuzaki_ops_api::Scope::Publish),
        ..super::agent_request("test.publish", json!({"path": "p", "value": 1}))
    };
    rig.exec.run(req).unwrap();
    let e = rig.exec.undo(&person(), &Grants::agent_default()).unwrap_err();
    assert_eq!(e.code, omuzaki_ops_api::ErrorCode::Denied);
    assert_eq!(rig.at("p"), Some(json!(1)));
    assert!(rig.undo().unwrap().is_some(), "with the scope it goes");
}
