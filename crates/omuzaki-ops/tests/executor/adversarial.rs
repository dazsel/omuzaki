//! Adversarial cases from the review, adapted to the per-actor undo and read-scope contract: each one broke
//! an earlier version of the executor (value leaks, wedged history, undo reaching across actors, deadlock).

use std::sync::Arc;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{ErrorCode, Grants, Mode, Request, Runner, Scope};
use serde_json::json;

use super::{agent, agent_request, person, request, rig};

/// A dry-run plan carries `before` values, so it needs `read` like everything else.
#[test]
fn dry_run_of_an_edit_without_read_is_denied_and_reveals_nothing() {
    let rig = rig(json!({"secret": "hunter2"}));
    for grants in [Grants::default(), Grants::of([Scope::EditCharacter])] {
        let req = Request {
            mode: Mode::DryRun,
            actor: agent(),
            grants,
            ..request("test.set", json!({"path": "secret", "value": "x"}))
        };
        let e = rig.exec.run(req).unwrap_err();
        assert_eq!(e.code, ErrorCode::Denied);
        assert!(!format!("{e:?}").contains("hunter2"), "LEAK: {e:?}");
    }
}

/// A conflict message quotes the values it found, so an actor holding an edit scope but not `read` must be stopped
/// before the conflict can happen; one that holds `read` may see them.
#[test]
fn conflict_messages_quote_values_only_for_actors_with_read() {
    let rig = rig(json!({"secret": "hunter2"}));
    let stale = |grants| Request {
        actor: agent(),
        grants,
        ..request("test.set_if", json!({"path": "secret", "expect": "guess", "value": "x"}))
    };
    let e = rig.exec.run(stale(Grants::of([Scope::EditCharacter]))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(!format!("{e:?}").contains("hunter2"), "LEAK: {e:?}");

    let e = rig.exec.run(stale(Grants::of([Scope::Read, Scope::EditCharacter]))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
}

/// Undo, redo and rollback don't take `read`, so their conflicts name paths and actors but never values.
#[test]
fn undo_and_rollback_conflicts_never_quote_document_values() {
    let rig = rig(json!({"secret": "hunter2"}));
    rig.set("secret", json!("swordfish"));
    rig.agent_set("secret", json!("opensesame"));
    let e = rig.undo().unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    let shown = format!("{e:?}");
    assert!(shown.contains("secret"), "the path is fair game: {shown}");
    for value in ["hunter2", "swordfish", "opensesame"] {
        assert!(!shown.contains(value), "LEAK of {value}: {shown}");
    }

    let tx = rig.exec.begin("agent's", &agent());
    rig.exec
        .run(Request { tx: Some(tx), ..agent_request("test.set", json!({"path": "secret", "value": "abracadabra"})) })
        .unwrap();
    rig.set("secret", json!("shazam"));
    let e = rig.exec.rollback(tx, &agent()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    let shown = format!("{e:?}");
    assert!(shown.contains("person alice"), "{shown}");
    for value in ["opensesame", "abracadabra", "shazam"] {
        assert!(!shown.contains(value), "LEAK of {value}: {shown}");
    }
}

/// T applies `x` first but commits last, with an edit to `x` by someone else in between. With one global stack this
/// wedged undo for good; per actor, both can always be undone.
#[test]
fn undo_is_not_wedged_by_a_transaction_that_commits_after_a_later_edit_to_the_same_path() {
    let build = || {
        let rig = rig(json!({"x": 0}));
        let tx = rig.exec.begin("long agent job", &agent());
        rig.exec.run(Request { tx: Some(tx), ..agent_request("test.set", json!({"path": "x", "value": 1})) }).unwrap();
        rig.set("x", json!(2)); // the person edits the same field meanwhile
        rig.exec.commit(tx, &agent()).unwrap();
        rig
    };

    // Person first, then the agent: both succeed and the original value is reachable.
    let rig = build();
    assert!(rig.undo().unwrap().is_some());
    assert!(rig.agent_undo().unwrap().is_some());
    assert_eq!(rig.at("x"), Some(json!(0)), "undoing everything reaches the original value");

    // Agent first: its step conflicts and is dropped, which frees the rest of its history; nothing is stuck.
    let rig = build();
    assert_eq!(rig.agent_undo().unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(rig.agent_undo().unwrap(), None);
    assert!(rig.undo().unwrap().is_some());
    assert_eq!(rig.at("x"), Some(json!(1)));
    assert_eq!(rig.undo().unwrap(), None);
}

/// Undo never reaches across actors, in either direction.
#[test]
fn undo_never_reverts_another_actors_step() {
    let rig = rig(json!({"x": 0}));
    rig.set("x", json!(1)); // by the person
    assert_eq!(rig.agent_undo().unwrap(), None, "an agent must not undo a person's edit");
    assert_eq!(rig.at("x"), Some(json!(1)));
    rig.agent_set("y", json!(1));
    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("y"), Some(json!(1)), "the person's undo left the agent's edit alone");
}

/// Churn subscribers (subscribe, drop, never read) while four threads run ops, transactions, undo and redo.
/// Everything must finish; a watchdog turns a deadlock into a failure.
#[test]
fn subscriber_churn_during_concurrent_ops_does_not_deadlock() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Arc::new(Executor::new(Box::new(MemDoc::new()), clock).unwrap());
    let _slow = exec.subscribe(&Grants::person()).unwrap(); // never drained
    let (done_tx, done_rx) = channel();
    let mut handles = Vec::new();
    for t in 0..4 {
        let exec = Arc::clone(&exec);
        let done = done_tx.clone();
        handles.push(thread::spawn(move || {
            for i in 0..200 {
                let _rx = exec.subscribe(&Grants::person()).unwrap();
                let args = json!({"path": format!("t{t}_{i}"), "value": i});
                exec.run(request("test.set", args)).unwrap();
                let tx = exec.begin("tx", &person());
                let joined =
                    Request { tx: Some(tx), ..request("test.set", json!({"path": format!("u{t}_{i}"), "value": 1})) };
                let _ = exec.run(joined);
                if i % 2 == 0 {
                    let _ = exec.commit(tx, &person());
                } else {
                    let _ = exec.rollback(tx, &person());
                }
                let _ = exec.undo(&person(), &Grants::person());
                let _ = exec.redo(&person(), &Grants::person());
            }
            done.send(()).unwrap();
        }));
    }
    for _ in 0..4 {
        done_rx.recv_timeout(Duration::from_secs(60)).expect("deadlock or hang");
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let log = exec.log(0, usize::MAX, &Grants::person()).unwrap();
    assert_eq!(log.iter().map(|r| r.seq).collect::<Vec<_>>(), (1..=log.len() as u64).collect::<Vec<_>>());
}
