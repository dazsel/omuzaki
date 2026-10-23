//! Branches in the app core (`docs/design/branches.md` 5, 8 and 9): lanes edit apart from the folder, checkpoints
//! record committed state only, a switch rewrites the folder and survives a crash at every step, restore keeps the
//! abandoned future, and what an agent may do is what design 8.2 lists.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::path::Path;
use std::time::Duration;

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc::Project;
use omuzaki_doc::branch::{FolderMarker, FolderState};
use omuzaki_doc_api::{DocPath, DocRead, Patch};
use omuzaki_history_api::JournalSink;
use omuzaki_history_api::branch::{
    BranchError, BranchId, BranchInfo, BranchName, Branches, Lane, LaneState, NewBranch, PointKind, ReviewDecision,
    ReviewState,
};
use omuzaki_history_api::state::{BranchStore, StateRef};
use omuzaki_ops_api::{
    Actor, Budget, Grants, Limits, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, SessionInfo,
    SessionSpec, Transport, TxId, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Sets the project's note: the one edit these tests need.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetNote {
    note: String,
}

impl Op for SetNote {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.set_note",
            summary: "Set the project's note.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = note_path();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.note)))))
    }
}
register_op!(SetNote);

fn note_path() -> DocPath {
    DocPath::parse("project/note").unwrap()
}

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

fn options() -> AppOptions {
    AppOptions::studio(person())
}

fn name(text: &str) -> BranchName {
    BranchName::new(text).unwrap()
}

fn request(actor: &Actor, note: &str, tx: Option<TxId>) -> Request {
    Request {
        op: "test.set_note".to_owned(),
        args: json!({ "note": note }),
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx,
        base: None,
    }
}

fn set(runner: &dyn Runner, actor: &Actor, note: &str) {
    runner.run(request(actor, note, None)).unwrap();
}

fn note_of(runner: &dyn Runner) -> Option<Value> {
    runner.get(&note_path(), &Grants::person()).unwrap()
}

fn note_on_disk(dir: &Path) -> Option<Value> {
    Project::open(dir).unwrap().get(&note_path()).cloned()
}

fn note_at(core: &AppCore, info: &BranchInfo) -> Option<Value> {
    core.branches().store().tree(&info.head).unwrap().pointer("/project/note").cloned()
}

fn branch(core: &AppCore, text: &str) -> BranchInfo {
    core.branches().by_name(&name(text)).unwrap()
}

fn create(core: &AppCore, text: &str, by: &Actor) -> BranchInfo {
    core.branches().create(NewBranch::new(name(text), BranchId::main()), by).unwrap()
}

fn lane(core: &AppCore, info: &BranchInfo) -> Lane {
    core.branches().lane(&info.id).unwrap()
}

fn agent_session(lane: &Lane, client: &str) -> SessionInfo {
    lane.sessions
        .open(SessionSpec {
            transport: Transport::Stdio,
            client: client.into(),
            model: None,
            on_behalf_of: person(),
            grants: Grants::person(),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        })
        .unwrap()
}

