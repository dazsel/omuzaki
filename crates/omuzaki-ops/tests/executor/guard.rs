//! The stale-write guard (#356, design `agent-sessions.md` §5.1): an agent's edit that carries `base` is refused when
//! another actor changed one of its paths (or a path above or below one) after that version. Persons, and requests
//! without `base`, are never guarded.

use omuzaki_doc_api::DocVersion;
use omuzaki_ops_api::{Actor, ClashKind, ErrorCode, ErrorDetail, Event, Mode, Request, Runner};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::{Value, json};

use super::{agent, agent_request, drain, path, person, request, rig, second_agent};

fn face() -> Value {
    json!({"face": {"blend": 0, "eyes": 0}, "hair": 0})
}

fn based(actor: &Actor, at: &str, value: i64, base: DocVersion) -> Request {
    Request { actor: actor.clone(), base: Some(base), ..agent_request("test.set", json!({"path": at, "value": value})) }
}

fn version(rig: &super::Rig) -> DocVersion {
    rig.log().last().map_or(DocVersion(0), |r| r.version)
}

#[test]
fn an_edit_planned_before_someone_elses_change_is_refused_naming_them() {
    let rig = rig(face());
    let read = version(&rig);
    let changed = rig.set("face/blend", json!(5)).applied.unwrap().version;
    let rx = rig.subscribe();

    let e = rig.exec.run(based(&agent(), "face/blend", 1, read)).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert_eq!(e.detail.as_deref(), Some(&ErrorDetail::Changed { path: path("face/blend"), by: person(), version: changed }));
    assert!(e.hint.as_deref().is_some_and(|h| h.contains("re-read face/blend")), "{e:?}");
    assert_eq!(rig.at("face/blend"), Some(json!(5)));
    let clashes = drain(&rx).into_iter().filter(|e| matches!(e, Event::Clash { kind: ClashKind::Stale, .. })).count();
    assert_eq!(clashes, 1);
    let dry = Request { mode: Mode::DryRun, ..based(&agent(), "face/blend", 1, read) };
    assert_eq!(rig.exec.run(dry).unwrap_err().code, ErrorCode::Conflict, "a dry run is guarded too");
    assert_eq!(drain(&rx).len(), 0, "but clashes only when applied");

    rig.exec.run(based(&agent(), "face/blend", 1, changed)).unwrap();
    rig.exec.run(based(&agent(), "hair", 1, read)).unwrap();
}

#[test]
fn own_changes_pass_and_parents_and_undos_by_others_count() {
    let rig = rig(face());
    let read = version(&rig);
    rig.agent_set("face/blend", json!(1)); // the agent's own change after `read`
    rig.exec.run(based(&agent(), "face/blend", 2, read)).unwrap();

    let read = version(&rig);
    rig.set("face", json!({"blend": 9, "eyes": 9})); // a parent, by the person
    let e = rig.exec.run(based(&agent(), "face/blend", 3, read)).unwrap_err();
    assert!(matches!(e.detail.as_deref(), Some(ErrorDetail::Changed { path, .. }) if *path == super::path("face")), "{e:?}");

    let read = version(&rig);
    rig.undo().unwrap(); // the person's undo puts `face` back: a change too
    assert_eq!(rig.exec.run(based(&agent(), "face/eyes", 3, read)).unwrap_err().code, ErrorCode::Conflict);
}

#[test]
fn a_persons_request_is_never_refused_by_the_guard() {
    let config = Config { cases: 64, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    let step = (0..3_usize, prop::sample::select(vec!["face/blend", "face/eyes", "hair"]), 0..50_i64);
    runner
        .run(&prop::collection::vec(step, 1..30), |steps| {
            let rig = rig(face());
            for (who, at, value) in steps {
                let base = DocVersion(u64::try_from(value % 5).unwrap_or(0));
                let req = match who {
                    0 => Request { base: Some(base), ..request("test.set", json!({"path": at, "value": value})) },
                    1 => based(&agent(), at, value, base),
                    _ => based(&second_agent(), at, value, base),
                };
                let result = rig.exec.run(req);
                if who == 0 {
                    prop_assert!(result.is_ok(), "the person was refused: {:?}", result);
                } else if let Err(e) = result {
                    // An agent is refused only for someone else's change, never for its own.
                    let me = if who == 1 { agent() } else { second_agent() };
                    let others = matches!(e.detail.as_deref(), Some(ErrorDetail::Changed { by, .. }) if *by != me);
                    prop_assert!(others, "{:?}", e);
                }
            }
            Ok::<(), TestCaseError>(())
        })
        .unwrap();
}
