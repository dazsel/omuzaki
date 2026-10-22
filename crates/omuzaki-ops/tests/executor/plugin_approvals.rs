//! Plugin-session approval rules (#1146, `plugins.md` §4): a scope is queued only when the session's principal and the
//! request's own grants both hold it, and a plugin session's scope needs offer only Approve or Reject.

use std::sync::{Arc, Mutex};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Decision, ErrorCode, ErrorDetail, Event, Grants, Need, Principals, Request, Resolved, Runner, Scope,
    SessionSpec, Sessions, Transport,
};
use serde_json::json;

use super::asks::{queued, wide};
use super::{drain, person, rig, session_spec};

struct Dial(Mutex<Grants>);

impl Principals for Dial {
    fn grants(&self, person: &Actor) -> Grants {
        if matches!(person, Actor::Person { .. }) { self.0.lock().unwrap().clone() } else { Grants::default() }
    }
}

fn broadcast(actor: &Actor, grants: Grants) -> Request {
    Request { grants, ..wide(actor, "test.broadcast", json!({"path": "live", "value": true})) }
}

#[test]
fn a_scope_queues_only_when_the_person_and_the_request_both_hold_it() {
    let rig = rig(json!({}));
    let rx = rig.subscribe();
    // In the ceiling: it runs.
    let holder = rig.open_with(Grants::agent_default().with(Scope::Broadcast)).actor;
    rig.exec.run(broadcast(&holder, Grants::person())).unwrap();
    // Missing from the ceiling, held by the person and the request: queued, one event.
    let agent = rig.open_narrow().actor;
    drain(&rx);
    queued(&rig.exec.run(broadcast(&agent, Grants::person())).unwrap_err());
    assert_eq!(drain(&rx).iter().filter(|e| matches!(e, Event::ApprovalRequested { .. })).count(), 1);
    // Held by the person but not by the request: refused, nothing queued, no event.
    let e = rig.exec.run(broadcast(&agent, Grants::agent_default())).unwrap_err();
    assert_eq!(
        (e.code, e.detail.as_deref()),
        (ErrorCode::Denied, Some(&ErrorDetail::MissingScope { scope: Scope::Broadcast }))
    );
    assert_eq!(rig.exec.approvals().len(), 1);
    assert!(!drain(&rx).iter().any(|e| matches!(e, Event::ApprovalRequested { .. })));

    // The person lacks it: refused, as before.
    let dial = Arc::new(Dial(Mutex::new(Grants::person().without(Scope::Broadcast))));
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_principals(dial);
    let narrow = SessionSpec { grants: Grants::agent_default(), ..session_spec("claude") };
    let actor = exec.open(narrow).unwrap().actor;
    assert_eq!(exec.run(broadcast(&actor, Grants::person())).unwrap_err().code, ErrorCode::Denied);
    assert_eq!(exec.approvals().len(), 0);
}

#[test]
fn a_plugin_sessions_scope_need_offers_only_approve_or_reject() {
    let rig = rig(json!({}));
    let spec = SessionSpec { transport: Transport::Plugin, grants: Grants::agent_default(), ..session_spec("stamper") };
    let plugin = rig.exec.open(spec).unwrap().actor;
    let (id, need) = queued(&rig.exec.run(broadcast(&plugin, Grants::person())).unwrap_err());
    assert_eq!(need, Need::Scope { scope: Scope::Broadcast });
    for widening in [Decision::ApproveForSession, Decision::ApproveForProject] {
        assert_eq!(rig.exec.resolve(id, widening, &person()).unwrap_err().code, ErrorCode::Invalid);
    }
    assert_eq!(rig.exec.approvals().len(), 1, "still queued");
    let resolution = rig.exec.resolve(id, Decision::Approve, &person()).unwrap();
    assert!(matches!(resolution.result, Resolved::Applied { .. }), "{:?}", resolution.result);
    assert_eq!(rig.log().len(), 1, "once");

    let (again, _) = queued(&rig.exec.run(broadcast(&plugin, Grants::person())).unwrap_err());
    let comment = Some("no".to_owned());
    let rejected = rig.exec.resolve(again, Decision::Reject { comment: comment.clone() }, &person()).unwrap();
    assert_eq!(rejected.result, Resolved::Rejected { comment });

    // An agent session still accepts "for this session".
    let agent = rig.open_narrow().actor;
    let (id, _) = queued(&rig.exec.run(broadcast(&agent, Grants::person())).unwrap_err());
    rig.exec.resolve(id, Decision::ApproveForSession, &person()).unwrap();
}
