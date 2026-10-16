//! Agent sessions: the registry, the principal, effective grants, pause and resume (design `agent-sessions.md` §3, §4.3, §6).
//! The rig has already opened the sessions behind `agent()` (`claude#1`) and `second_agent()` (`claude#2`), for alice.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, ErrorCode, ErrorDetail, Event, Grants, PauseReason, Principals, Request, Runner, Scope,
    SessionChange, SessionId, SessionState, Sessions, Spend,
};
use serde_json::json;

use super::{agent, agent_request, drain, person, request, rig, second_agent, session_spec};

fn bob() -> Actor {
    Actor::Person { name: "bob".into() }
}

/// A principal source whose person holds whatever the test last set.
struct Dial(Mutex<Grants>);

impl Principals for Dial {
    fn grants(&self, person: &Actor) -> Grants {
        if matches!(person, Actor::Person { .. }) { self.0.lock().unwrap().clone() } else { Grants::default() }
    }
}

fn publish(tx: Option<omuzaki_ops_api::TxId>) -> Request {
    Request {
        tx,
        grants: Grants::agent_default().with(Scope::Publish),
        ..agent_request("test.publish", json!({"path": "p", "value": 1}))
    }
}

#[test]
fn a_session_is_named_by_the_executor_and_stamps_its_principal_on_every_record() {
    let rig = rig(json!({}));
    let info = rig.exec.session(SessionId(1)).unwrap();
    assert_eq!((info.actor, info.on_behalf_of, info.state), (agent(), person(), SessionState::Active));
    assert_eq!(rig.exec.session_of(&second_agent()).unwrap().id, SessionId(2));

    let by_agent = rig.exec.run(agent_request("test.set", json!({"path": "a", "value": 1}))).unwrap().applied.unwrap();
    let by_person = rig.set("b", json!(2)).applied.unwrap();
    assert_eq!(by_agent.on_behalf_of, Some(person()));
    assert_eq!(by_person.on_behalf_of, None, "persons act for themselves");
    assert!(rig.log().iter().filter(|r| r.actor == agent()).all(|r| r.on_behalf_of == Some(person())));
}

#[test]
fn a_client_cannot_spell_another_sessions_actor() {
    let rig = rig(json!({}));
    let info = rig.exec.open(session_spec("claude#1 ")).unwrap();
    assert_eq!(info.client, "claude-1-");
    assert_eq!(info.actor, Actor::Agent { id: "claude-1-#3".into(), model: None });
}

#[test]
fn ids_start_where_the_executor_is_told() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::new()), clock).unwrap().with_first_session_id(40);
    assert_eq!(exec.open(session_spec("c")).unwrap().id, SessionId(40));
    assert_eq!(exec.open(session_spec("c")).unwrap().id, SessionId(41));
}

#[test]
fn an_agent_or_plugin_without_a_session_is_refused() {
    let rig = rig(json!({}));
    for actor in [Actor::Agent { id: "ghost#9".into(), model: None }, Actor::Plugin { id: "stamper".into() }] {
        let req = Request {
            actor: actor.clone(),
            grants: Grants::agent_default(),
            ..agent_request("test.get", json!({"path": "a"}))
        };
        let e = rig.exec.run(req).unwrap_err();
        assert_eq!((e.code, e.detail.as_deref()), (ErrorCode::Denied, Some(&ErrorDetail::NoSession)), "{actor:?}");
        assert_eq!(rig.exec.undo(&actor, &Grants::agent_default()).unwrap_err().code, ErrorCode::Denied);
        assert_eq!(rig.exec.commit(rig.exec.begin("x", &actor), &actor).unwrap_err().code, ErrorCode::Denied);
    }
    // A claimed model does not make another actor's session its own.
    let model = Actor::Agent { id: "claude#1".into(), model: Some("other".into()) };
    let req = Request { actor: model, ..agent_request("test.get", json!({"path": "a"})) };
    assert_eq!(rig.exec.run(req).unwrap_err().code, ErrorCode::Denied);
}

