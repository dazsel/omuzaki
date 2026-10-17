//! Requests caused by a script never queue (#1095, ADR 0034 decision 4): a queued call would run later as its own
//! transaction, after the script's run ended and rolled back. They are refused as if no approver were present. Macro
//! replay is a script; the Build Theatre's re-runs (`Replay`) still queue.

use omuzaki_ops_api::{Cause, CauseKind, ErrorCode, ErrorDetail, Event, Request, Runner, Scope, Sessions};
use serde_json::json;

use super::asks::{queued, wide};
use super::{drain, rig};

fn caused(mut req: Request, kind: CauseKind, text: &str) -> Request {
    req.cause = Some(Cause { kind, text: Some(text.to_owned()) });
    req
}

fn needs(actor: &omuzaki_ops_api::Actor) -> [Request; 2] {
    [
        wide(actor, "test.publish", json!({"path": "p", "value": 1})),
        wide(actor, "test.confirm", json!({"path": "m", "value": 1})),
    ]
}

#[test]
fn a_script_cause_is_refused_with_the_need_and_a_hint_and_queues_nothing() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let rx = rig.subscribe();
    for text in ["fix the bangs", "macro: demo"] {
        let [scope, person] = needs(&agent);
        let e = rig.exec.run(caused(scope, CauseKind::Script, text)).unwrap_err();
        assert_eq!(
            (e.code, e.detail.as_deref()),
            (ErrorCode::Denied, Some(&ErrorDetail::MissingScope { scope: Scope::Publish }))
        );
        assert!(e.hint.as_deref().is_some_and(|h| h.contains("before running the script")), "{e:?}");
        let e = rig.exec.run(caused(person, CauseKind::Script, text)).unwrap_err();
        assert!(
            e.code == ErrorCode::Denied && matches!(e.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })),
            "{e:?}"
        );
    }
    assert_eq!(rig.exec.approvals().len(), 0);
    assert!(!drain(&rx).iter().any(|e| matches!(e, Event::ApprovalRequested { .. })));
    let only = rig
        .exec
        .run(caused(wide(&agent, "test.person_only", json!({"path": "d"})), CauseKind::Script, "s"))
        .unwrap_err();
    assert!(matches!(only.detail.as_deref(), Some(ErrorDetail::PersonOnly { .. })), "person-only is refused as before");
}

#[test]
fn other_causes_still_queue() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    for kind in [CauseKind::Prompt, CauseKind::Direct, CauseKind::Replay] {
        for req in needs(&agent) {
            queued(&rig.exec.run(caused(req, kind, "go")).unwrap_err());
        }
    }
    assert_eq!(rig.exec.approvals().len(), 6);
}

#[test]
fn a_failed_macro_replay_leaves_no_approval_behind() {
    let rig = rig(json!({"a": 0}));
    let agent = rig.open_narrow().actor;
    let tx = rig.exec.begin("macro: demo", &agent);
    let step = |req: Request| Request { tx: Some(tx), ..caused(req, CauseKind::Script, "macro: demo") };
    rig.exec.run(step(wide(&agent, "test.set", json!({"path": "a", "value": 1})))).unwrap();
    let refused = rig.exec.run(step(wide(&agent, "test.publish", json!({"path": "p", "value": 1})))).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Denied);
    rig.exec.rollback(tx, &agent).unwrap();
    assert_eq!(rig.exec.approvals().len(), 0);
    assert_eq!(rig.at("a"), Some(json!(0)));
}
