//! The acceptance lists of the branch host issues that `branches.rs` doesn't already cover (`docs/design/branches.md`
//! 5, 6, 8 and 9): undo on each side of a fork, forks from old states, reviews end to end, `contains` following the
//! merges record, lanes reopening with their undo stacks, ids across restarts, restores that fail or come in quick
//! succession, many threads on many lanes at once, and what a checkpoint, a switch and a merge preview cost on a
//! 500-file project.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_history_api::branch::{
    BranchError, BranchEvent, BranchId, BranchInfo, BranchName, Branches, Lane, NewBranch, ReviewDecision, ReviewState,
};
use omuzaki_history_api::state::{BranchStore, StateRef};
use omuzaki_ops_api::{
    Actor, Budget, Grants, Limits, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, SessionInfo,
    SessionSpec, Transport, TxId, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Longest any wait for an event may take: a hang guard, never an ordering.
const WAIT: Duration = Duration::from_secs(30);

/// Sets the project's note.
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

/// Sets one part's index, in the 500-file project of the cost tests.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetIndex {
    part: usize,
    index: u64,
}

impl Op for SetIndex {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.set_index",
            summary: "Set a part's index.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = part_index(self.part);
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.index)))))
    }
}
register_op!(SetIndex);

fn note_path() -> DocPath {
    DocPath::parse("project/note").unwrap()
}

