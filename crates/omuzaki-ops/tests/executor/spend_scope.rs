//! A plan that costs money needs `spend` (#1003, `connectors.md` §5): the executor knows the cost only after the op
//! planned, so the rule lives there. A session without `spend` waits for a person like any missing scope; the scope is
//! asked before the budget; a dry-run never needs it.

use omuzaki_ops_api::{
    Budget, Decision, ErrorCode, ErrorDetail, Grants, Mode, Need, Request, Resolved, Runner, Scope, SessionSpec,
    Sessions, Spend,
};
use serde_json::json;

use super::asks::{queued, wide};
use super::{person, request, rig, session_spec};

const DOLLAR: u64 = 1_000_000;

fn paid(actor: &omuzaki_ops_api::Actor) -> Request {
    wide(actor, "test.paid", json!({"path": "a", "value": 1}))
}

#[test]
fn an_agent_without_spend_waits_and_the_budget_still_decides() {
    for (budget, applies) in [(DOLLAR, true), (0, false)] {
        let rig = rig(json!({}));
        let spec = SessionSpec {
            grants: Grants::agent_default(),
            budget: Budget { usd_micros: budget, ..Budget::default() },
            ..session_spec("claude")
        };
        let session = rig.exec.open(spec).unwrap();
        let (id, need) = queued(&rig.exec.run(paid(&session.actor)).unwrap_err());
        assert_eq!(need, Need::Scope { scope: Scope::Spend });
        assert_eq!(rig.at("a"), None, "nothing ran");
        let resolution = rig.exec.resolve(id, Decision::Approve, &person()).unwrap();
        if applies {
            assert!(matches!(resolution.result, Resolved::Applied { .. }), "{:?}", resolution.result);
            assert_eq!(rig.exec.session(session.id).unwrap().spent.usd_micros, 120_000);
            // The approval was for that one run: the next paid request asks again.
            assert_eq!(queued(&rig.exec.run(paid(&session.actor)).unwrap_err()).1, Need::Scope { scope: Scope::Spend });
        } else {
            let Resolved::Failed { error } = resolution.result else { panic!("{:?}", resolution.result) };
            let (_, need) = queued(&error);
            let left = Spend { usd_micros: 0, tokens: u64::MAX, seconds: u64::MAX };
            assert_eq!(need, Need::Budget { needed: Spend { usd_micros: 120_000, ..Spend::default() }, left });
        }
    }
}

#[test]
fn a_dry_run_shows_the_cost_and_asks_for_nothing() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let outcome = rig.exec.run(Request { mode: Mode::DryRun, ..paid(&agent) }).unwrap();
    assert_eq!(outcome.plan.cost.and_then(|c| c.usd), Some(0.12));
    assert_eq!(rig.exec.approvals().len(), 0);
}

#[test]
fn a_person_or_a_session_holding_spend_with_a_budget_just_runs_it() {
    let rig = rig(json!({}));
    rig.exec.run(request("test.paid", json!({"path": "a", "value": 1}))).unwrap();
    let spec = SessionSpec {
        grants: Grants::agent_default().with(Scope::Spend),
        budget: Budget { usd_micros: DOLLAR, ..Budget::default() },
        ..session_spec("claude")
    };
    let session = rig.exec.open(spec).unwrap();
    rig.exec.run(wide(&session.actor, "test.paid", json!({"path": "b", "value": 1}))).unwrap();
    assert_eq!(rig.exec.approvals().len(), 0);
}

#[test]
fn a_free_plan_never_asks_for_spend() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    for zero in [false, true] {
        rig.exec.run(wide(&agent, "test.free", json!({"path": "a", "value": zero, "zero": zero}))).unwrap();
    }
    assert_eq!(rig.exec.approvals().len(), 0);
}

#[test]
fn a_broken_cost_is_a_bug_in_the_op() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    for bad in ["nan", "infinite", "negative"] {
        for actor in [agent.clone(), person()] {
            let req = wide(&actor, "test.paid", json!({"path": "a", "value": 1, "bad": bad}));
            let e = rig.exec.run(req).unwrap_err();
            assert_eq!(e.code, ErrorCode::Internal, "{bad}: {e:?}");
            assert!(e.message.contains("test.paid"), "{e}");
        }
    }
    assert_eq!(rig.at("a"), None);
}

#[test]
fn a_session_whose_request_lacks_spend_is_refused() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let req = Request { grants: Grants::agent_default(), ..paid(&agent) };
    let e = rig.exec.run(req).unwrap_err();
    assert_eq!(
        (e.code, e.detail.as_deref()),
        (ErrorCode::Denied, Some(&ErrorDetail::MissingScope { scope: Scope::Spend }))
    );
    assert_eq!(rig.exec.approvals().len(), 0);
}
