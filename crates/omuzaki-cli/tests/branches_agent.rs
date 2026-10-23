//! An agent on its own branch, end to end (`docs/design/branches.md` 8 and 12 items 3 and 5): the built `omuzaki mcp`
//! bridges an agent over stdio into a studio that holds the project (this test plays the studio and its person). The
//! agent makes a branch, moves its session onto it and turns off three of a kit character's parts while the person
//! renames the character on main. It marks a point ready; the person merges two of the three (a pick), then the third
//! (a full merge), undoes that and merges again (the change is offered again), and undo and redo hold. An agent's
//! merge into main waits for the person's yes. Undoing the agent's session on its branch reverts only its steps there,
//! leaving the person's step on the branch and everything merged into main.

// `CARGO_BIN_EXE_omuzaki-fixture` exists only when the fixture is built (`test-fixtures` feature, see Cargo.toml).
#![cfg(feature = "test-fixtures")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::time::Instant;

use common::mcp::{LIMIT, Mcp};
use common::new_project;
use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc_api::DocPath;
use omuzaki_history_api::branch::{BranchName, Branches};
use omuzaki_ops_api::{Actor, Decision, Grants, Mode, Need, Request, SessionId};
use serde_json::{Value, json};

const OMUZAKI: &str = env!("CARGO_BIN_EXE_omuzaki");

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

/// Whether `part` of the character is enabled, on the studio's folder branch.
fn enabled(studio: &AppCore, part: &str) -> bool {
    let at = DocPath::parse(&format!("characters/mia/parts/{part}/enabled")).unwrap();
    studio.get(&at) != Some(json!(false))
}

fn disable(part: &str) -> Value {
    json!({"character": "mia", "part": part, "enabled": false})
}