fn part_index(part: usize) -> DocPath {
    DocPath::parse(&format!("characters/c{}/part{part}/index", part % 25)).unwrap()
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

fn set(runner: &dyn Runner, actor: &Actor, note: &str) {
    let request = Request {
        op: "test.set_note".to_owned(),
        args: json!({ "note": note }),
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap();
}

fn note_of(runner: &dyn Runner) -> Option<Value> {
    runner.get(&note_path(), &Grants::person()).unwrap()
}

fn whole(runner: &dyn Runner) -> Value {
    runner.get(&DocPath::root(), &Grants::person()).unwrap().unwrap()
}

fn last_tx(core: &AppCore) -> TxId {
    TxId(core.executor().history_export().next_tx.0 - 1)
}

fn branch(core: &AppCore, text: &str) -> BranchInfo {
    core.branches().by_name(&name(text)).unwrap()
}

fn create(core: &AppCore, text: &str) -> BranchInfo {
    core.branches().create(NewBranch::new(name(text), BranchId::main()), &person()).unwrap()
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

/// The first event `matches` accepts, skipping the others; fails after [`WAIT`].
fn next_event(events: &Receiver<BranchEvent>, matches: impl Fn(&BranchEvent) -> bool) -> BranchEvent {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(event) if matches(&event) => return event,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => panic!("no matching branch event within {WAIT:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("the branch events ended"),
        }
    }
}

/// 499 part files in 25 characters beside `project.toml`: the design's 500-file project.
fn five_hundred_files(dir: &std::path::Path) {
    AppCore::create(dir, "bench", options()).unwrap().close().unwrap();
    for i in 0..499 {
        let folder = dir.join(format!("characters/c{}", i % 25));
        std::fs::create_dir_all(&folder).unwrap();
        let text = format!("kind = \"lock\"\nindex = {i}\ncurve = [0.1, 0.2, 0.3, 0.4]\ncolour = \"#a0b0c0\"\n");
        std::fs::write(folder.join(format!("part{i}.toml")), text).unwrap();
    }
}

/// Ten edits, each to a different part file, on `runner`.
fn ten_changes(runner: &dyn Runner, round: u64) {
    for part in 0..10_u64 {
        let request = Request {
            op: "test.set_index".to_owned(),
            args: json!({ "part": part * 37 % 499, "index": 100_000 + round * 10 + part }),
            mode: Mode::Apply,
            actor: person(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        runner.run(request).unwrap();
    }
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

/// #279: a fork from now has the same undo on each side; a fork from an old state holds exactly it and no history.
#[test]
fn undo_on_each_side_of_a_fork_and_a_fork_from_an_old_state() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(core.runner().as_ref(), &person(), "a");
    let first = last_tx(&core);
    let at_first = whole(core.runner().as_ref());
    set(core.runner().as_ref(), &person(), "b");

    let now = create(&core, "fork/now");
    let side = lane(&core, &now);
    assert!(side.runner.undo(&person(), &Grants::person()).unwrap().is_some(), "the fork carries the undo stack");
    assert!(core.undo().unwrap().is_some());
    assert_eq!(note_of(side.runner.as_ref()), Some(json!("a")));
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("a")));
    assert_eq!(side.runner.redo(&person(), &Grants::person()).unwrap().is_some(), core.redo().unwrap().is_some());
    drop(side);

    let old = core
        .branches()
        .create(NewBranch::new(name("fork/old"), BranchId::main()).at(StateRef::tx(first)), &person())
        .unwrap();
    // #441: the point made from an old transaction's tree loads back equal to that tree.
    assert_eq!(*core.branches().store().tree(&old.head).unwrap(), at_first);
    let side = lane(&core, &old);
    assert_eq!(note_of(side.runner.as_ref()), Some(json!("a")));
    assert!(side.runner.undo(&person(), &Grants::person()).unwrap().is_none(), "an old state starts with no history");
    drop(side);
    core.close().unwrap();
}

/// #279: a ready checkpoint asks for review; merging that point marks it merged; a send-back reaches subscribers with
/// its comment. #441: `contains` follows the live merges record, so an undone merge leaves it.
#[test]
fn reviews_end_to_end_and_contains_follows_the_merges_record() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(core.runner().as_ref(), &person(), "base");
    let events = core.branches().subscribe();
    let oval = create(&core, "aria/oval");
    let side = lane(&core, &oval);
    let agent = agent_session(&side, "designer").actor;
    set(side.runner.as_ref(), &agent, "oval");
    let ready = core.branches().checkpoint(&oval.id, "version A", Some("rounder chin".into()), &agent).unwrap();
    drop(side);
    let asked = next_event(&events, |e| matches!(e, BranchEvent::ReviewRequested { .. }));
    let BranchEvent::ReviewRequested { branch: asked_on, point, .. } = asked else { unreachable!() };
    assert_eq!((asked_on, point), (oval.id.clone(), ready));

    // Two checkpoints with no change between are the same point.
    let main_before = core.branches().checkpoint(&BranchId::main(), "before", None, &person()).unwrap();
    assert_eq!(core.branches().checkpoint(&BranchId::main(), "again", None, &person()).unwrap(), main_before);
    assert!(core.branches().store().point(&main_before).unwrap().contains.get(&oval.id).is_none());

    core.run("branch.merge", json!({ "from": "aria/oval", "at": ready.to_string() })).unwrap();
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("oval")));
    let merged = next_event(&events, |e| matches!(e, BranchEvent::Reviewed { .. }));
    let BranchEvent::Reviewed { branch: reviewed, state, .. } = merged else { unreachable!() };
    assert_eq!((reviewed, state), (oval.id.clone(), ReviewState::Merged));
    assert_eq!(branch(&core, "aria/oval").review.map(|r| r.state), Some(ReviewState::Merged));

    let with = core.branches().checkpoint(&BranchId::main(), "merged", None, &person()).unwrap();
    assert!(core.branches().store().point(&with).unwrap().contains.get(&oval.id).is_some(), "the merge is contained");
    assert!(core.undo().unwrap().is_some());
    let without = core.branches().checkpoint(&BranchId::main(), "undone", None, &person()).unwrap();
    assert!(
        core.branches().store().point(&without).unwrap().contains.get(&oval.id).is_none(),
        "an undone merge leaves contains"
    );

    // A send-back carries its comment to subscribers and onto the branch.
    let side = lane(&core, &oval);
    set(side.runner.as_ref(), &agent, "oval 2");
    core.branches().checkpoint(&oval.id, "version B", Some("again".into()), &agent).unwrap();
    drop(side);
    core.branches().review(&oval.id, ReviewDecision::send_back("make it softer"), &person()).unwrap();
    let back = next_event(&events, |e| matches!(e, BranchEvent::Reviewed { state: ReviewState::SentBack, .. }));
    assert!(matches!(back, BranchEvent::Reviewed { .. }));
    let review = branch(&core, "aria/oval").review.unwrap();
    assert_eq!((review.state, review.note.as_deref()), (ReviewState::SentBack, Some("make it softer")));
    core.close().unwrap();
}