#[test]
fn a_branch_s_lane_edits_apart_from_the_folder_and_its_checkpoints_advance_its_head() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(core.runner().as_ref(), &person(), "main 1");
    assert!(core.branches().list().is_empty(), "nothing is branched until something branches");

    // Creating the first branch makes main's ref too, both at main's committed state.
    let oval = create(&core, "aria/oval", &person());
    let main = branch(&core, "main");
    assert_eq!((main.seq, oval.from), (1, Some(main.head)));
    assert_eq!(note_at(&core, &oval), Some(json!("main 1")));
    assert!(oval.inherits.includes(&BranchId::main(), &omuzaki_history_api::branch::Version::new(1, main.head)));

    // The branch's lane is its own executor: its edit doesn't reach the folder, nor the folder's reach it.
    let side = lane(&core, &oval);
    set(side.runner.as_ref(), &person(), "oval 1");
    set(core.runner().as_ref(), &person(), "main 2");
    assert_eq!(note_of(side.runner.as_ref()), Some(json!("oval 1")));
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("main 2")));
    let listed = core.branches().list();
    assert_eq!(listed.iter().map(|b| b.name.to_string()).collect::<Vec<_>>(), ["main", "aria/oval"]);
    assert_eq!(listed[0].lane, LaneState::open(true, 0));
    assert_eq!(listed[1].lane, LaneState::open(false, 0));

    // A checkpoint is a point of the lane's state, the branch's own first (its seqs count its own points); with
    // nothing changed since, it is the head again.
    let first = core.branches().checkpoint(&oval.id, "first look", None, &person()).unwrap();
    let oval = branch(&core, "aria/oval");
    assert_eq!((oval.head, oval.seq), (first, 1));
    let point = core.branches().store().point(&first).unwrap();
    assert_eq!((point.branch.clone(), point.parents.clone()), (oval.id.clone(), vec![main.head]));
    assert_eq!(note_at(&core, &oval), Some(json!("oval 1")));
    assert_eq!(core.branches().checkpoint(&oval.id, "again", None, &person()).unwrap(), first);
    assert_eq!(core.branches().store().point(&first).unwrap().kind, PointKind::Checkpoint);

    // An idle side lane closes (with a handle held it doesn't), and its next open starts from its own journal.
    core.branches().close_idle(Duration::ZERO);
    assert!(core.branches().open_lane(&oval.id).is_some(), "a held lane stays open");
    drop(side);
    core.branches().close_idle(Duration::ZERO);
    assert!(core.branches().open_lane(&oval.id).is_none(), "an idle lane closes");
    assert_eq!(branch(&core, "aria/oval").lane, LaneState::Closed);
    let side = lane(&core, &oval);
    assert_eq!(note_of(side.runner.as_ref()), Some(json!("oval 1")));
    drop(side);
    core.close().unwrap();
    assert_eq!(note_on_disk(tmp.path()), Some(json!("main 2")), "only the folder lane saves the files");
}

#[test]
fn a_checkpoint_records_committed_state_and_leaves_an_open_transaction_out() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(core.runner().as_ref(), &person(), "committed");
    let runner = core.runner();
    let tx = runner.begin("a long edit", &person());
    runner.run(request(&person(), "half done", Some(tx))).unwrap();
    assert_eq!(note_of(runner.as_ref()), Some(json!("half done")));

    let point = core.branches().checkpoint(&BranchId::main(), "mid edit", None, &person()).unwrap();
    let tree = core.branches().store().tree(&point).unwrap();
    assert_eq!(tree.pointer("/project/note"), Some(&json!("committed")), "the open transaction is left out");
    assert!(core.branches().store().point(&point).unwrap().history.is_none(), "no history while one is open");

    runner.commit(tx, &person()).unwrap();
    let after = core.branches().checkpoint(&BranchId::main(), "done", None, &person()).unwrap();
    assert_ne!(after, point);
    let tree = core.branches().store().tree(&after).unwrap();
    assert_eq!(tree.pointer("/project/note"), Some(&json!("half done")));
    assert!(core.branches().store().point(&after).unwrap().history.is_some());
    core.close().unwrap();
}

#[test]
fn a_switch_rewrites_the_folder_and_the_next_open_is_on_that_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(core.runner().as_ref(), &person(), "on main");
    let oval = create(&core, "aria/oval", &person());
    let side = lane(&core, &oval);
    set(side.runner.as_ref(), &person(), "on oval");
    drop(side);

    let held = core.runner();
    core.branches().switch(&oval.id, &person()).unwrap();
    // The calling thread returns once the folder is rebound; the files are written off it (#2323).
    assert_eq!(core.branches().folder(), oval.id);
    assert_eq!(note_of(held.as_ref()), Some(json!("on oval")), "the folder branch is the new one at once");
    core.branches().settled().unwrap();
    assert_eq!(note_on_disk(tmp.path()), Some(json!("on oval")), "the folder holds the branch");
    assert_eq!(note_of(held.as_ref()), Some(json!("on oval")), "a held runner follows the folder");
    assert!(core.branches().open_lane(&BranchId::main()).is_none(), "main's lane closed with nothing on it");
    assert_eq!(FolderMarker::read(tmp.path()).unwrap().branch, oval.id);
    // The person's edit lands on the folder branch, and main's head kept where it was.
    set(held.as_ref(), &person(), "oval 2");
    assert_eq!(note_at(&core, &branch(&core, "main")), Some(json!("on main")));
    core.close().unwrap();
    assert_eq!(note_on_disk(tmp.path()), Some(json!("oval 2")));

    let core = AppCore::open(tmp.path(), options()).unwrap();
    assert_eq!(core.branches().folder(), oval.id);
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("oval 2")));
    assert!(core.undo().unwrap().is_some(), "undo reaches across the restart on the branch");
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("on oval")));
    core.branches().switch(&BranchId::main(), &person()).unwrap();
    core.branches().settled().unwrap();
    assert_eq!(note_on_disk(tmp.path()), Some(json!("on main")));
    core.close().unwrap();
}