#[test]
fn grants_are_the_intersection_with_the_person_and_the_request_only_narrows() {
    let dial = Arc::new(Dial(Mutex::new(Grants::person())));
    let exec = rebuild(&dial);

    // A ceiling beyond the person is refused at the door.
    let mut spec = session_spec("claude");
    dial.0.lock().unwrap().clone_from(&Grants::agent_default());
    spec.grants = Grants::agent_default().with(Scope::Publish);
    assert_eq!(exec.open(spec.clone()).unwrap_err().code, ErrorCode::Denied);

    // Within it, the session holds `publish`: the person did too when it opened.
    *dial.0.lock().unwrap() = Grants::person();
    let session = exec.open(spec).unwrap();
    assert!(session.grants.allows(Scope::Publish));
    exec.run(Request { actor: session.actor.clone(), ..publish(None) }).unwrap();

    // The person loses `publish`: the session's ceiling still names it, and is ineffective at once.
    *dial.0.lock().unwrap() = Grants::agent_default();
    assert!(!exec.session(session.id).unwrap().grants.allows(Scope::Publish));
    let e = exec.run(Request { actor: session.actor.clone(), ..publish(None) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);

    // A request cannot widen the ceiling either.
    *dial.0.lock().unwrap() = Grants::person();
    let narrow = exec.open(session_spec("narrow")).unwrap();
    let wide = Request {
        actor: narrow.actor.clone(),
        grants: Grants::person(),
        ..agent_request("test.get", json!({"path": "p"}))
    };
    exec.run(wide).unwrap();
}

/// An executor whose people hold what `dial` says.
fn rebuild(dial: &Arc<Dial>) -> Executor {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_principals(dial.clone())
}

#[test]
fn persons_hold_what_their_principal_holds() {
    let dial = Arc::new(Dial(Mutex::new(Grants::of([Scope::Read]))));
    let exec = rebuild(&dial);
    let req = request("test.set", json!({"path": "a", "value": 1}));
    assert_eq!(exec.run(req).unwrap_err().code, ErrorCode::Denied, "alice holds only `read`");
}

#[test]
fn a_sub_agent_holds_at_most_what_its_parent_holds_and_works_for_the_same_person() {
    let rig = rig(json!({}));
    let mut parent = session_spec("lead");
    parent.grants = Grants::agent_default();
    let parent = rig.exec.open(parent).unwrap();

    let mut child = session_spec("helper");
    child.parent = Some(parent.id);
    child.grants = Grants::agent_default().with(Scope::Publish);
    assert_eq!(rig.exec.open(child.clone()).unwrap_err().code, ErrorCode::Denied, "wider than the parent");
    child.grants = Grants::agent_default();
    child.on_behalf_of = bob();
    assert_eq!(rig.exec.open(child.clone()).unwrap_err().code, ErrorCode::Denied, "another person's");
    child.on_behalf_of = person();
    child.parent = Some(SessionId(99));
    assert_eq!(rig.exec.open(child).unwrap_err().code, ErrorCode::Invalid, "no such parent");
}

#[test]
fn a_session_works_for_a_person_and_claims_no_fence() {
    let rig = rig(json!({}));
    let mut spec = session_spec("claude");
    spec.on_behalf_of = agent();
    assert_eq!(rig.exec.open(spec).unwrap_err().code, ErrorCode::Invalid);
    let mut spec = session_spec("claude");
    spec.fence = vec![super::path("a")];
    assert_eq!(rig.exec.open(spec).unwrap_err().code, ErrorCode::Unsupported, "fences are not enforced yet");
}

#[test]
fn a_paused_session_is_refused_fail_closed_and_resume_restores_it() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("agent work", &agent());
    rig.exec.run(Request { tx: Some(tx), ..agent_request("test.set", json!({"path": "a", "value": 2})) }).unwrap();
    assert_eq!(rig.exec.session(SessionId(1)).unwrap().open_tx.unwrap().tx, tx);

    let paused = rig.exec.pause(SessionId(1), &person()).unwrap();
    let state = SessionState::Paused { by: person(), reason: PauseReason::Person };
    assert_eq!(paused.state, state);

    let waiting = |e: omuzaki_ops_api::OpError| {
        assert_eq!(e.code, ErrorCode::Waiting, "{e}");
        assert_eq!(e.detail.as_deref(), Some(&ErrorDetail::Paused { by: person(), reason: PauseReason::Person }));
    };
    waiting(
        rig.exec
            .run(Request { tx: Some(tx), ..agent_request("test.set", json!({"path": "a", "value": 3})) })
            .unwrap_err(),
    );
    waiting(rig.exec.run(agent_request("test.get", json!({"path": "a"}))).unwrap_err());
    waiting(
        rig.exec
            .run(omuzaki_ops_api::Request {
                mode: omuzaki_ops_api::Mode::DryRun,
                ..agent_request("test.set", json!({"path": "a", "value": 3}))
            })
            .unwrap_err(),
    );
    waiting(rig.exec.commit(tx, &agent()).unwrap_err());
    waiting(rig.exec.rollback(tx, &agent()).unwrap_err());
    waiting(rig.agent_undo().unwrap_err());
    waiting(rig.agent_redo().unwrap_err());
    // `begin` cannot say no: what it hands back is not an open transaction, and every use of it is refused.
    let phantom = rig.exec.begin("while paused", &agent());
    assert_ne!(phantom, tx);
    assert!(
        rig.exec.session(SessionId(1)).unwrap().open_tx.is_some_and(|open| open.tx == tx),
        "the real one stays open"
    );

    // The transaction stays open and a person's edit meanwhile applies.
    assert_eq!(rig.at("a"), Some(json!(2)));
    rig.set("b", json!(5));
    assert_eq!(rig.at("b"), Some(json!(5)));
    assert_eq!(rig.log().iter().filter(|r| r.actor == agent()).count(), 1, "nothing of the agent's ran while paused");

    let resumed = rig.exec.resume(SessionId(1), &person()).unwrap();
    assert_eq!(resumed.state, SessionState::Active);
    rig.exec.commit(tx, &agent()).unwrap();
    assert_eq!(rig.exec.session(SessionId(1)).unwrap().steps, 1);
    assert_eq!(rig.agent_undo().unwrap().as_deref(), Some("agent work"));
    assert_eq!(rig.at("a"), Some(json!(1)));
    assert!(rig.agent_undo().unwrap().is_none(), "one undo step");
}

