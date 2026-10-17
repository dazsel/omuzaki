//! The controls of #2106 against requests on other threads, and a property of undoing a session.
//!
//! - An end, like a pause, wins against a request admitted a moment before it: the request is checked again under the
//!   lock that applies it, so it either applied before the end or is refused after it, never applied after.
//! - An approval resolves exactly once, however many people press Approve at once, and an end withdraws it or lets
//!   it run, never both.
//! - Requests racing for the last of a budget never spend past it.
//! - Undoing a session and then undoing that step (the person's Ctrl+Z) gives back the document exactly; redo gives
//!   the undone one back; paths the agent never touched are never changed (fixed seed).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocPath;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, Decision, EndReason, ErrorCode, Event, Grants, Principals, Request, Resolved, Runner, Scope,
    SessionId, SessionSpec, Sessions,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::{Value, json};

use super::asks::{queued, wide};
use super::{agent, agent_request, drain, person, request, rig, session_spec};

/// A principal source whose next question waits for the test, as in the pause race of `sessions.rs`.
#[derive(Default)]
struct Pinned(Mutex<Option<(Sender<()>, Receiver<()>)>>);

impl Pinned {
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

#[test]
fn an_end_never_lets_an_admitted_request_apply_after_it() {
    for _ in 0..20 {
        let pinned = Arc::new(Pinned::default());
        let clock = Arc::new(FakeClock::new(Seconds(0.0)));
        let exec = Arc::new(
            Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_principals(pinned.clone()),
        );
        let info = exec.open(session_spec("claude")).unwrap();
        let events = exec.subscribe(&Grants::person()).unwrap();
        // The request's early look is pinned; the end queues for the lock behind it.
        let (entered, release) = pinned.arm();
        let (run_exec, actor) = (exec.clone(), info.actor.clone());
        let running = std::thread::spawn(move || {
            run_exec.run(Request { actor, ..agent_request("test.set", json!({"path": "a", "value": 1})) })
        });
        entered.recv().unwrap();
        let (end_exec, id) = (exec.clone(), info.id);
        let ending = std::thread::spawn(move || end_exec.end(id, EndReason::Disconnected));
        std::thread::sleep(Duration::from_millis(20));
        release.send(()).unwrap();
        let ran = running.join().unwrap();
        ending.join().unwrap().unwrap();

        let order: Vec<&str> = drain(&events)
            .iter()
            .filter_map(|e| match e {
                Event::OpApplied { .. } => Some("applied"),
                Event::SessionEnded { .. } => Some("ended"),
                _ => None,
            })
            .collect();
        match ran {
            Ok(_) => assert_eq!(order, ["applied", "ended"]),
            Err(e) => {
                assert_eq!((e.code, order), (ErrorCode::Denied, vec!["ended"]), "{e:?}");
                assert_eq!(exec.get(&DocPath::parse("a").unwrap(), &Grants::person()).unwrap(), None);
            }
        }
    }
}

#[test]
fn an_approval_resolves_exactly_once_under_concurrent_decisions_and_ends() {
    for _ in 0..20 {
        let rig = Arc::new(rig(json!({})));
        let session = rig.open_narrow();
        let (id, _) =
            queued(&rig.exec.run(wide(&session.actor, "test.publish", json!({"path": "p", "value": 1}))).unwrap_err());
        let start = Arc::new(Barrier::new(9));
        let mut deciding = Vec::new();
        for _ in 0..8 {
            let (rig, start) = (rig.clone(), start.clone());
            deciding.push(std::thread::spawn(move || {
                start.wait();
                rig.exec.resolve(id, Decision::Approve, &person())
            }));
        }
        let (closer, end_start) = (rig.clone(), start.clone());
        let ending = std::thread::spawn(move || {
            end_start.wait();
            closer.exec.end(session.id, EndReason::Closed)
        });
        let results: Vec<_> = deciding.into_iter().map(|t| t.join().unwrap()).collect();
        let ended = ending.join().unwrap().unwrap();
        let resolved: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert!(results.iter().filter_map(|r| r.as_ref().err()).all(|e| e.code == ErrorCode::NotFound));
        match resolved.as_slice() {
            [one] => {
                assert!(matches!(one.result, Resolved::Applied { .. }), "{:?}", one.result);
                assert_eq!(ended.withdrawn.len(), 0);
                assert_eq!(rig.log().len(), 1);
            }
            [] => {
                assert_eq!(ended.withdrawn, vec![id], "withdrawn instead");
                assert_eq!(rig.log().len(), 0);
            }
            many => panic!("resolved {} times", many.len()),
        }
    }
}

#[test]
fn racing_requests_never_spend_past_the_budget() {
    let rig = Arc::new(rig(json!({})));
    let spec = SessionSpec {
        grants: Grants::agent_default().with(Scope::Spend),
        budget: Budget { usd_micros: 1_000_000, ..Budget::default() },
        ..session_spec("claude")
    };
    let session = rig.exec.open(spec).unwrap();
    let start = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let (rig, start, actor) = (rig.clone(), start.clone(), session.actor.clone());
            std::thread::spawn(move || {
                start.wait();
                rig.exec.run(wide(&actor, "test.paid", json!({"path": format!("p{i}"), "value": i, "usd": 0.4})))
            })
        })
        .collect();
    let applied = threads.into_iter().flat_map(|t| t.join().unwrap()).count();
    assert_eq!(applied, 2);
    assert_eq!(rig.exec.session(session.id).unwrap().spent.usd_micros, 800_000);
    assert_eq!(rig.exec.approvals().len(), 1, "one budget approval; the rest found the session paused");
}