/// #433: an agent working on the old folder branch keeps working in its lane after the person switches away.
#[test]
fn an_agent_on_the_old_branch_keeps_working_after_a_switch() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(core.runner().as_ref(), &person(), "main");
    let oval = create(&core, "aria/oval");
    let main_lane = core.branches().lane(&BranchId::main()).unwrap();
    let agent = agent_session(&main_lane, "helper").actor;

    core.branches().switch(&oval.id, &person()).unwrap();
    assert!(core.branches().open_lane(&BranchId::main()).is_some(), "a lane with a session stays open");
    set(main_lane.runner.as_ref(), &agent, "the agent's");
    assert_eq!(note_of(main_lane.runner.as_ref()), Some(json!("the agent's")));
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("main")), "the folder holds oval, untouched");
    let point = core.branches().checkpoint(&BranchId::main(), "agent", None, &person()).unwrap();
    assert_eq!(core.branches().store().tree(&point).unwrap().pointer("/project/note"), Some(&json!("the agent's")));
    drop(main_lane);
    core.close().unwrap();
}

/// #444: a lane closed for idleness reopens to the same document and undo stack; session ids never repeat across lanes
/// or restarts.
#[test]
fn an_idle_lane_reopens_with_its_undo_stack_and_ids_never_repeat_across_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let oval = create(&core, "aria/oval");
    let side = lane(&core, &oval);
    let first = agent_session(&side, "one").id;
    set(side.runner.as_ref(), &person(), "x");
    set(side.runner.as_ref(), &person(), "y");
    let doc = whole(side.runner.as_ref());
    side.sessions.end(first, omuzaki_ops_api::EndReason::Closed).unwrap();
    drop(side);
    core.branches().close_idle(Duration::ZERO);
    assert!(core.branches().open_lane(&oval.id).is_none());

    let side = lane(&core, &oval);
    assert_eq!(whole(side.runner.as_ref()), doc);
    assert!(side.runner.undo(&person(), &Grants::person()).unwrap().is_some());
    assert_eq!(note_of(side.runner.as_ref()), Some(json!("x")), "the undo stack came back with the lane");
    let second = agent_session(&side, "two").id;
    assert!(second > first, "{second:?} after {first:?}");
    drop(side);
    core.close().unwrap();

    let core = AppCore::open(tmp.path(), options()).unwrap();
    let folder = core.branches().lane(&BranchId::main()).unwrap();
    let third = agent_session(&folder, "three").id;
    let side = lane(&core, &branch(&core, "aria/oval"));
    let fourth = agent_session(&side, "four").id;
    drop((folder, side));
    // A crash writes no close mark: the mark kept ahead still keeps the next run's ids new.
    core.abandon();

    let core = AppCore::open(tmp.path(), options()).unwrap();
    let folder = core.branches().lane(&BranchId::main()).unwrap();
    let fifth = agent_session(&folder, "five").id;
    let ids = [first, second, third, fourth, fifth];
    let mut seen = ids.to_vec();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), ids.len(), "no session id repeats: {ids:?}");
    drop(folder);
    core.close().unwrap();
}