#[test]
fn only_the_sessions_person_controls_it() {
    let rig = rig(json!({}));
    for who in [agent(), second_agent(), bob(), Actor::System] {
        assert_eq!(rig.exec.pause(SessionId(1), &who).unwrap_err().code, ErrorCode::Denied, "{who:?}");
        assert_eq!(rig.exec.resume(SessionId(1), &who).unwrap_err().code, ErrorCode::Denied);
        assert_eq!(rig.exec.set_grants(SessionId(1), Grants::default(), &who).unwrap_err().code, ErrorCode::Denied);
        assert_eq!(rig.exec.set_budget(SessionId(1), Budget::default(), &who).unwrap_err().code, ErrorCode::Denied);
    }
    assert_eq!(rig.exec.pause(SessionId(77), &person()).unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(rig.exec.session(SessionId(1)).unwrap().state, SessionState::Active);
}

#[test]
fn set_grants_replaces_the_ceiling_up_to_the_persons_own() {
    let dial = Arc::new(Dial(Mutex::new(Grants::agent_default())));
    let exec = rebuild(&dial);
    let mut spec = session_spec("claude");
    spec.grants = Grants::of([Scope::Read]);
    let id = exec.open(spec).unwrap().id;

    let wider = exec.set_grants(id, Grants::agent_default().with(Scope::Publish), &person()).unwrap_err();
    assert_eq!(wider.code, ErrorCode::Denied, "beyond the person's own");
    let info = exec.set_grants(id, Grants::agent_default(), &person()).unwrap();
    assert!(info.grants.allows(Scope::EditCharacter));
}

#[test]
fn controls_and_pauses_emit_one_event_per_change() {
    let rig = rig(json!({}));
    let rx = rig.subscribe();
    rig.exec.pause(SessionId(1), &person()).unwrap();
    rig.exec.pause(SessionId(1), &person()).unwrap(); // already paused: nothing changed
    rig.exec.resume(SessionId(1), &person()).unwrap();
    rig.exec.resume(SessionId(1), &person()).unwrap();
    rig.exec.set_budget(SessionId(1), Budget::default(), &person()).unwrap();
    let opened = rig.exec.open(session_spec("late")).unwrap();

    let events = drain(&rx);
    let changes: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::SessionChanged { change, session, .. } => Some((session.id, change.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        changes,
        [
            (SessionId(1), SessionChange::Paused),
            (SessionId(1), SessionChange::Resumed { by: person() }),
            (SessionId(1), SessionChange::Budget { by: person() }),
        ]
    );
    assert!(matches!(events.last(), Some(Event::SessionOpened { session, .. }) if session.id == opened.id));
}

#[test]
fn pausing_a_parent_pauses_its_children_and_resuming_resumes_them() {
    let rig = rig(json!({}));
    let mut child = session_spec("child");
    child.parent = Some(SessionId(1));
    child.grants = Grants::agent_default();
    let child = rig.exec.open(child).unwrap();
    let other_child = {
        let mut spec = session_spec("other");
        spec.parent = Some(SessionId(2));
        rig.exec.open(spec).unwrap()
    };

    let rx = rig.subscribe();
    rig.exec.pause(SessionId(1), &person()).unwrap();
    assert!(matches!(
        rig.exec.session(child.id).unwrap().state,
        SessionState::Paused { reason: PauseReason::Person, .. }
    ));
    assert_eq!(rig.exec.session(other_child.id).unwrap().state, SessionState::Active, "not in the family");
    assert_eq!(drain(&rx).len(), 2, "one event per session that moved");
    assert_eq!(
        rig.exec
            .run(Request { actor: child.actor.clone(), ..agent_request("test.get", json!({"path": "a"})) })
            .unwrap_err()
            .code,
        ErrorCode::Waiting
    );

    let mut late = session_spec("late");
    late.parent = Some(SessionId(1));
    assert_eq!(rig.exec.open(late).unwrap_err().code, ErrorCode::Conflict, "no new child under a paused parent");
    assert_eq!(rig.exec.resume(child.id, &person()).unwrap_err().code, ErrorCode::Conflict, "the parent first");

    rig.exec.resume(SessionId(1), &person()).unwrap();
    assert_eq!(rig.exec.session(child.id).unwrap().state, SessionState::Active);
}

#[test]
fn what_the_executor_does_not_do_yet_is_unsupported() {
    let rig = rig(json!({}));
    let ended = rig.exec.end(SessionId(1), omuzaki_ops_api::EndReason::Closed).unwrap_err();
    assert_eq!(ended.code, ErrorCode::Unsupported);
    assert_eq!(rig.exec.undo_session(SessionId(1), &person()).unwrap_err().code, ErrorCode::Unsupported);
    assert_eq!(rig.exec.charge(SessionId(1), Spend::default()).unwrap_err().code, ErrorCode::Unsupported);
    assert_eq!(rig.exec.approvals().len(), 0);
}

// ---- the check and the change are one hold of the lock ------------------------------------------------------------

/// A principal source that narrows its person to `read` after `full` more questions.
struct Narrowing(AtomicUsize);

impl Principals for Narrowing {
    fn grants(&self, _: &Actor) -> Grants {
        let left = self.0.load(Ordering::SeqCst);
        if left == 0 {
            return Grants::of([Scope::Read]);
        }
        self.0.store(left - 1, Ordering::SeqCst);
        Grants::person()
    }
}

#[test]
fn grants_are_recomputed_under_the_lock_that_applies_the_edit() {
    let narrowing = Arc::new(Narrowing(AtomicUsize::new(usize::MAX)));
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec =
        Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_principals(narrowing.clone());
    let agent = exec.open(session_spec("claude")).unwrap().actor;

    // The first question is the early look in `run`; the person loses the edit right before the second, which is the
    // one made under the lock that plans and applies.
    narrowing.0.store(1, Ordering::SeqCst);
    let req = Request { actor: agent, ..agent_request("test.set", json!({"path": "a", "value": 1})) };
    let e = exec.run(req).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied, "{e}");
    assert_eq!(exec.get(&super::path("a"), &Grants::person()).unwrap(), None);
    assert_eq!(exec.log(0, 10, &Grants::person()).unwrap().len(), 0);
}

/// A principal source whose next question waits for the test: the thread asking holds the executor's lock meanwhile.
#[derive(Default)]
struct Pinned(Mutex<Option<(Sender<()>, Receiver<()>)>>);

impl Pinned {
    /// Arms the pin: the next question announces itself on the first channel and waits for the second.
    fn arm(&self) -> (Receiver<()>, Sender<()>) {
        let (entered, entered_rx) = channel();
        let (release, release_rx) = channel();
        *self.0.lock().unwrap() = Some((entered, release_rx));
        (entered_rx, release)
    }
}

impl Principals for Pinned {
    fn grants(&self, _: &Actor) -> Grants {
        let pinned = self.0.lock().unwrap().take();
        if let Some((entered, release)) = pinned {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        Grants::person()
    }
}

/// Runs `control` for an agent while another thread pauses its session. The pause must land either before the control
/// is admitted (refused) or after it ran: never between the check and the change. The control is held in its check
/// until the pause is waiting for the lock, so a split check lets the pause through first.
fn pause_during(
    control: fn(&Executor, &Actor, omuzaki_ops_api::TxId) -> Result<(), omuzaki_ops_api::OpError>,
    kind: &str,
) {
    let pinned = Arc::new(Pinned::default());
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Arc::new(
        Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_principals(pinned.clone()),
    );
    let info = exec.open(session_spec("claude")).unwrap();
    let tx = exec.begin("work", &info.actor);
    let req = Request {
        tx: Some(tx),
        actor: info.actor.clone(),
        ..agent_request("test.set", json!({"path": "a", "value": 1}))
    };
    exec.run(req).unwrap();
    if kind == "undo" {
        exec.commit(tx, &info.actor).unwrap();
    }
    let events = exec.subscribe(&Grants::person()).unwrap();

    let (entered, release) = pinned.arm();
    let (control_exec, actor) = (exec.clone(), info.actor.clone());
    let controlling = std::thread::spawn(move || control(&control_exec, &actor, tx));
    entered.recv().unwrap();
    let (pause_exec, id) = (exec.clone(), info.id);
    let pausing = std::thread::spawn(move || pause_exec.pause(id, &person()));
    std::thread::sleep(Duration::from_millis(50));
    release.send(()).unwrap();
    controlling.join().unwrap().unwrap_or_else(|e| panic!("{kind} was refused: {e}"));
    pausing.join().unwrap().unwrap();

    let order: Vec<&str> = drain(&events)
        .iter()
        .filter_map(|e| match e {
            Event::TxCommitted { .. } => Some("committed"),
            Event::TxRolledBack { .. } => Some("rolled back"),
            Event::Undone { .. } => Some("undone"),
            Event::SessionChanged { .. } => Some("paused"),
            _ => None,
        })
        .collect();
    let did = match kind {
        "commit" => "committed",
        "rollback" => "rolled back",
        _ => "undone",
    };
    assert_eq!(order, [did, "paused"], "{kind}: the pause landed in the middle of the control");
}

#[test]
fn a_pause_never_lands_between_the_check_and_the_change_of_a_transaction_control() {
    // The window is a race on the lock, so each shape runs several times.
    for _ in 0..20 {
        pause_during(|exec, actor, tx| exec.commit(tx, actor), "commit");
        pause_during(|exec, actor, tx| exec.rollback(tx, actor), "rollback");
        pause_during(|exec, actor, _| exec.undo(actor, &Grants::agent_default()).map(drop), "undo");
    }
}

#[test]
fn narrowing_a_parent_emits_a_change_for_each_sub_agent_whose_grants_moved() {
    let rig = rig(json!({}));
    let mut parent = session_spec("lead");
    parent.grants = Grants::agent_default();
    let parent = rig.exec.open(parent).unwrap();
    let mut child = session_spec("helper");
    child.parent = Some(parent.id);
    child.grants = Grants::agent_default();
    let child = rig.exec.open(child).unwrap();
    let mut bystander = session_spec("other");
    bystander.grants = Grants::agent_default();
    rig.exec.open(bystander).unwrap();

    let rx = rig.subscribe();
    rig.exec.set_grants(parent.id, Grants::of([Scope::Read]), &person()).unwrap();
    let changed: Vec<_> = drain(&rx)
        .iter()
        .filter_map(|e| match e {
            Event::SessionChanged { change, session, .. } => Some((session.id, change.clone(), session.grants.clone())),
            _ => None,
        })
        .collect();
    let by = SessionChange::Grants { by: person() };
    assert_eq!(changed.len(), 2, "the parent and its sub-agent, not the unrelated session: {changed:?}");
    assert_eq!((changed[0].0, &changed[0].1), (parent.id, &by));
    assert_eq!((changed[1].0, &changed[1].1), (child.id, &by));
    assert_eq!(changed[1].2, Grants::of([Scope::Read]), "the snapshot carries the narrowed effective grants");
}
