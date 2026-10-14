//! Undo and redo: per-actor stacks of committed transactions, conflicts that drop the step, permissions.

use omuzaki_ops_api::{ErrorCode, Grants, Request, Runner, Scope};
use serde_json::json;

use super::{agent, person, request, rig};

#[test]
fn nothing_to_undo_or_redo_is_none() {
    let rig = rig(json!({}));
    assert_eq!(rig.undo().unwrap(), None);
    assert_eq!(rig.redo().unwrap(), None);
}

#[test]
fn undo_then_redo_round_trips_through_labels() {
    let rig = rig(json!({"a": 1}));
    rig.set("a", json!(2));
    rig.set("a", json!(3));

    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("a"), Some(json!(2)));
    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("a"), Some(json!(1)));
    assert_eq!(rig.undo().unwrap(), None);

    assert_eq!(rig.redo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("a"), Some(json!(2)));
    assert_eq!(rig.redo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("a"), Some(json!(3)));
    assert_eq!(rig.redo().unwrap(), None);
}

#[test]
fn undo_still_works_after_a_later_edit_to_another_path() {
    let rig = rig(json!({"a": 1, "b": 1}));
    rig.set("a", json!(2));
    rig.agent_set("b", json!(2)); // a later edit, by someone else, elsewhere

    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(1)), Some(json!(2))), "only the person's step reverted");

    // The same actor's own later edit, still in an open transaction, is not on the stack yet either.
    rig.set("a", json!(5));
    let tx = rig.exec.begin("later", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "b", "value": 9})) }).unwrap();
    rig.undo().unwrap();
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(1)), Some(json!(9))));
}

#[test]
fn undo_is_per_actor() {
    let rig = rig(json!({"a": 1, "b": 1}));
    rig.set("a", json!(2)); // the person
    rig.agent_set("b", json!(2)); // the agent, later

    // The person's undo skips past the agent's later step; the agent's undo has its own.
    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(1)), Some(json!(2))));
    assert_eq!(rig.undo().unwrap(), None, "the person has nothing else; the agent's step is not theirs");
    assert_eq!(rig.at("b"), Some(json!(2)));
    assert_eq!(rig.agent_undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("b"), Some(json!(1)));
}

#[test]
fn an_agent_cannot_undo_a_persons_step() {
    let rig = rig(json!({"x": 0}));
    rig.set("x", json!(1));
    assert_eq!(rig.agent_undo().unwrap(), None);
    assert_eq!(rig.at("x"), Some(json!(1)));
    assert_eq!(rig.agent_redo().unwrap(), None);
    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("x"), Some(json!(0)));
}

#[test]
fn a_commit_clears_only_its_own_actors_redo() {
    let rig = rig(json!({}));
    rig.set("p", json!(1));
    rig.agent_set("q", json!(1));
    rig.undo().unwrap();
    rig.agent_undo().unwrap();

    // The agent commits something new: its redo ends, the person's survives.
    rig.agent_set("r", json!(1));
    assert_eq!(rig.agent_redo().unwrap(), None);
    assert_eq!(rig.redo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("p"), Some(json!(1)));
}

#[test]
fn a_new_commit_clears_redo_for_single_ops_and_transactions() {
    let rig = rig(json!({}));
    rig.set("a", json!(1));
    rig.undo().unwrap();
    rig.set("b", json!(2));
    assert_eq!(rig.redo().unwrap(), None, "redo history ends at a new edit");
    assert_eq!((rig.at("a"), rig.at("b")), (None, Some(json!(2))));

    rig.undo().unwrap();
    let tx = rig.exec.begin("t", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "c", "value": 3})) }).unwrap();
    rig.exec.commit(tx, &person()).unwrap();
    assert_eq!(rig.redo().unwrap(), None);
}

#[test]
fn a_transaction_is_undone_by_the_actor_that_committed_it() {
    let rig = rig(json!({"a": 1, "b": 1}));
    let tx = rig.exec.begin("agent's pair", &agent());
    for (path, value) in [("a", 2), ("b", 2)] {
        let req = Request { tx: Some(tx), ..super::agent_request("test.set", json!({"path": path, "value": value})) };
        rig.exec.run(req).unwrap();
    }
    rig.exec.commit(tx, &agent()).unwrap();
    assert_eq!(rig.undo().unwrap(), None, "not the person's");
    assert_eq!(rig.agent_undo().unwrap().as_deref(), Some("agent's pair"));
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(1)), Some(json!(1))));
}

#[test]
fn a_conflicting_undo_names_the_path_and_who_changed_it_and_drops_the_step() {
    let rig = rig(json!({"a": 0, "x": 0}));
    rig.agent_set("a", json!(1)); // step 1
    rig.agent_set("x", json!(1)); // step 2
    rig.set("x", json!(5)); // the person edits what step 2 touched

    let e = rig.agent_undo().unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(e.message.contains("test.set") && e.message.contains(" x ") && e.message.contains("person alice"), "{e}");
    assert!(e.hint.as_deref().unwrap().contains("dropped"), "{e}");
    assert_eq!(rig.at("x"), Some(json!(5)), "a refused undo changes nothing");

    // The step is gone, so history is not wedged: the next undo reaches the step before it.
    assert_eq!(rig.agent_undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("a"), Some(json!(0)));
    assert_eq!(rig.agent_undo().unwrap(), None);
    // The person's own step is untouched by all of this.
    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("x"), Some(json!(1)), "undoing the person's edit restores what the agent's dropped step left");
}