/// #444: two lanes edited from many threads at once while the manager creates, checkpoints, lists and closes: every
/// call returns (no lock taken out of order deadlocks) and each lane keeps its own document.
#[test]
fn many_threads_on_many_lanes_never_deadlock_and_keep_lanes_apart() {
    let tmp = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::create(tmp.path(), "demo", options()).unwrap());
    let oval = create(&core, "aria/oval");
    let start = Arc::new(Barrier::new(4));
    let (done_tx, done) = std::sync::mpsc::channel();
    let mut threads = Vec::new();
    for worker in 0..4 {
        let (core, oval, start, done_tx) = (Arc::clone(&core), oval.clone(), Arc::clone(&start), done_tx.clone());
        threads.push(std::thread::spawn(move || {
            start.wait();
            for round in 0..25 {
                match worker {
                    0 => set(core.runner().as_ref(), &person(), &format!("main {round}")),
                    1 => {
                        let side = core.branches().lane(&oval.id).unwrap();
                        set(side.runner.as_ref(), &person(), &format!("oval {round}"));
                        side.runner
                            .run(Request {
                                op: "branch.status".to_owned(),
                                args: json!({}),
                                mode: Mode::Apply,
                                actor: person(),
                                grants: Grants::person(),
                                cause: None,
                                tx: None,
                                base: None,
                            })
                            .unwrap();
                    }
                    2 => {
                        core.branches().checkpoint(&BranchId::main(), "tick", None, &person()).unwrap();
                        core.branches()
                            .create(NewBranch::new(name(&format!("t/{round}")), oval.id.clone()), &person())
                            .unwrap();
                    }
                    _ => {
                        core.run("branch.list", json!({})).unwrap();
                        drop(core.branches().list());
                        core.branches().close_idle(Duration::ZERO);
                    }
                }
            }
            done_tx.send(worker).unwrap();
        }));
    }
    drop(done_tx);
    for _ in 0..4 {
        done.recv_timeout(Duration::from_secs(120)).expect("every thread finishes: no deadlock");
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("main 24")));
    let side = core.branches().lane(&oval.id).unwrap();
    assert_eq!(note_of(side.runner.as_ref()), Some(json!("oval 24")));
    drop(side);
    Arc::try_unwrap(core).ok().unwrap().close().unwrap();
}

/// #447: a refused restore leaves no branch behind; two restores in one minute get distinct names.
#[test]
fn a_refused_restore_leaves_no_branch_and_quick_restores_get_distinct_names() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(core.runner().as_ref(), &person(), "first");
    let first = last_tx(&core);
    set(core.runner().as_ref(), &person(), "second");
    let second = last_tx(&core);
    set(core.runner().as_ref(), &person(), "third");
    create(&core, "keep");
    let before = core.branches().list().len();

    let reader = Grants::of([Scope::Read]);
    let refused = core.branches().restore(&BranchId::main(), StateRef::tx(first), &person(), &reader).unwrap_err();
    assert!(matches!(refused, BranchError::Denied(_)), "{refused}");
    assert_eq!(core.branches().list().len(), before, "a refused restore makes no branch");
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("third")));

    let one = core.branches().restore(&BranchId::main(), StateRef::tx(first), &person(), &Grants::person()).unwrap();
    let two = core.branches().restore(&BranchId::main(), StateRef::tx(second), &person(), &Grants::person()).unwrap();
    assert_ne!(one.backup.name, two.backup.name);
    assert_eq!(note_of(core.runner().as_ref()), Some(json!("second")));
    let all = core.branches().list();
    assert_eq!(all.len(), before + 2);
    let one = all.iter().find(|b| b.id == one.backup.id).unwrap();
    assert_eq!(
        core.branches().store().tree(&one.head).unwrap().pointer("/project/note"),
        Some(&json!("third")),
        "the first restore kept the state before it"
    );
    core.close().unwrap();
}

