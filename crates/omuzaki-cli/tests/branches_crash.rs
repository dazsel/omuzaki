//! A branch switch killed at every step, end to end (`docs/design/branches.md` 5.5 and 12 item 4): a seeded project
//! with two branches and edits on both is switched by the built binary, which kills itself (no unwinding, nothing
//! flushed) at a seeded step of the switch, or part way through the folder's rewrite. The next open must finish the
//! switch: the folder holds exactly what a clean switch writes, both branches keep every committed transaction and
//! their undo, and files outside the project's tree are untouched.
//!
//! A kill in the rewrite is played by stopping after the old lane's point (the last step before the rewrite) and then
//! writing a seeded part of the rewrite by hand: each file is written atomically, so a kill there leaves each file
//! either as it was or as the target has it, and some stale files not yet removed. The clean switch it is compared
//! against runs on a copy of the same project.
//!
//! Each pull request runs four seeds, one for each kill; `OMUZAKI_BRANCH_CRASH_SEEDS=20` runs twenty (the nightly
//! job), and `OMUZAKI_BRANCH_CRASH_SEED=<n>` runs one, to reproduce a failure.

// `CARGO_BIN_EXE_omuzaki-fixture` exists only when the fixture is built (`test-fixtures` feature, see Cargo.toml).
#![cfg(feature = "test-fixtures")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests write and read their fixtures on disk; the fence is for ops")]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc::branch::{FolderMarker, FolderState};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_history_api::branch::{BranchId, BranchName, Branches, NewBranch};
use omuzaki_ops_api::{
    Actor, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const FIXTURE: &str = env!("CARGO_BIN_EXE_omuzaki-fixture");

/// Longest any child process may take. Generous: a loaded CI machine is slow, a hang is infinite.
const LIMIT: Duration = Duration::from_secs(60);

/// Where the switch is killed. `rewriting` stops after `switch:old-point` and writes part of the rewrite by hand.
const KILLS: [&str; 4] = ["switch:marked", "switch:old-point", "rewriting", "switch:rewritten"];

/// Writes `value` at `path` (`null` removes it).
#[derive(Serialize, Deserialize, JsonSchema)]
struct CrashPut {
    path: String,
    value: Value,
}

impl Op for CrashPut {
    fn meta() -> OpMeta {
        OpMeta {
            name: "crash.put",
            summary: "Write a value.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = DocPath::parse(&self.path).map_err(|e| OpError::invalid("path", e.reason))?;
        let value = (!self.value.is_null()).then(|| self.value.clone());
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), value)))
    }
}
register_op!(CrashPut);

/// A small deterministic generator (`SplitMix64`), so a seed names the same project on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn coin(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

fn options() -> AppOptions {
    AppOptions::headless(person())
}

/// What each branch should hold: every path an edit wrote, and its value.
type Oracle = BTreeMap<String, Value>;