#[test]
fn a_switch_refuses_an_open_transaction_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let oval = create(&core, "aria/oval", &person());
    let tx = core.runner().begin("hair", &person());
    core.runner().run(request(&person(), "mid", Some(tx))).unwrap();
    let refused = core.branches().switch(&oval.id, &person()).unwrap_err();
    assert!(matches!(refused, BranchError::Busy(_)), "{refused}");
    assert_eq!(core.branches().folder(), BranchId::main());
    assert_eq!(FolderMarker::read(tmp.path()).unwrap().state, FolderState::Ok);
    core.runner().rollback(tx, &person()).unwrap();
    core.close().unwrap();
}

#[test]
fn a_switch_cut_short_at_any_step_is_finished_by_the_next_open() {
    for step in ["switch:marked", "switch:old-point", "switch:rewritten"] {
        let tmp = tempfile::tempdir().unwrap();
        let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
        set(core.runner().as_ref(), &person(), "on main");
        let oval = create(&core, "aria/oval", &person());
        set(lane(&core, &oval).runner.as_ref(), &person(), "on oval");
        core.close().unwrap();

        let crashing =
            options().with_switch_failpoint(move |at| if at == step { Err(format!("cut at {at}")) } else { Ok(()) });
        let core = AppCore::open(tmp.path(), crashing).unwrap();
        // A cut on the calling thread fails the switch; one on the switcher thread comes back from `settled`.
        let cut = core.branches().switch(&oval.id, &person()).and_then(|_| core.branches().settled()).unwrap_err();
        assert!(cut.to_string().contains(step), "{step}: {cut}");
        core.abandon();
        assert_eq!(FolderMarker::read(tmp.path()).unwrap().state, FolderState::Switching, "{step}");

        let core = AppCore::open(tmp.path(), options()).unwrap();
        assert_eq!(core.branches().folder(), oval.id, "{step}: the switch is finished");
        let marker = FolderMarker::read(tmp.path()).unwrap();
        assert_eq!((marker.state, marker.branch), (FolderState::Ok, oval.id.clone()), "{step}");
        assert_eq!(note_of(core.runner().as_ref()), Some(json!("on oval")), "{step}");
        assert_eq!(note_on_disk(tmp.path()), Some(json!("on oval")), "{step}: the folder was rewritten");
        core.close().unwrap();
    }
}

#[test]
fn restore_keeps_the_abandoned_future_on_an_automatic_branch_and_undo_returns() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(core.runner().as_ref(), &person(), "first");
    let first = TxId(core.executor().history_export().next_tx.0 - 1);
    set(core.runner().as_ref(), &person(), "second");

    let restored =
        core.branches().restore(&BranchId::main(), StateRef::tx(first), &person(), &Grants::person()).unwrap();
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("first")));
    let backup = &restored.backup;
    assert!(backup.auto && backup.name.as_str().starts_with("restore/"), "{}", backup.name);
    assert_eq!(note_at(&core, backup), Some(json!("second")), "the abandoned future is kept");
    assert_eq!(core.branches().store().point(&backup.head).unwrap().kind, PointKind::Restore);
    assert!(core.undo().unwrap().is_some());
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("second")));

    // Restoring to now changes nothing and makes no branch.
    let before = core.branches().list().len();
    assert!(core.branches().restore(&BranchId::main(), StateRef::Now, &person(), &Grants::person()).is_err());
    assert_eq!(core.branches().list().len(), before);
    core.close().unwrap();
}

