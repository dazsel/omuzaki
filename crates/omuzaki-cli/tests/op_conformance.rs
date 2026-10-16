//! Registry-wide op conformance (QG-11): every op the `omuzaki` binary registers has a valid schema and honest
//! metadata, and each example it declares decodes, dry-runs without touching anything, and, for `Undo::Auto`, applies
//! and undoes back to the exact prior document and files.
//!
//! Examples are declared on the args type (`OpRegistration::examples`). Ops with none are listed in
//! `op_conformance_allowlist.txt`, which may only shrink: an op that is missing from it and has no examples fails, and
//! so does a line for an op that now has examples. The suite prints the list's size.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "the test reads the project folder to prove an op left it alone")]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use omuzaki_anim_api::{Calibration, DeviceProfile, RetargetConfig};
use omuzaki_cli::project::SavedProject;
use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Grants, Mode, OpKind, OpRegistration, ProjectDir, Request, Runner, ServicesBuilder, Undo, registrations,
};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{
    CalibrationState, PerformError, PerformHost, PerformStatus, RetargetChange, RetargetReceipt,
};

const ALLOWLIST: &str = include_str!("op_conformance_allowlist.txt");

/// The one fixture project there is: an empty project named `demo`.
const DEFAULT_FIXTURE: &str = "demo";

/// What a run can leave behind: the document tree, every file under the project folder, and the op log's length.
#[derive(PartialEq, Debug)]
struct State {
    tree: Option<Value>,
    files: BTreeMap<String, Vec<u8>>,
    log: usize,
}

/// The performer an op's example runs against: none. It lists no triggers and refuses every request, which is all a
/// dry-run of the `trigger.*` examples reaches (`trigger.list` is the one that reads it).
struct IdleHost;

impl PerformHost for IdleHost {
    fn calibrate(&self, _frames: usize) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    fn set_retarget(&self, _change: RetargetChange) -> Result<RetargetReceipt, PerformError> {
        Err(PerformError::Unsupported)
    }

    fn set_calibration(&self, _calibration: Calibration) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    fn calibration(&self) -> Calibration {
        Calibration::identity(DeviceProfile::Arkit)
    }

    fn status(&self) -> PerformStatus {
        PerformStatus {
            tracking: Tracking::NoFrames,
            calibration: CalibrationState::Idle,
            retarget: RetargetConfig::default(),
            retarget_seq: 0,
            source_fps: 0.0,
        }
    }
}

struct Rig {
    runner: Executor,
    dir: tempfile::TempDir,
    actor: Actor,
    grants: Grants,
}

impl Rig {
    fn new(reg: &'static OpRegistration, fixture: Option<&str>) -> Result<Self, String> {
        match fixture.unwrap_or(DEFAULT_FIXTURE) {
            DEFAULT_FIXTURE => {}
            other => {
                return Err(format!(
                    "names fixture `{other}`, and the conformance test knows only `{DEFAULT_FIXTURE}`"
                ));
            }
        }
        let dir = common::new_project();
        let doc = SavedProject::open(dir.path()).map_err(|e| format!("opening the fixture failed: {e}"))?;
        let project = ProjectDir::open(dir.path()).map_err(|e| format!("opening the fixture folder failed: {e}"))?;
        let runner = Executor::with_registrations(Box::new(doc), Arc::new(SystemClock::new()), [reg])
            .map_err(|e| e.to_string())?
            .with_services(ServicesBuilder::new().insert::<dyn PerformHost>(Arc::new(IdleHost)).build())
            .with_project_dir(project);
        let actor = Actor::Person { name: "conformance".to_owned() };
        Ok(Self { runner, dir, actor, grants: Grants::person() })
    }

    fn state(&self) -> State {
        State {
            tree: self.runner.get(&DocPath::root(), &self.grants).expect("the person may read"),
            files: files_under(self.dir.path()),
            log: self.runner.log(0, usize::MAX, &self.grants).expect("the person may read").len(),
        }
    }

    fn run(&self, op: &str, args: &Value, mode: Mode) -> Result<omuzaki_ops_api::Outcome, String> {
        let request = Request {
            op: op.to_owned(),
            args: args.clone(),
            mode,
            actor: self.actor.clone(),
            grants: self.grants.clone(),
            cause: None,
            tx: None,
            base: None,
        };
        self.runner.run(request).map_err(|e| e.message)
    }
}

