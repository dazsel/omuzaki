//! Transactions: grouping, visibility, rollback, ownership and the errors around them.

use omuzaki_ops_api::{ErrorCode, Mode, Request, Runner, TxId};
use serde_json::json;

use super::{agent, drain, person, request, rig};

fn in_tx(tx: TxId, op: &str, args: serde_json::Value) -> Request {
    Request { tx: Some(tx), ..request(op, args) }
}

#[test]
fn ops_in_a_transaction_share_its_id_and_are_one_undo_step() {
    let rig = rig(json!({"a": 1, "b": 2}));
    let tx = rig.exec.begin("make the face more oval", &person());
    let first = rig.exec.run(in_tx(tx, "test.set", json!({"path": "a", "value": 10}))).unwrap().applied.unwrap();
    let second = rig.exec.run(in_tx(tx, "test.set", json!({"path": "b", "value": 20}))).unwrap().applied.unwrap();
    assert_eq!((first.tx, second.tx), (tx, tx));
    rig.exec.commit(tx, &person()).unwrap();

    assert_eq!(rig.undo().unwrap().as_deref(), Some("make the face more oval"));
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(1)), Some(json!(2))));
    assert!(rig.undo().unwrap().is_none(), "both ops were one step");
}

#[test]
fn an_op_without_a_transaction_is_its_own_one_op_step_labelled_by_its_name() {
    let rig = rig(json!({}));
    let a = rig.set("a", json!(1)).applied.unwrap();
    let b = rig.set("b", json!(2)).applied.unwrap();
    assert_ne!(a.tx, b.tx);
    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(1)), None));
}

#[test]
fn ops_in_an_open_transaction_apply_immediately_and_later_ops_see_them() {
    let rig = rig(json!({}));
    let tx = rig.exec.begin("grow", &person());
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "node", "value": {}}))).unwrap();
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "node/x", "value": 5}))).unwrap();
    assert_eq!(rig.at("node/x"), Some(json!(5)));
    let answer = rig.exec.run(in_tx(tx, "test.get", json!({"path": "node/x"}))).unwrap();
    assert_eq!(answer.plan.output, Some(json!(5)));
    assert_eq!(rig.log().len(), 2, "the query is not logged");
}

#[test]
fn rollback_reverts_everything_in_reverse_order_and_closes_the_transaction() {
    let rig = rig(json!({"keep": 1}));
    let tx = rig.exec.begin("try it", &person());
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "keep", "value": 2}))).unwrap();
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "node", "value": {}}))).unwrap();
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "node/x", "value": 1}))).unwrap();
    rig.exec.run(in_tx(tx, "test.remove", json!({"path": "keep"}))).unwrap();

    rig.exec.rollback(tx, &person()).unwrap();
    assert_eq!(rig.exec.get(&super::path(""), &omuzaki_ops_api::Grants::person()).unwrap(), Some(json!({"keep": 1})));
    assert!(rig.undo().unwrap().is_none(), "a rolled-back transaction leaves no undo step");
    assert_eq!(rig.log().len(), 4, "the log is history: rolled-back ops stay in it");
    for again in [rig.exec.commit(tx, &person()), rig.exec.rollback(tx, &person())] {
        assert_eq!(again.unwrap_err().code, ErrorCode::NotFound);
    }
}

#[test]
fn a_rollback_that_cannot_apply_conflicts_and_leaves_the_transaction_open() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("mine", &person());
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "a", "value": 2}))).unwrap();
    // Someone else edits the same path before the rollback.
    rig.set("a", json!(3));

    let e = rig.exec.rollback(tx, &person()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert_eq!(rig.at("a"), Some(json!(3)), "a failed rollback changes nothing");
    rig.exec.commit(tx, &person()).unwrap(); // still open, so it can be committed instead
}

