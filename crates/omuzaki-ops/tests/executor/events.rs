//! Events: one channel per subscriber, log order, nothing for failures, safe under concurrency.

use std::sync::Arc;
use std::thread;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocVersion;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Event, Grants, Request, Runner};
use serde_json::json;

use super::{drain, open_agents, person, request, rig};

#[test]
fn applying_an_op_emits_op_applied_then_tx_committed() {
    let rig = rig(json!({}));
    let rx = rig.subscribe();
    let record = rig.set("a", json!(1)).applied.unwrap();
    assert_eq!(
        drain(&rx),
        vec![Event::op_applied(record.clone()), Event::tx_committed(record.tx, super::SET, person())]
    );
}

#[test]
fn a_transaction_emits_each_op_then_one_commit_or_rollback() {
    let rig = rig(json!({}));
    let rx = rig.subscribe();
    let tx = rig.exec.begin("pair", &person());
    for k in ["a", "b"] {
        rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": k, "value": 1})) }).unwrap();
    }
    assert_eq!(drain(&rx).len(), 2, "no commit event until commit");
    rig.exec.commit(tx, &person()).unwrap();
    assert_eq!(drain(&rx), vec![Event::tx_committed(tx, "pair", person())]);

    let doomed = rig.exec.begin("doomed", &person());
    rig.exec.run(Request { tx: Some(doomed), ..request("test.set", json!({"path": "c", "value": 1})) }).unwrap();
    rig.exec.rollback(doomed, &person()).unwrap();
    let events = drain(&rx);
    assert!(
        matches!(events.as_slice(), [Event::OpApplied { .. }, Event::TxRolledBack { tx: t, actor, .. }] if *t == doomed && *actor == person())
    );
}

#[test]
fn undo_and_redo_emit_their_events() {
    let rig = rig(json!({}));
    let tx = rig.set("a", json!(1)).applied.unwrap().tx;
    let rx = rig.subscribe();
    rig.undo().unwrap();
    rig.redo().unwrap();
    assert_eq!(
        drain(&rx),
        vec![
            Event::undone(tx, super::SET, person(), person(), DocVersion(2)),
            Event::redone(tx, super::SET, person(), person(), DocVersion(3)),
        ]
    );
}

#[test]
fn every_subscriber_has_its_own_channel_and_late_ones_miss_earlier_events() {
    let rig = rig(json!({}));
    let (first, second) = (rig.subscribe(), rig.subscribe());
    rig.set("a", json!(1));
    let late = rig.subscribe();
    rig.set("b", json!(2));
    assert_eq!(drain(&first).len(), 4);
    assert_eq!(drain(&second).len(), 4);
    assert_eq!(drain(&late).len(), 2);
}

#[test]
fn dropped_receivers_do_not_disturb_running_ops_or_other_subscribers() {
    let rig = rig(json!({}));
    let kept = rig.subscribe();
    drop(rig.subscribe());
    rig.set("a", json!(1));
    rig.set("b", json!(2));
    assert_eq!(drain(&kept).len(), 4);
}

#[test]
fn failures_dry_runs_and_queries_emit_nothing() {
    let rig = rig(json!({"a": 1}));
    let rx = rig.subscribe();
    let _ = rig.run("test.reject", json!({}));
    let _ = rig.run("test.set_if", json!({"path": "a", "expect": 5, "value": 2}));
    let _ = rig.run("test.get", json!({"path": "a"}));
    let _ = rig.exec.commit(omuzaki_ops_api::TxId(42), &person());
    assert_eq!(drain(&rx), vec![]);
}

/// Four threads apply ops at once. The log must stay gapless and every subscriber must see events in log order:
/// that is what lets the Build Theatre trust event order.
#[test]
fn concurrent_ops_keep_the_log_gapless_and_events_in_log_order() {
    const THREADS: usize = 4;
    const PER_THREAD: usize = 50;
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::new()), clock).unwrap();
    open_agents(&exec);
    let exec = Arc::new(exec);
    let rx = exec.subscribe(&Grants::person()).unwrap();

    let workers: Vec<_> = (0..THREADS)
        .map(|t| {
            let exec = Arc::clone(&exec);
            thread::spawn(move || {
                for i in 0..PER_THREAD {
                    let args = json!({"path": format!("t{t}_{i}"), "value": i});
                    exec.run(request("test.set", args)).unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }

    let log = exec.log(0, usize::MAX, &Grants::person()).unwrap();
    assert_eq!(log.iter().map(|r| r.seq).collect::<Vec<_>>(), (1..=(THREADS * PER_THREAD) as u64).collect::<Vec<_>>());
    assert!(log.windows(2).all(|w| w[0].version < w[1].version), "versions follow the log order");

    let applied: Vec<u64> = drain(&rx)
        .into_iter()
        .filter_map(|e| if let Event::OpApplied { record, .. } = e { Some(record.seq) } else { None })
        .collect();
    assert_eq!(applied, log.iter().map(|r| r.seq).collect::<Vec<_>>());
}