#[test]
fn a_conflict_caused_by_the_actors_own_open_transaction_is_reported_the_same_way() {
    let rig = rig(json!({"a": 1}));
    rig.set("a", json!(2));
    let tx = rig.exec.begin("later", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 3})) }).unwrap();

    let e = rig.undo().unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(e.message.contains("person alice"), "{e}");
    assert_eq!(rig.at("a"), Some(json!(3)));
    assert_eq!(rig.undo().unwrap(), None, "the conflicting step was dropped");
}

#[test]
fn steps_that_change_nothing_do_not_clear_redo() {
    let rig = rig(json!({"a": 1}));
    rig.set("a", json!(2));
    rig.undo().unwrap();

    rig.run("test.get", json!({"path": "a"})).unwrap(); // a query
    rig.set("a", json!(1)); // an edit that sets what is already there: no effective change
    let tx = rig.exec.begin("empty", &person());
    rig.exec.commit(tx, &person()).unwrap();

    assert_eq!(rig.redo().unwrap().as_deref(), Some("test.set"));
    assert_eq!(rig.at("a"), Some(json!(2)));
}

#[test]
fn undo_and_redo_need_the_scopes_of_the_ops_they_revert_and_a_refusal_keeps_the_step() {
    let rig = rig(json!({"a": 1, "p": 1}));
    // One transaction that edits a character and publishes.
    let tx = rig.exec.begin("edit and publish", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 2})) }).unwrap();
    rig.exec.run(Request { tx: Some(tx), ..request("test.publish", json!({"path": "p", "value": 2})) }).unwrap();
    rig.exec.commit(tx, &person()).unwrap();

    let limited = Grants::agent_default(); // no publish
    let e = rig.exec.undo(&person(), &limited).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(e.hint.as_deref().unwrap().contains("publish"), "{e}");
    assert!(e.message.contains("person alice") && e.message.contains("edit and publish"), "{e}");
    assert_eq!((rig.at("a"), rig.at("p")), (Some(json!(2)), Some(json!(2))), "a refused undo changes nothing");

    let with_publish = limited.clone().with(Scope::Publish);
    assert_eq!(rig.exec.undo(&person(), &with_publish).unwrap().as_deref(), Some("edit and publish"));
    assert_eq!((rig.at("a"), rig.at("p")), (Some(json!(1)), Some(json!(1))));

    assert_eq!(rig.exec.redo(&person(), &limited).unwrap_err().code, ErrorCode::Denied);
    assert_eq!(rig.exec.redo(&person(), &with_publish).unwrap().as_deref(), Some("edit and publish"));
}

#[test]
fn queries_in_a_transaction_do_not_add_scopes_to_the_undo_step() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("t", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 2})) }).unwrap();
    rig.exec.run(Request { tx: Some(tx), ..request("test.get", json!({"path": "a"})) }).unwrap();
    rig.exec.commit(tx, &person()).unwrap();
    // `edit:character` is all this step needs; the query's `read` is not part of reverting it.
    let only_edit = Grants::of([Scope::EditCharacter]);
    assert!(rig.exec.undo(&person(), &only_edit).unwrap().is_some());
}

#[test]
fn a_conflicting_redo_names_the_culprit_and_drops_the_step() {
    let rig = rig(json!({"a": 1}));
    rig.set("a", json!(2));
    rig.undo().unwrap();
    rig.agent_set("a", json!(7)); // someone else edits the same path between undo and redo

    let e = rig.redo().unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(e.message.contains("redo") && e.message.contains("agent claude-1"), "{e}");
    assert!(e.hint.as_deref().unwrap().contains("dropped"), "{e}");
    assert_eq!(rig.at("a"), Some(json!(7)));
    assert_eq!(rig.redo().unwrap(), None, "the conflicting step was dropped from the redo history");
}

#[test]
fn history_never_wedges_however_the_conflicts_fall() {
    // Every step of the person's conflicts with an agent edit made afterwards. Each undo reports one conflict and
    // drops one step; the loop must end with `None` after exactly as many calls as there were steps.
    let rig = rig(json!({"a": 0, "b": 0, "c": 0}));
    for key in ["a", "b", "c"] {
        rig.set(key, json!(1));
    }
    for key in ["a", "b", "c"] {
        rig.agent_set(key, json!(2));
    }
    let mut calls = 0;
    loop {
        calls += 1;
        assert!(calls <= 4, "undo is wedged");
        match rig.undo() {
            Ok(None) => break,
            Ok(Some(_)) => panic!("nothing can be undone here"),
            Err(e) => assert_eq!(e.code, ErrorCode::Conflict),
        }
    }
    assert_eq!(calls, 4);
}