fn put(runner: &dyn Runner, oracle: &mut Oracle, path: &str, value: Value) {
    let request = Request {
        op: "crash.put".to_owned(),
        args: json!({ "path": path, "value": value }),
        mode: Mode::Apply,
        actor: person(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap_or_else(|e| panic!("put {path}: {e}"));
    oracle.insert(path.to_owned(), value);
}

/// `n` seeded edits by one side, tagged `side`: a shared character's note, a character only this side has (some
/// removed again, so the switch must delete their files), the project's note.
fn edit(rng: &mut Rng, runner: &dyn Runner, oracle: &mut Oracle, side: &str, n: usize) {
    for i in 0..n {
        match rng.below(4) {
            0 => put(runner, oracle, "characters/mia/note", json!(format!("{side} {i}"))),
            1 => put(runner, oracle, &format!("characters/{side}{i}"), json!({ "name": format!("{side} {i}") })),
            2 if i > 0 => put(runner, oracle, &format!("characters/{side}{}", i - 1), Value::Null),
            _ => put(runner, oracle, "project/note", json!(format!("{side} {i}"))),
        }
    }
}

/// Runs the fixture on `dir`, killing itself at `abort` when given. Returns the exit code (`None`: killed).
fn omuzaki(dir: &Path, args: &[&str], abort: Option<&str>) -> (Option<i32>, String) {
    let mut command = Command::new(FIXTURE);
    command.arg("--project").arg(dir).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(step) = abort {
        command.env("OMUZAKI_FIXTURE_ABORT_AT", step);
    }
    let mut child = command.spawn().expect("spawn the binary");
    let deadline = Instant::now() + LIMIT;
    while child.try_wait().expect("wait").is_none() {
        if Instant::now() > deadline {
            child.kill().ok();
            panic!("omuzaki {args:?} took longer than {LIMIT:?}");
        }
        std::thread::park_timeout(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("the output");
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned())
}

/// Every file under `dir` that isn't in a dot folder, by its relative path, split into the project's tree (the
/// `.toml` files a switch rewrites) and the rest.
fn files(dir: &Path) -> (BTreeMap<String, Vec<u8>>, BTreeMap<String, Vec<u8>>) {
    fn walk(dir: &Path, rel: &str, tree: &mut BTreeMap<String, Vec<u8>>, rest: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir.join(rel)).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if name.starts_with('.') {
                continue;
            }
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if entry.file_type().unwrap().is_dir() {
                walk(dir, &path, tree, rest);
            } else {
                let bytes = std::fs::read(entry.path()).unwrap();
                if Path::new(&name).extension().is_some_and(|e| e == "toml") {
                    tree.insert(path, bytes)
                } else {
                    rest.insert(path, bytes)
                };
            }
        }
    }
    let (mut tree, mut rest) = (BTreeMap::new(), BTreeMap::new());
    walk(dir, "", &mut tree, &mut rest);
    (tree, rest)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_dir(&entry.path(), &to.join(entry.file_name()));
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// The values `oracle` names, as `runner`'s document holds them (`null` for a path it doesn't hold).
fn held(runner: &dyn Runner, oracle: &Oracle) -> Oracle {
    let get = |path: &String| runner.get(&DocPath::parse(path).unwrap(), &Grants::person()).unwrap();
    oracle.keys().map(|path| (path.clone(), get(path).unwrap_or(Value::Null))).collect()
}

/// How many steps the person can undo on `runner`, undoing them all.
fn undo_all(runner: &dyn Runner) -> usize {
    let mut steps = 0;
    while runner.undo(&person(), &Grants::person()).unwrap().is_some() {
        steps += 1;
        assert!(steps < 1_000, "undo never ends");
    }
    steps
}

/// What a reopened project holds: each branch's values, then (undoing everything) each branch's undo depth.
fn check(dir: &Path, ids: &[(BranchId, &Oracle)]) -> Vec<(Oracle, usize)> {
    let core = AppCore::open(dir, options()).unwrap();
    let mut out = Vec::new();
    for (id, oracle) in ids {
        let lane = core.branches().lane(id).unwrap();
        out.push((held(lane.runner.as_ref(), oracle), undo_all(lane.runner.as_ref())));
    }
    core.abandon();
    out
}

fn one_seed(seed: u64) {
    let mut rng = Rng(seed);
    let kill = KILLS[usize::try_from(seed).unwrap() % KILLS.len()];
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("demo");

    // The project: a base on main, a branch from it, and seeded edits on both sides.
    let core = AppCore::create(&dir, "demo", options()).unwrap();
    let (mut on_main, mut on_side) = (Oracle::new(), Oracle::new());
    put(core.runner().as_ref(), &mut on_main, "characters", json!({ "mia": { "name": "Mia", "note": "base" } }));
    // The oracle names leaves, which later edits change, rather than the table they sit in.
    on_main.remove("characters");
    on_main.insert("characters/mia/name".to_owned(), json!("Mia"));
    on_main.insert("characters/mia/note".to_owned(), json!("base"));
    put(core.runner().as_ref(), &mut on_main, "project/note", json!("base"));
    on_side.clone_from(&on_main);
    let side =
        core.branches().create(NewBranch::new(BranchName::new("aria/side").unwrap(), BranchId::main()), &person());
    let side = side.unwrap();
    let lane = core.branches().lane(&side.id).unwrap();
    let (main_edits, side_edits) = (1 + rng.below(4), 1 + rng.below(4));
    edit(&mut rng, core.runner().as_ref(), &mut on_main, "zed", main_edits);
    edit(&mut rng, lane.runner.as_ref(), &mut on_side, "kai", side_edits);
    drop(lane);
    // Half the seeds switch to the branch first, so the switch that is killed goes back to main.
    let back = rng.coin();
    if back {
        core.branches().switch(&side.id, &person()).unwrap();
    }
    core.close().unwrap();
    let to = if back { BranchId::main() } else { side.id.clone() };
    let target = if back { "main" } else { "aria/side" };

    // Files the person keeps in the project folder that aren't the project's: a switch never touches them.
    std::fs::write(dir.join("notes.txt"), format!("seed {seed}\n")).unwrap();
    std::fs::create_dir_all(dir.join("renders")).unwrap();
    std::fs::write(dir.join("renders").join("take.png"), [0x89, b'P', b'N', b'G', 1, 2, 3]).unwrap();
    let (_, outside) = files(&dir);

    // The clean switch, on a copy: what the folder must hold afterwards.
    let reference = tmp.path().join("reference");
    copy_dir(&dir, &reference);
    let (exit, stderr) = omuzaki(&reference, &["branch", "switch", target], None);
    assert_eq!(exit, Some(0), "seed {seed}: the clean switch: {stderr}");
    let (want, _) = files(&reference);

    // The switch, killed.
    let step = if kill == "rewriting" { "switch:old-point" } else { kill };
    let (exit, stderr) = omuzaki(&dir, &["branch", "switch", target], Some(step));
    assert_eq!(exit, None, "seed {seed}: the switch was killed at {step}: {stderr}");
    let marker = FolderMarker::read(&dir).unwrap();
    assert_eq!(marker.state, FolderState::Switching, "seed {seed}: {kill}");
    if kill == "rewriting" {
        // Part of the rewrite: some files written as the target has them, some stale ones not yet removed.
        let (have, _) = files(&dir);
        for (path, bytes) in &want {
            if rng.coin() {
                let file = dir.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, bytes).unwrap();
            }
        }
        for path in have.keys().filter(|p| !want.contains_key(*p)) {
            if rng.coin() {
                std::fs::remove_file(dir.join(path)).unwrap();
            }
        }
    }

    // The next open finishes the switch, through the binary as a person would run it.
    let (exit, stderr) = omuzaki(&dir, &["branch", "status"], None);
    assert_eq!(exit, Some(0), "seed {seed}: {kill}: reopening: {stderr}");
    let marker = FolderMarker::read(&dir).unwrap();
    assert_eq!((marker.state, &marker.branch), (FolderState::Ok, &to), "seed {seed}: {kill}: the switch is finished");
    let (tree, rest) = files(&dir);
    assert_eq!(tree, want, "seed {seed}: {kill}: the folder holds exactly the target branch");
    assert_eq!(rest, outside, "seed {seed}: {kill}: files outside the tree are untouched");

    // Both branches keep every committed transaction: their values, and as many undo steps as the clean switch's.
    let ids = [(BranchId::main(), &on_main), (side.id.clone(), &on_side)];
    let after = check(&dir, &ids);
    let clean = check(&reference, &ids);
    for ((id, oracle), ((values, depth), (_, clean_depth))) in ids.iter().zip(after.iter().zip(&clean)) {
        assert_eq!(values, *oracle, "seed {seed}: {kill}: {id} keeps its edits");
        assert_eq!(depth, clean_depth, "seed {seed}: {kill}: {id} keeps its undo");
    }
    assert!(after.iter().all(|(_, depth)| *depth >= 3), "seed {seed}: the base and each side's edits undo: {after:?}");
}

fn seeds() -> Vec<u64> {
    if let Some(seed) = std::env::var("OMUZAKI_BRANCH_CRASH_SEED").ok().and_then(|s| s.parse().ok()) {
        return vec![seed];
    }
    let count = std::env::var("OMUZAKI_BRANCH_CRASH_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
    (0..count).collect()
}

#[test]
fn a_switch_killed_at_any_step_or_in_the_rewrite_is_finished_by_the_next_open() {
    for seed in seeds() {
        one_seed(seed);
    }
}