#[test]
fn an_agent_works_on_its_own_branch_and_the_person_picks_merges_and_undoes() {
    omuzaki_cli::link();
    let (project, data) = (new_project(), tempfile::tempdir().unwrap());
    let studio = AppCore::open(project.path(), AppOptions::studio(person())).unwrap();
    studio.run("character.create", json!({"id": "mia", "name": "Mia", "template": "soft-sd"})).unwrap();
    let parts = studio.get(&DocPath::parse("characters/mia/parts").unwrap()).unwrap();
    // The parts nothing else needs, which can each be turned off on their own.
    let parts: Vec<String> = parts
        .as_object()
        .unwrap()
        .keys()
        .filter(|part| studio.runner().run(studio.request("part.set_enabled", disable(part), Mode::DryRun)).is_ok())
        .cloned()
        .collect();
    assert!(parts.len() >= 4, "the kit character has parts enough: {parts:?}");
    let [a, b, c, f] = std::array::from_fn(|i| parts[i].clone());
    let name = || studio.get(&DocPath::parse("characters/mia/name").unwrap());
    studio.serve_http(omuzaki_mcp::HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap())).unwrap();

    // The agent connects through the real binary and the person pairs it.
    let data_dir = data.path().to_str().unwrap();
    let env = [("OMUZAKI_DATA_DIR", data_dir), ("OMUZAKI_CONFIG_DIR", data_dir)];
    let mut agent = Mcp::start_with_env(OMUZAKI, project.path(), &env);
    agent.handshake();
    let sessions = studio.sessions();
    let deadline = Instant::now() + LIMIT;
    while sessions.approvals().is_empty() {
        assert!(Instant::now() < deadline, "no pairing card");
        std::thread::yield_now();
    }
    let card = sessions.approvals().remove(0);
    sessions.resolve(card.id, Decision::Approve, &person()).unwrap();

    // Its own branch, and three parts changed there.
    agent.call("branch_create", &json!({"name": "agent/look", "focus": ["characters/mia"]}));
    let moved = agent.call("branch_work", &json!({"branch": "agent/look"}));

    let on_branch = SessionId(moved["session"].as_u64().expect("the session on the branch"));
    for part in [&a, &b, &c] {
        agent.call("part_set_enabled", &disable(part));
    }
    let look = studio.branches().by_name(&BranchName::new("agent/look").unwrap()).unwrap();

    // Meanwhile the person edits main, and never sees the agent's edits there.
    studio.run("character.rename", json!({"character": "mia", "name": "Mia Two"})).unwrap();
    assert!(enabled(&studio, &a) && enabled(&studio, &b) && enabled(&studio, &c), "the agent's edits are its branch's");
    let ready = agent.call("branch_checkpoint", &json!({"label": "look A", "ready": true}));
    assert_eq!(ready["ready"], json!(true), "{ready}");

    // A pick: two of the three parts.
    let unit = |part: &str| format!("characters/mia/parts/{part}");
    studio.run("branch.merge", json!({"from": "agent/look", "exclude": [unit(&c)]})).unwrap();
    assert!(!enabled(&studio, &a) && !enabled(&studio, &b), "the picked parts arrive");
    assert!(enabled(&studio, &c), "the part left out stays as it was");
    assert_eq!(name(), Some(json!("Mia Two")), "the person's own edit stays");

    // The full merge brings the third, and undo and redo take it back and forth.
    studio.run("branch.merge", json!({"from": "agent/look"})).unwrap();
    assert!(!enabled(&studio, &c), "the part left out before is offered again and arrives");
    assert!(studio.undo().unwrap().is_some());
    assert!(enabled(&studio, &c) && !enabled(&studio, &a), "undo takes back the second merge only");
    // Merged again after the undo, the branch's change is offered again (design 12 item 3), and undo and redo hold.
    studio.run("branch.merge", json!({"from": "agent/look"})).unwrap();
    assert!(!enabled(&studio, &c), "an undone merge doesn't count: the part arrives again");
    assert!(studio.undo().unwrap().is_some());
    assert!(enabled(&studio, &c));
    assert!(studio.redo().unwrap().is_some());
    assert!(!enabled(&studio, &c));

    // An agent's merge into main waits for the person.
    agent.call("part_set_enabled", &disable(&f));
    // A merge takes the branch's newest point, so the agent records one.
    agent.call("branch_checkpoint", &json!({"label": "look B"}));
    agent.call("branch_work", &json!({"branch": "main"}));
    let waiting = agent.result("branch_merge", &json!({"from": "agent/look"}));
    assert_eq!(waiting["structuredContent"]["code"], json!("waiting"), "{}", Mcp::text(&waiting));
    let card = sessions.approvals().into_iter().next().expect("the person is asked");
    assert!(!matches!(card.need, Need::Pairing { .. }), "{:?}", card.need);
    assert!(
        Mcp::text(&waiting).contains(&format!("Merge agent/look into main (mia: part {f})")),
        "{}",
        Mcp::text(&waiting)
    );
    assert!(enabled(&studio, &f), "nothing lands before the person says yes");
    sessions.resolve(card.id, Decision::Approve, &person()).unwrap();
    let merged = agent.result("approval_wait", &json!({"id": card.id.0}));
    assert_ne!(merged["isError"], json!(true), "{}", Mcp::text(&merged));
    assert!(!enabled(&studio, &f), "after the yes, the agent's merge lands on main");

    // Undoing the agent's session on its branch reverts only its steps there: not the person's, not main.
    let lane = studio.branches().lane(&look.id).unwrap();
    let request = Request {
        op: "character.rename".to_owned(),
        args: json!({"character": "mia", "name": "Mia on the branch"}),
        mode: Mode::Apply,
        actor: person(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    lane.runner.run(request).unwrap();
    let undone = sessions.undo_session(on_branch, &person()).unwrap();
    assert_eq!(undone.reverted.len(), 4, "the agent's four edits on its branch: {undone:?}");
    let on_look = |part: &str| {
        let at = DocPath::parse(&format!("characters/mia/parts/{part}/enabled")).unwrap();
        lane.runner.get(&at, &Grants::person()).unwrap() != Some(json!(false))
    };
    assert!([&a, &b, &c, &f].iter().all(|p| on_look(p)), "the agent's edits are reverted on its branch");
    let named = lane.runner.get(&DocPath::parse("characters/mia/name").unwrap(), &Grants::person()).unwrap();
    assert_eq!(named, Some(json!("Mia on the branch")), "the person's edit on the branch stays");
    assert!([&a, &b, &c, &f].iter().all(|p| !enabled(&studio, p)), "what was merged into main stays");

    drop(lane);
    let (code, stderr) = agent.close();
    assert_eq!(code, Some(0), "{stderr}");
    studio.close().unwrap();
}