#[test]
fn an_open_transaction_belongs_to_its_actor() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("alice's", &person());
    let mine_not = Request { actor: agent(), ..in_tx(tx, "test.set", json!({"path": "a", "value": 2})) };
    let e = rig.exec.run(mine_not).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(e.message.contains("person alice"), "{e}");
    assert_eq!(rig.at("a"), Some(json!(1)));
    assert_eq!(rig.log(), vec![]);

    rig.exec.run(in_tx(tx, "test.set", json!({"path": "a", "value": 2}))).unwrap();
    assert_eq!(rig.at("a"), Some(json!(2)));
}

#[test]
fn closed_or_unknown_transactions_are_not_found_even_for_dry_runs_and_queries() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("t", &person());
    rig.exec.commit(tx, &person()).unwrap();
    for stale in [tx, TxId(999)] {
        for req in [
            in_tx(stale, "test.set", json!({"path": "a", "value": 2})),
            Request { mode: Mode::DryRun, ..in_tx(stale, "test.set", json!({"path": "a", "value": 2})) },
            in_tx(stale, "test.get", json!({"path": "a"})),
        ] {
            assert_eq!(rig.exec.run(req).unwrap_err().code, ErrorCode::NotFound);
        }
    }
    assert_eq!(rig.exec.commit(TxId(999), &person()).unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(rig.at("a"), Some(json!(1)));
}

#[test]
fn a_failing_op_inside_a_transaction_leaves_the_transaction_usable() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("t", &person());
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "a", "value": 2}))).unwrap();
    assert!(rig.exec.run(in_tx(tx, "test.reject", json!({}))).is_err());
    assert!(rig.exec.run(in_tx(tx, "test.set_if", json!({"path": "a", "expect": 0, "value": 9}))).is_err());
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "a", "value": 3}))).unwrap();
    rig.exec.commit(tx, &person()).unwrap();
    rig.undo().unwrap();
    assert_eq!(rig.at("a"), Some(json!(1)), "the two successful ops were one step");
}

#[test]
fn transactions_get_distinct_ids_even_when_opened_back_to_back() {
    let rig = rig(json!({}));
    let ids: Vec<TxId> = (0..3).map(|_| rig.exec.begin("t", &person())).collect();
    assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);
}

#[test]
fn committing_an_empty_transaction_adds_no_undo_step_and_emits_the_event() {
    let rig = rig(json!({}));
    rig.set("a", json!(1));
    let rx = rig.subscribe();
    let tx = rig.exec.begin("nothing", &person());
    rig.exec.commit(tx, &person()).unwrap();
    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"), "the empty transaction was not a step");
    assert!(matches!(drain(&rx).first(), Some(omuzaki_ops_api::Event::TxCommitted { tx: t, .. }) if *t == tx));
}

#[test]
fn only_the_owner_can_commit_or_roll_back_a_transaction() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("alice's", &person());
    rig.exec.run(in_tx(tx, "test.set", json!({"path": "a", "value": 2}))).unwrap();

    for refused in [rig.exec.commit(tx, &agent()), rig.exec.rollback(tx, &agent())] {
        let e = refused.unwrap_err();
        assert_eq!(e.code, ErrorCode::Denied);
        assert!(e.message.contains("person alice"), "{e}");
    }
    assert_eq!(rig.at("a"), Some(json!(2)), "a refused rollback reverts nothing");
    assert_eq!(rig.agent_undo().unwrap(), None, "a refused commit made no undo step for anyone");

    // The transaction is still open and still the owner's.
    rig.exec.rollback(tx, &person()).unwrap();
    assert_eq!(rig.at("a"), Some(json!(1)));
}

#[test]
fn a_stranger_asking_about_a_closed_transaction_gets_not_found_not_a_hint_about_its_owner() {
    let rig = rig(json!({}));
    let tx = rig.exec.begin("t", &person());
    rig.exec.commit(tx, &person()).unwrap();
    assert_eq!(rig.exec.commit(tx, &agent()).unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(rig.exec.rollback(tx, &agent()).unwrap_err().code, ErrorCode::NotFound);
}
