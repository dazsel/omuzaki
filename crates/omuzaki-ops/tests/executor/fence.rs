//! Fence claims and enforcement (#967, design `agent-sessions.md` §5.3, "Fences"): overlapping claims are refused
//! naming the holder, an agent's edits stay inside its own fence and out of others', a refused apply is one `Clash`,
//! and a claim lasts exactly as long as its session.

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{
    Actor, ClashKind, EndReason, ErrorCode, ErrorDetail, Event, Mode, Request, Runner, SessionChange, SessionId,
    SessionInfo, SessionSpec, Sessions, Transport,
};
use serde_json::{Value, json};

use super::{Rig, agent_request, drain, path, person, request, rig, session_spec};

fn character() -> Value {
    json!({"characters": {"a": {"parts": {"hair": {"bangs": 0}, "face": {"eyes": 0}, "tail": 0}}}})
}

fn fence(paths: &[&str]) -> Vec<DocPath> {
    paths.iter().map(|p| path(p)).collect()
}

fn open_fenced(rig: &Rig, paths: &[&str], parent: Option<SessionId>) -> Result<SessionInfo, omuzaki_ops_api::OpError> {
    rig.exec.open(SessionSpec { fence: fence(paths), parent, ..session_spec("claude") })
}

fn set(actor: &Actor, at: &str) -> Request {
    Request { actor: actor.clone(), ..agent_request("test.set", json!({"path": at, "value": 1})) }
}

fn fenced_by(error: &omuzaki_ops_api::OpError) -> Option<Actor> {
    match error.detail.as_deref() {
        Some(ErrorDetail::Fenced { holder, .. }) => holder.clone(),
        other => panic!("not fenced: {other:?} ({error:?})"),
    }
}

#[test]
fn an_overlapping_claim_is_refused_naming_its_holder() {
    let rig = rig(character());
    let a = open_fenced(&rig, &["characters/a/parts/hair"], None).unwrap();
    let refused = open_fenced(&rig, &["characters/a/parts/hair/bangs"], None).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert_eq!(fenced_by(&refused), Some(a.actor.clone()));
    let b = open_fenced(&rig, &["characters/a/parts/face"], None).unwrap();
    assert_eq!(b.fence, fence(&["characters/a/parts/face"]));
}

#[test]
fn edits_stay_inside_their_fence_and_out_of_others_while_queries_and_persons_pass() {
    let rig = rig(character());
    let a = open_fenced(&rig, &["characters/a/parts/hair"], None).unwrap();
    let b = open_fenced(&rig, &["characters/a/parts/face"], None).unwrap();
    let rx = rig.subscribe();

    let refused = rig.exec.run(set(&a.actor, "characters/a/parts/face/eyes")).unwrap_err();
    assert_eq!((refused.code, fenced_by(&refused)), (ErrorCode::Denied, Some(b.actor.clone())));
    let dry =
        rig.exec.run(Request { mode: Mode::DryRun, ..set(&a.actor, "characters/a/parts/face/eyes") }).unwrap_err();
    assert_eq!(dry, refused, "a dry run is refused the same way");
    let clashes: Vec<Event> = drain(&rx).into_iter().filter(|e| matches!(e, Event::Clash { .. })).collect();
    assert_eq!(clashes.len(), 1, "the apply clashes once, the dry run not at all");
    let Event::Clash { session, kind, other, path: at, .. } = &clashes[0] else { unreachable!() };
    assert_eq!(
        (*session, *kind, other.clone(), at.clone()),
        (a.id, ClashKind::Fenced, Some(b.actor.clone()), path("characters/a/parts/face/eyes"))
    );

    rig.exec.run(set(&a.actor, "characters/a/parts/hair/bangs")).unwrap();
    let outside = rig.exec.run(set(&a.actor, "characters/a/parts/tail")).unwrap_err();
    assert_eq!(fenced_by(&outside), None, "outside its own fence");
    let query = Request {
        actor: a.actor.clone(),
        ..agent_request("test.get", json!({"path": "characters/a/parts/face/eyes"}))
    };
    rig.exec.run(query).unwrap();

    // An unfenced agent is refused under B's fence; a person is never stopped.
    let unfenced = rig.exec.open(session_spec("free")).unwrap();
    let e = rig.exec.run(set(&unfenced.actor, "characters/a/parts/face/eyes")).unwrap_err();
    assert_eq!(fenced_by(&e), Some(b.actor.clone()));
    rig.exec.run(request("test.set", json!({"path": "characters/a/parts/face/eyes", "value": 9}))).unwrap();
    assert_eq!(rig.at("characters/a/parts/face/eyes"), Some(json!(9)));
}

#[test]
fn a_claim_ends_with_its_session() {
    let rig = rig(character());
    let a = open_fenced(&rig, &["characters/a/parts/hair"], None).unwrap();
    rig.exec.end(a.id, EndReason::Closed).unwrap();
    let next = open_fenced(&rig, &["characters/a/parts/hair"], None).unwrap();
    rig.exec.run(set(&next.actor, "characters/a/parts/hair/bangs")).unwrap();
    for reason in [EndReason::IdleTimeout, EndReason::Revoked { by: person(), credential: true }] {
        rig.exec.end(next.id, reason).unwrap();
    }
    open_fenced(&rig, &["characters/a/parts/hair"], None).unwrap();
}

#[test]
fn a_family_shares_a_claim_and_only_the_person_moves_it() {
    let rig = rig(character());
    let parent = open_fenced(&rig, &["characters/a/parts"], None).unwrap();
    let child = open_fenced(&rig, &["characters/a/parts/hair"], Some(parent.id)).unwrap();
    rig.exec.run(set(&parent.actor, "characters/a/parts/hair/bangs")).unwrap();
    rig.exec.run(set(&child.actor, "characters/a/parts/hair/bangs")).unwrap();
    let outside = open_fenced(&rig, &["characters/b"], Some(parent.id)).unwrap_err();
    assert!(
        outside.code == ErrorCode::Denied && outside.hint.as_deref().is_some_and(|h| h.contains("parent")),
        "{outside:?}"
    );
    assert_eq!(open_fenced(&rig, &[], Some(parent.id)).unwrap_err().code, ErrorCode::Denied, "unfenced under a fence");

    let tail = fence(&["characters/a/parts/tail"]);
    assert_eq!(rig.exec.set_fence(child.id, tail.clone(), &child.actor).unwrap_err().code, ErrorCode::Denied);
    let rx = rig.subscribe();
    let moved = rig.exec.set_fence(child.id, tail.clone(), &person()).unwrap();
    assert_eq!(moved.fence, tail);
    assert!(drain(&rx).iter().any(|e| matches!(e, Event::SessionChanged { change: SessionChange::Fence { .. }, .. })));
    let narrowed = rig.exec.set_fence(parent.id, fence(&["characters/a/parts/face"]), &person()).unwrap_err();
    assert_eq!(narrowed.code, ErrorCode::Denied, "the child's fence would leave it");
    assert_eq!(rig.exec.set_fence(parent.id, vec![DocPath::root()], &person()).unwrap_err().code, ErrorCode::Invalid);
}

#[test]
fn plugin_sessions_are_not_fenced() {
    let rig = rig(character());
    open_fenced(&rig, &["characters/a/parts/face"], None).unwrap();
    let spec = SessionSpec { transport: Transport::Plugin, ..session_spec("stamper") };
    let plugin = rig.exec.open(spec).unwrap();
    rig.exec.run(set(&plugin.actor, "characters/a/parts/face/eyes")).unwrap();
}