fn files_under(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, into: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("list the folder") {
            let path = entry.expect("read the entry").path();
            let name = path.strip_prefix(root).expect("under the root").to_string_lossy().into_owned();
            // The app's own bookkeeping (`.omuzaki/`, e.g. backups of replaced files) is not the user's files.
            if name == ".omuzaki" {
                continue;
            }
            if path.is_dir() {
                // A directory is recorded too (as a trailing-slash key with no bytes), so an empty one counts.
                into.insert(format!("{name}/"), Vec::new());
                walk(root, &path, into);
            } else {
                into.insert(name, std::fs::read(&path).expect("read the file"));
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

/// Every way `reg` breaks the conformance rules; empty when it conforms.
fn problems(reg: &'static OpRegistration) -> Vec<String> {
    let meta = (reg.meta)();
    let name = meta.name;
    let mut found = Vec::new();
    let mut flag = |what: String| found.push(format!("{name}: {what}"));

    if meta.undo == Undo::None && meta.kind != OpKind::Query && !allowlist().1.contains(name) {
        flag(format!("is a {:?} op with `Undo::None`; only queries have nothing to undo", meta.kind));
    }
    let schema = serde_json::to_value((reg.schema)()).expect("a schema serialises");
    if schema.get("type") != Some(&Value::from("object")) {
        flag("its schema is not an object schema".to_owned());
    }
    for (n, args) in reg.examples().iter().enumerate() {
        if let Err(why) = check_example(reg, meta.kind, meta.undo, args) {
            flag(format!("example {n} {why}"));
        }
    }
    found
}

fn check_example(reg: &'static OpRegistration, kind: OpKind, undo: Undo, args: &Value) -> Result<(), String> {
    (reg.decode)(args.clone()).map_err(|e| format!("doesn't decode: {}", e.message))?;
    let name = (reg.meta)().name;
    let rig = Rig::new(reg, reg.fixture().as_deref())?;

    let before = rig.state();
    let dry = rig.run(name, args, Mode::DryRun).map_err(|e| format!("fails its dry-run: {e}"))?;
    if dry.applied.is_some() || rig.state() != before {
        return Err("changed the document, its files or the log in a dry-run".to_owned());
    }
    // Jobs apply by running their work, and only `Auto` ops promise an exact inverse; the rest stop at the dry-run.
    if kind == OpKind::Job || undo != Undo::Auto {
        return Ok(());
    }
    rig.run(name, args, Mode::Apply).map_err(|e| format!("fails when applied: {e}"))?;
    if !dry.plan.patch.is_empty() {
        let label = rig.runner.undo(&rig.actor, &rig.grants).map_err(|e| format!("can't be undone: {}", e.message))?;
        if label.is_none() {
            return Err("left nothing to undo although its plan has changes".to_owned());
        }
    }
    let after = rig.state();
    if after.tree != before.tree || after.files != before.files {
        let moved: Vec<&String> = before
            .files
            .keys()
            .chain(after.files.keys())
            .filter(|k| before.files.get(*k) != after.files.get(*k))
            .collect();
        return Err(format!(
            "is not restored exactly by undo (tree changed: {}, files changed: {moved:?})",
            after.tree != before.tree
        ));
    }
    Ok(())
}

/// The section of the allowlist file for edits that still declare `Undo::None`; the lines above it are ops without
/// examples.
const UNDO_NONE_SECTION: &str = "[edits with Undo::None]";

/// The allowlist's two sections: ops without examples, and edits that still declare `Undo::None`.
fn allowlist() -> (BTreeSet<&'static str>, BTreeSet<&'static str>) {
    let (mut no_examples, mut undo_none) = (BTreeSet::new(), BTreeSet::new());
    let mut section = &mut no_examples;
    for line in ALLOWLIST.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        if line == UNDO_NONE_SECTION {
            section = &mut undo_none;
        } else {
            section.insert(line);
        }
    }
    (no_examples, undo_none)
}

/// Everything wrong with `registered` against `allowed`: ops with no examples that aren't listed, and listed names that
/// no longer need to be.
fn allowlist_problems(registered: &BTreeMap<&'static str, bool>, allowed: &BTreeSet<&str>) -> Vec<String> {
    let mut found = Vec::new();
    for (name, has_examples) in registered {
        match (has_examples, allowed.contains(name)) {
            (false, false) => found.push(format!("{name}: declares no examples and isn't allowlisted; add examples")),
            (true, true) => found.push(format!("{name}: has examples now; delete it from the allowlist")),
            _ => {}
        }
    }
    found.extend(
        allowed
            .iter()
            .filter(|name| !registered.contains_key(*name))
            .map(|name| format!("{name}: allowlisted but no op has that name")),
    );
    found
}

/// Lines of the `Undo::None` section that no longer name an edit with `Undo::None`.
fn stale_undo_none(violating: &BTreeMap<&'static str, bool>, listed: &BTreeSet<&str>) -> Vec<String> {
    listed
        .iter()
        .filter(|name| !violating.get(*name).copied().unwrap_or(false))
        .map(|name| format!("{name}: no longer an edit with `Undo::None`; delete it from the allowlist"))
        .collect()
}

#[test]
fn every_registered_op_conforms() {
    omuzaki_cli::link();
    let mut registered = BTreeMap::new();
    let mut registered_kinds = BTreeMap::new();
    let mut found = Vec::new();
    for reg in registrations() {
        let meta = (reg.meta)();
        registered.insert(meta.name, !reg.examples().is_empty());
        registered_kinds.insert(meta.name, meta.undo == Undo::None && meta.kind != OpKind::Query);
        found.extend(problems(reg));
    }
    let (allowed, undo_none) = allowlist();
    found.extend(stale_undo_none(&registered_kinds, &undo_none));
    found.extend(allowlist_problems(&registered, &allowed));
    println!(
        "op conformance: {} ops registered, {} with examples, {} on the allowlist",
        registered.len(),
        registered.values().filter(|&&has| has).count(),
        allowed.len()
    );
    // `registrations()` iterates in linker order; sort so the report is the same on every platform.
    assert!(found.is_empty(), "{}", sorted_report(found));
}

/// The failures one per line, in name order, so the report doesn't depend on the order the linker put registrations in.
fn sorted_report(mut found: Vec<String>) -> String {
    found.sort();
    found.join("\n")
}

/// Planted ops, never registered: each breaks exactly one rule, and the suite must say so.
mod planted {
    use omuzaki_doc_api::Patch;
    use omuzaki_ops_api::{FsMode, Op, OpCtx, OpError, OpMeta, Plan, Scope, WriteMode};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    fn note() -> DocPath {
        DocPath::root().join("project").join("note")
    }

    fn set_note(ctx: &OpCtx<'_>, text: &str) -> Plan {
        Plan::edit(Patch::new().with(note(), ctx.doc.get(&note()).cloned(), Some(json!(text))))
    }

    fn write_marker(ctx: &OpCtx<'_>) -> Result<(), OpError> {
        use std::io::Write;
        let mut file = ctx.fs_access("marker.txt", FsMode::Write)?.open_write(WriteMode::CreateOrTruncate)?;
        file.write_all(b"written").map_err(|e| OpError::internal(e.to_string()))
    }

    const fn meta(name: &'static str, undo: Undo) -> OpMeta {
        OpMeta { name, summary: "A planted op.", kind: OpKind::Edit, scope: Scope::EditScene, undo }
    }

    /// Conforms: sets the note.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct Honest {
        text: String,
    }

    impl Op for Honest {
        fn meta() -> OpMeta {
            meta("planted.honest", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(set_note(ctx, &self.text))
        }
    }

    /// Writes a file while planning a dry-run.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct WritesInDryRun {
        text: String,
    }

    impl Op for WritesInDryRun {
        fn meta() -> OpMeta {
            meta("planted.writes_in_dry_run", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            write_marker(ctx)?;
            Ok(set_note(ctx, &self.text))
        }
    }

    /// Claims `Undo::Auto` but also writes a file when applied, which undoing the patch cannot take back.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct WrongInverse {
        text: String,
    }

    impl Op for WrongInverse {
        fn meta() -> OpMeta {
            meta("planted.wrong_inverse", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            if !ctx.dry_run {
                write_marker(ctx)?;
            }
            Ok(set_note(ctx, &self.text))
        }
    }

    /// Creates an empty folder while planning a dry-run.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct MakesFolder {
        text: String,
    }

    impl Op for MakesFolder {
        fn meta() -> OpMeta {
            meta("planted.makes_folder", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            ctx.fs_access("scratch", FsMode::Write)?.create_dir_all()?;
            Ok(set_note(ctx, &self.text))
        }
    }

    /// Creates an empty folder only when applied, which undoing the patch cannot take back.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct MakesFolderWhenApplied {
        text: String,
    }

    impl Op for MakesFolderWhenApplied {
        fn meta() -> OpMeta {
            meta("planted.makes_folder_when_applied", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            if !ctx.dry_run {
                ctx.fs_access("scratch", FsMode::Write)?.create_dir_all()?;
            }
            Ok(set_note(ctx, &self.text))
        }
    }

    /// An edit that says there is nothing to undo.
    #[derive(Serialize, Deserialize, JsonSchema)]
    pub struct EditWithoutUndo {}

    impl Op for EditWithoutUndo {
        fn meta() -> OpMeta {
            meta("planted.edit_without_undo", Undo::None)
        }

        fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(Plan::default())
        }
    }

    /// An example its own args type refuses.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": 7 }]))]
    pub struct BadExample {
        text: String,
    }

    impl Op for BadExample {
        fn meta() -> OpMeta {
            meta("planted.bad_example", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(set_note(ctx, &self.text))
        }
    }
}

macro_rules! planted_reg {
    ($ty:ty) => {{
        static REG: OpRegistration = OpRegistration::of::<$ty>();
        &REG
    }};
}

fn assert_flagged(reg: &'static OpRegistration, needle: &str) {
    let found = problems(reg);
    assert!(found.iter().any(|p| p.contains(needle)), "expected a problem containing {needle:?}, got {found:?}");
}

#[test]
fn an_honest_op_conforms() {
    assert_eq!(problems(planted_reg!(planted::Honest)), Vec::<String>::new());
}

#[test]
fn a_dry_run_that_writes_fails() {
    assert_flagged(planted_reg!(planted::WritesInDryRun), "changed the document, its files or the log in a dry-run");
}

#[test]
fn a_wrong_inverse_fails() {
    assert_flagged(planted_reg!(planted::WrongInverse), "is not restored exactly by undo");
}

#[test]
fn an_empty_folder_made_in_a_dry_run_fails() {
    assert_flagged(planted_reg!(planted::MakesFolder), "changed the document, its files or the log in a dry-run");
}

#[test]
fn an_empty_folder_left_after_undo_fails() {
    assert_flagged(planted_reg!(planted::MakesFolderWhenApplied), "is not restored exactly by undo");
}

#[test]
fn failures_are_reported_in_name_order() {
    let found = vec!["b.op: second".to_owned(), "c.op: third".to_owned(), "a.op: first".to_owned()];
    assert_eq!(sorted_report(found), "a.op: first\nb.op: second\nc.op: third");
}

#[test]
fn undo_none_on_an_edit_fails() {
    assert_flagged(planted_reg!(planted::EditWithoutUndo), "only queries have nothing to undo");
}

#[test]
fn an_example_that_does_not_decode_fails() {
    assert_flagged(planted_reg!(planted::BadExample), "doesn't decode");
}

#[test]
fn the_undo_none_section_may_only_shrink() {
    let violating = BTreeMap::from([("a.bad", true), ("a.fixed", false)]);
    let found = stale_undo_none(&violating, &BTreeSet::from(["a.bad", "a.fixed", "a.gone"]));
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn the_allowlist_may_only_shrink() {
    let registered = BTreeMap::from([("a.new", false), ("a.done", true), ("a.todo", false)]);
    let allowed = BTreeSet::from(["a.done", "a.todo", "a.gone"]);
    let found = allowlist_problems(&registered, &allowed);
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().any(|p| p.starts_with("a.new:")));
    assert!(found.iter().any(|p| p.starts_with("a.done:")));
    assert!(found.iter().any(|p| p.starts_with("a.gone:")));
}

#[test]
fn the_snapshot_leaves_out_the_apps_own_folder_only() {
    let dir = tempfile::tempdir().expect("make a temp folder");
    std::fs::create_dir_all(dir.path().join(".omuzaki/replaced")).expect("make the bookkeeping folder");
    std::fs::write(dir.path().join(".omuzaki/replaced/a.kept"), b"old").expect("write a backup");
    std::fs::write(dir.path().join("marker.txt"), b"stray").expect("write a stray file");
    let files: Vec<String> = files_under(dir.path()).into_keys().collect();
    assert_eq!(files, vec!["marker.txt".to_string()]);
}
