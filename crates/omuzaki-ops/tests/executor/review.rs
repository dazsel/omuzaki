//! Team review (#1283, design `agent-sessions.md` §7.1, §7.5): with policies installed, a publish op and a broadcast
//! grant wait for the policy's reviewers; never the requester's own person; N distinct approvals run it once.

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, ApprovalRef, Decision, ErrorCode, Event, Grants, Need, Request, Resolved, ReviewPolicies, ReviewRequirement,
    Runner, Scope, SessionSpec, Sessions,
};
use serde_json::{Value, json};

use super::asks::{queued, wide};
use super::{drain, open_agents, person, session_spec};

fn named(name: &str) -> Actor {
    Actor::Person { name: name.into() }
}

/// `publish` needs `approvals` reviewers; a `broadcast` grant needs one; sam and kim review, nobody else.
struct Team {
    approvals: u8,
}

impl ReviewPolicies for Team {
    fn requirement(&self, scope: Scope, grant: bool) -> Option<ReviewRequirement> {
        match (scope, grant) {
            (Scope::Publish, false) => Some(ReviewRequirement::new("publish", self.approvals)),
            (Scope::Broadcast, true) => Some(ReviewRequirement::new("broadcast", 1)),
            _ => None,
        }
    }

    fn may_approve(&self, person: &Actor, _principal: &Actor, _policy: &str) -> bool {
        [named("sam"), named("kim")].contains(person)
    }
}

/// An executor with the team's policies and an agent that holds `publish` (and not `broadcast`).
fn team(approvals: u8, doc: Value) -> (Executor, Actor) {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(doc)), clock)
        .unwrap()
        .with_review_policies(Arc::new(Team { approvals }));
    open_agents(&exec);
    let grants = Grants::agent_default().with(Scope::Publish);
    let agent = exec.open(SessionSpec { grants, ..session_spec("claude") }).unwrap().actor;
    (exec, agent)
}

fn publish(agent: &Actor, path: &str) -> Request {
    wide(agent, "test.publish", json!({"path": path, "value": 1}))
}

#[test]
fn a_publish_waits_for_a_reviewer_who_is_not_the_requesters_person() {
    let (exec, agent) = team(1, json!({}));
    let rx = exec.subscribe(&Grants::person()).unwrap();
    let (id, need) = queued(&exec.run(publish(&agent, "p")).unwrap_err());
    assert_eq!(need, Need::Review { policy: "publish".into() });
    let requested = drain(&rx).into_iter().find_map(|e| match e {
        Event::ApprovalRequested { approval, .. } => Some(approval.need),
        _ => None,
    });
    assert_eq!(requested, Some(Need::Review { policy: "publish".into() }), "the event names the policy");

    assert_eq!(exec.resolve(id, Decision::Approve, &person()).unwrap_err().code, ErrorCode::Denied, "the principal");
    assert_eq!(exec.resolve(id, Decision::Approve, &named("eve")).unwrap_err().code, ErrorCode::Denied, "no reviewer");
    assert_eq!(exec.resolve(id, Decision::ApproveForSession, &named("sam")).unwrap_err().code, ErrorCode::Invalid);
    let resolution = exec.resolve(id, Decision::Approve, &named("sam")).unwrap();
    let Resolved::Applied { outcome } = resolution.result else { panic!("{:?}", resolution.result) };
    let record = outcome.applied.unwrap();
    assert_eq!(record.approval, Some(ApprovalRef::new(id, named("sam"))));
    assert_eq!(exec.log(0, 10, &Grants::person()).unwrap().len(), 1, "once, as its own transaction");
    assert_eq!(exec.approvals().len(), 0);
}

#[test]
fn two_reviewers_must_be_distinct_and_both_are_on_the_record() {
    let (exec, agent) = team(2, json!({}));
    let (id, _) = queued(&exec.run(publish(&agent, "p")).unwrap_err());
    for _ in 0..2 {
        let first = exec.resolve(id, Decision::Approve, &named("sam")).unwrap_err();
        assert_eq!(queued(&first).0, id, "still pending: the same reviewer counts once");
    }
    let resolution = exec.resolve(id, Decision::Approve, &named("kim")).unwrap();
    let Resolved::Applied { outcome } = resolution.result else { panic!("{:?}", resolution.result) };
    let approval = outcome.applied.unwrap().approval.unwrap();
    assert_eq!((approval.by, approval.also), (named("kim"), vec![named("sam")]));
}

#[test]
fn a_rejection_reaches_the_agent_and_a_failing_dry_run_queues_nothing() {
    let (exec, agent) = team(1, json!({}));
    let (id, _) = queued(&exec.run(publish(&agent, "p")).unwrap_err());
    let comment = Some("not this one".to_owned());
    let resolution = exec.resolve(id, Decision::Reject { comment: comment.clone() }, &named("kim")).unwrap();
    assert_eq!(resolution.result, Resolved::Rejected { comment });
    assert_eq!(exec.get(&super::path("p"), &Grants::person()).unwrap(), None);
    // The gate in the dry-run (here, a missing parent) fails first: nothing to review.
    assert_eq!(exec.run(publish(&agent, "gone/p")).unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(exec.approvals().len(), 0);
}

#[test]
fn a_broadcast_grant_is_reviewed_and_given_for_that_run() {
    let (exec, agent) = team(1, json!({}));
    let req = wide(&agent, "test.broadcast", json!({"path": "live", "value": true}));
    let (id, need) = queued(&exec.run(req.clone()).unwrap_err());
    assert_eq!(need, Need::Review { policy: "broadcast".into() });
    let resolution = exec.resolve(id, Decision::Approve, &named("sam")).unwrap();
    assert!(matches!(resolution.result, Resolved::Applied { .. }), "{:?}", resolution.result);
    assert_eq!(exec.get(&super::path("live"), &Grants::person()).unwrap(), Some(json!(true)));
    assert_eq!(queued(&exec.run(req).unwrap_err()).1, Need::Review { policy: "broadcast".into() }, "one run only");
}

#[test]
fn persons_are_not_reviewed_here() {
    let (exec, _) = team(1, json!({}));
    exec.run(super::request("test.publish", json!({"path": "p", "value": 1}))).unwrap();
    assert_eq!(exec.approvals().len(), 0);
}