/// #441 and #433: what a checkpoint and a switch cost on a 500-file project with 10 changed files. The design's budgets
/// (release build, local SSD): a checkpoint 50 ms; a switch to an open lane 16 ms on the calling thread, to a closed
/// branch 150 ms. The ceilings here are wide enough for a debug build on a shared CI runner; the medians go in the PR.
#[test]
fn checkpoint_and_switch_cost_on_a_500_file_project() {
    let tmp = tempfile::tempdir().unwrap();
    five_hundred_files(tmp.path());
    let core = AppCore::open(tmp.path(), options()).unwrap();
    let side = create(&core, "bench/side");

    let mut checkpoints = Vec::new();
    for round in 0..5 {
        ten_changes(core.runner().as_ref(), round);
        let started = Instant::now();
        core.branches().checkpoint(&BranchId::main(), "bench", None, &person()).unwrap();
        checkpoints.push(started.elapsed());
    }

    let (mut to_closed, mut to_open) = (Vec::new(), Vec::new());
    for round in 0..5 {
        let lane_of_side = lane(&core, &side);
        ten_changes(lane_of_side.runner.as_ref(), 100 + round);
        drop(lane_of_side);
        core.branches().close_idle(Duration::ZERO);
        assert!(core.branches().open_lane(&side.id).is_none());
        // A session keeps main's lane open as a side lane, so the way back is a switch to an open lane.
        let held = core.branches().lane(&BranchId::main()).unwrap();
        let session = agent_session(&held, "keeps main open").id;
        let started = Instant::now();
        core.branches().switch(&side.id, &person()).unwrap();
        to_closed.push(started.elapsed());
        let started = Instant::now();
        core.branches().switch(&BranchId::main(), &person()).unwrap();
        to_open.push(started.elapsed());
        held.sessions.end(session, omuzaki_ops_api::EndReason::Closed).unwrap();
        drop(held);
    }
    let (checkpoint, closed, open) = (median(checkpoints), median(to_closed), median(to_open));
    println!(
        "500 files, 10 changed: checkpoint {checkpoint:?}, switch to a closed branch {closed:?}, to an open lane {open:?}"
    );
    assert!(checkpoint < Duration::from_millis(1_000), "checkpoint {checkpoint:?}");
    assert!(closed < Duration::from_millis(3_000), "switch to a closed branch {closed:?}");
    assert!(open < Duration::from_millis(1_500), "switch to an open lane {open:?}");
    core.close().unwrap();
}

/// #371: the merge preview of a 500-file project with 50 changed leaves on each side. The design's budget is 30 ms
/// with the trees cached (release build); the ceiling here is for a debug build on a shared CI runner, and the median
/// goes in the PR.
#[test]
fn a_merge_preview_of_a_500_file_project_with_50_changes_a_side() {
    let tmp = tempfile::tempdir().unwrap();
    five_hundred_files(tmp.path());
    let core = AppCore::open(tmp.path(), options()).unwrap();
    let side = create(&core, "bench/side");
    let edit = |runner: &dyn Runner, part: u64, index: u64| {
        let request = Request {
            op: "test.set_index".to_owned(),
            args: json!({ "part": part, "index": index }),
            mode: Mode::Apply,
            actor: person(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        runner.run(request).unwrap();
    };
    let lane_of_side = lane(&core, &side);
    for part in 0..50 {
        edit(lane_of_side.runner.as_ref(), part, 1_000 + part);
    }
    drop(lane_of_side);
    core.branches().checkpoint(&side.id, "fifty", None, &person()).unwrap();
    for part in 250..300 {
        edit(core.runner().as_ref(), part, 2_000 + part);
    }

    let mut times = Vec::new();
    for _ in 0..6 {
        let started = Instant::now();
        let preview = core.dry_run("branch.merge", json!({ "from": "bench/side" })).unwrap();
        times.push(started.elapsed());
        let output = preview.plan.output.unwrap();
        assert!(output["units"].as_array().is_some_and(|u| !u.is_empty()), "{output:#}");
    }
    // The first preview reads the trees; the rest find them cached.
    let cached = median(times.split_off(1));
    println!("500 files, 50 changed leaves a side: merge preview {cached:?} (first, uncached: {:?})", times[0]);
    assert!(cached < Duration::from_millis(1_000), "merge preview {cached:?}");
    core.close().unwrap();
}