#[derive(Debug, Clone)]
enum Step {
    Agent(&'static str, i64),
    Person(&'static str, i64),
}

fn step() -> impl Strategy<Value = Step> {
    let key = prop::sample::select(vec!["a", "b", "c", "d"]);
    prop_oneof![
        3 => (key.clone(), 0..4_i64).prop_map(|(k, v)| Step::Agent(k, v)),
        1 => (key, 0..4_i64).prop_map(|(k, v)| Step::Person(k, v)),
    ]
}

fn document(rig: &super::Rig) -> Value {
    rig.exec.get(&DocPath::root(), &Grants::person()).unwrap().unwrap()
}

#[test]
fn undoing_a_session_then_undoing_that_step_restores_the_document() {
    let config = Config { cases: 128, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    runner
        .run(&prop::collection::vec(step(), 1..12), |steps| {
            let rig = rig(json!({"a": 0, "b": 0, "c": 0, "d": 0, "e": 0}));
            let mut touched = Vec::new();
            for step in &steps {
                let req = match step {
                    Step::Agent(key, value) => {
                        touched.push(*key);
                        agent_request("test.set", json!({"path": key, "value": value}))
                    }
                    Step::Person(key, value) => request("test.set", json!({"path": key, "value": value})),
                };
                rig.exec.run(req).map_err(|e| TestCaseError::fail(e.to_string()))?;
            }
            let before = document(&rig);
            let undone = rig.exec.undo_session(SessionId(1), &person()).unwrap();
            let after = document(&rig);
            for key in ["a", "b", "c", "d", "e"] {
                if !touched.contains(&key) {
                    prop_assert_eq!(&before[key], &after[key], "{} was never the agent's", key);
                }
            }
            if undone.step.is_some() {
                prop_assert!(rig.undo().unwrap().is_some());
                prop_assert_eq!(document(&rig), before.clone());
                prop_assert!(rig.redo().unwrap().is_some());
                prop_assert_eq!(document(&rig), after);
            } else {
                prop_assert_eq!(after, before);
            }
            prop_assert!(rig.exec.session_of(&agent()).is_some());
            Ok(())
        })
        .unwrap();
}