#[test]
fn an_agent_does_what_design_8_2_lists_and_no_more() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let theirs = create(&core, "person/look", &person());
    let folder = core.branches().lane(&BranchId::main()).unwrap();
    let session = agent_session(&folder, "designer");
    let agent = session.actor.clone();
    let stranger = Actor::Agent { id: "nobody#99".into(), model: None };

    // An agent with no session here can't make a branch; one with a session makes branches for itself.
    let refused = core.branches().create(NewBranch::new(name("x/y"), BranchId::main()), &stranger).unwrap_err();
    assert!(matches!(refused, BranchError::Denied(_)), "{refused}");
    let own = create(&core, "designer/oval", &agent);
    assert_eq!(own.owner_session, Some(session.id));

    // Its own branch it may rename, check point, mark ready and delete; the person's it may not rename or delete.
    let own = core.branches().rename(&own.id, name("designer/oval-a"), None, &agent).unwrap();
    assert!(matches!(core.branches().rename(&theirs.id, name("z"), None, &agent), Err(BranchError::Denied(_))));
    assert!(matches!(core.branches().delete(&theirs.id, &agent), Err(BranchError::Denied(_))));
    let ready = core.branches().checkpoint(&own.id, "version A", Some("ready".into()), &agent).unwrap();
    let reviewed = branch(&core, "designer/oval-a");
    let review = reviewed.review.clone().unwrap();
    assert_eq!((review.state, review.point), (ReviewState::Ready, ready));

    // Switching the folder and deciding a review are the person's.
    assert!(matches!(core.branches().switch(&own.id, &agent), Err(BranchError::Denied(_))));
    let decision = ReviewDecision::send_back("rounder");
    assert!(matches!(core.branches().review(&own.id, decision.clone(), &agent), Err(BranchError::Denied(_))));
    let sent = core.branches().review(&own.id, decision, &person()).unwrap();
    assert_eq!(sent.review.map(|r| r.state), Some(ReviewState::SentBack));

    // Session ids are unique across lanes.
    let side = lane(&core, &own);
    let other = agent_session(&side, "second");
    assert_ne!(other.id, session.id);
    assert_eq!(core.sessions().sessions().len(), 2, "the person's registry lists every lane's sessions");
    drop(side);
    core.close().unwrap();
}

#[test]
fn the_lane_limit_closes_an_idle_lane_first_and_refuses_when_every_lane_is_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options().with_max_lanes(2)).unwrap();
    let a = create(&core, "a", &person());
    let b = create(&core, "b", &person());
    let held = lane(&core, &a);
    assert!(matches!(core.branches().lane(&b.id), Err(BranchError::LaneLimit)), "a held lane is busy");
    drop(held);
    let opened = lane(&core, &b);
    assert!(core.branches().open_lane(&a.id).is_none(), "the idle lane closed to make room");
    drop(opened);
    core.close().unwrap();
}

#[test]
fn a_side_lane_left_open_by_a_crash_is_recovered_at_the_next_open() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let oval = create(&core, "aria/oval", &person());
    let side = lane(&core, &oval);
    set(side.runner.as_ref(), &person(), "kept");
    let journal = core.branches().open_lane(&oval.id).unwrap().journal.clone();
    journal.sync(Duration::from_secs(10)).unwrap();
    drop((side, journal));
    core.abandon();

    let core = AppCore::open(tmp.path(), options()).unwrap();
    let recovered = core.branches().recovered();
    assert_eq!(recovered.len(), 1, "{recovered:?}");
    assert!(recovered[0].1.contains("aria/oval"), "{recovered:?}");
    assert!(core.branches().open_lane(&oval.id).is_none(), "recovered, then closed again");
    let oval = branch(&core, "aria/oval");
    assert_eq!(note_at(&core, &oval), Some(json!("kept")), "the recovered state is the branch's close point");
    assert_eq!(note_of(lane(&core, &oval).runner.as_ref()), Some(json!("kept")));
    core.close().unwrap();
}
