//! Micro-benchmarks of `Runner::run` on a 1,000-node document.
//!
//! Two documents are measured for `Apply`: `MemDoc` (the test mock, which clones the whole tree on every patch, so it
//! dominates) and an in-place document that stands in for the real store, which isolates the executor's own cost:
//! lookup, decode, plan, grant check, log record, events and undo bookkeeping.
//!
//! Injected services and file access are measured too: `query/in_place_doc/services_unused` runs the plain query on an executor that
//! holds four services (no op asks for one, so it must cost the same as `query/in_place_doc`), and
//! `query/in_place_doc/service_lookup` runs a query that fetches one of them (the cost of `OpCtx::service`).
//! `query/in_place_doc/fs_access` runs a query that asks `OpCtx::fs_access` for a file inside the project folder (the
//! cost of resolving a path on a real disk), and `query/in_place_doc/grants_unused` runs the plain query on an
//! executor that has a project folder and grants with file roots, which no op asks about (it must cost the same as
//! `query/in_place_doc`: building the context passes two more references).
//!
//! Sessions (#2106): `apply/in_place_doc/100_changes` runs an edit that changes 100 nodes, and
//! `apply/in_place_doc/100_changes/8_fenced_sessions` runs it for a fenced agent while eight other sessions hold fences
//! (the cost of the fence check, ADR 0020 §5.3). `apply/in_place_doc/review_policies` runs the plain edit on an executor
//! with review policies installed that ask for no review (the cost of asking them). Every bench session has rate limits
//! too high to refuse anything, so their bookkeeping is measured without ever limiting.
//!
//! The stale-write guard (#356): `apply/in_place_doc/guard_10k_paths` runs the plain edit with a `base`, for an agent,
//! on an executor whose index of last changes holds 10,000 paths, so the difference from `apply/in_place_doc` is the
//! guard's cost for one changed path. The journal hand-off (#251): `apply/in_place_doc/1kib/no_journal` and
//! `.../noop_journal` run an edit with a 1 KiB patch without a journal and with a sink that does nothing, so their
//! difference is what handing an entry over costs.
//!
//! `job_start/idle_cpu_lane` is the time from `Jobs::start` to the first line of a job's `work` on an idle lane
//! (thread spawn included). The job is waited for between iterations, untimed, so the lane is idle at every start.
//!
//! Each sample runs a fresh executor for at most `CHUNK` ops, so the log and undo stacks (which grow with every
//! applied op) stay small however long Criterion keeps sampling.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, Value, lookup};
use omuzaki_doc_mock::MemDoc;
use omuzaki_history_api::{HistoryEntry, JournalSink, JournalStatus, Lsn, SyncError};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, FsMode, Grants, JobCtx, Jobs, Limits, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, ProjectDir,
    Request, ReviewPolicies, ReviewRequirement, Runner, Scope, Services, ServicesBuilder, SessionSpec, Sessions,
    Transport, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const NODES: u32 = 1_000;
const CHUNK: u64 = 1_000;

/// Moves one node: the shape of a typical edit op (read the current value, plan one change).
#[derive(Serialize, Deserialize, JsonSchema)]
struct Nudge {
    node: u32,
    x: i64,
}

fn node_path(node: u32) -> Result<DocPath, OpError> {
    DocPath::parse(&format!("nodes/n{node}/x")).map_err(|e| OpError::invalid("node", e.to_string()))
}

impl Op for Nudge {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bench.nudge",
            summary: "Move a node.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = node_path(self.node)?;
        let before = ctx.doc.get(&path).cloned();
        Ok(Plan::edit(Patch::new().with(path, before, Some(json!(self.x)))))
    }
}
register_op!(Nudge);

/// Reads a node: the shape of a typical query op.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Peek {
    node: u32,
}

impl Op for Peek {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bench.peek",
            summary: "Read a node.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = ctx.doc.get(&node_path(self.node)?).cloned();
        found.map(Plan::answer).ok_or_else(|| OpError::not_found("no such node"))
    }
}
register_op!(Peek);

/// Moves 100 nodes from `start` on: an edit whose patch has 100 changes.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Spread {
    start: u32,
}

impl Op for Spread {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bench.spread",
            summary: "Move 100 nodes.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut patch = Patch::new();
        for offset in 0..100 {
            let node = node_path((self.start + offset) % NODES)?;
            let before = ctx.doc.get(&node).cloned();
            patch = patch.with(node, before, Some(json!(self.start)));
        }
        Ok(Plan::edit(patch))
    }
}
register_op!(Spread);

/// Records a change at 10,000 distinct paths (which the in-place document leaves alone): fills the guard's index.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Fill {}

impl Op for Fill {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bench.fill",
            summary: "Touch 10,000 paths.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut patch = Patch::new();
        for n in 0..10_000 {
            let path = DocPath::parse(&format!("filled/p{n}")).map_err(|e| OpError::invalid("path", e.to_string()))?;
            patch = patch.with(path, None, Some(json!(n)));
        }
        Ok(Plan::edit(patch))
    }
}
register_op!(Fill);

/// Sets a node's name to a 1 KiB string: an edit whose patch is about 1 KiB.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Rename {
    node: u32,
}

impl Op for Rename {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bench.rename",
            summary: "Rename a node.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = DocPath::parse(&format!("nodes/n{}/name", self.node))
            .map_err(|e| OpError::invalid("node", e.to_string()))?;
        let before = ctx.doc.get(&path).cloned();
        Ok(Plan::edit(Patch::new().with(path, before, Some(json!("x".repeat(1_000))))))
    }
}
register_op!(Rename);

/// A journal that takes every entry and does nothing with it.
struct NoopJournal;

impl JournalSink for NoopJournal {
    fn append(&self, entry: HistoryEntry) -> Lsn {
        black_box(entry);
        Lsn(1)
    }

    fn admit(&self) -> Result<(), OpError> {
        Ok(())
    }

    fn wants_mark(&self) -> bool {
        false
    }

    fn sync(&self, _timeout: Duration) -> Result<Lsn, SyncError> {
        Ok(Lsn(1))
    }

    fn status(&self) -> JournalStatus {
        JournalStatus::Healthy
    }
}

/// Four stand-in API-crate services, so the lookup is not a one-entry special case.
macro_rules! probes {
    ($($name:ident),+) => {$(
        trait $name: Send + Sync {}
        impl $name for Probes {}
    )+};
}

struct Probes;
probes!(ProbeA, ProbeB, ProbeC, ProbeD);

fn services() -> Services {
    ServicesBuilder::new()
        .insert::<dyn ProbeA>(Arc::new(Probes))
        .insert::<dyn ProbeB>(Arc::new(Probes))
        .insert::<dyn ProbeC>(Arc::new(Probes))
        .insert::<dyn ProbeD>(Arc::new(Probes))
        .build()
}

/// Reads a node through an injected service: the shape of a query op that needs the store or the renderer.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Lookup {
    node: u32,
}

impl Op for Lookup {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bench.lookup",
            summary: "Fetch a service and read a node.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.service::<dyn ProbeC>()?;
        let found = ctx.doc.get(&node_path(self.node)?).cloned();
        found.map(Plan::answer).ok_or_else(|| OpError::not_found("no such node"))
    }
}
register_op!(Lookup);

/// Resolves a file in the project folder: the shape of an op that reads a file the user named.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Open {
    path: String,
}

impl Op for Open {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bench.open",
            summary: "Resolve a file in the project folder.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = ctx.fs_access(&self.path, FsMode::Read)?;
        Ok(Plan::answer(json!(path.to_string())))
    }
}
register_op!(Open);

/// Where `BenchJob::work` reports the moment it began.
static WORK_ENTRY: Mutex<Option<Sender<Instant>>> = Mutex::new(None);

/// A job that does nothing but say when it started.
#[derive(Serialize, Deserialize, JsonSchema)]
struct BenchJob {}

impl Op for BenchJob {
    fn meta() -> OpMeta {
        OpMeta {
            name: "bench.job",
            summary: "A job that returns at once.",
            kind: OpKind::Job,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }

    fn work(&self, _job: &JobCtx<'_>) -> Result<Value, OpError> {
        let entered = Instant::now();
        if let Some(tx) = WORK_ENTRY.lock().unwrap().as_ref() {
            tx.send(entered).unwrap();
        }
        Ok(Value::Null)
    }
}
register_op!(BenchJob);

fn tree() -> Value {
    let nodes: serde_json::Map<String, Value> =
        (0..NODES).map(|i| (format!("n{i}"), json!({"x": 0, "y": 0, "name": format!("node {i}")}))).collect();
    json!({ "nodes": nodes })
}

/// A document that changes in place, like the real store will (structural sharing, no whole-tree copy).
/// Not atomic on failure: the benchmark only applies patches that succeed.
struct InPlaceDoc {
    root: Value,
    version: DocVersion,
}

impl DocRead for InPlaceDoc {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        lookup(&self.root, path)
    }

    fn version(&self) -> DocVersion {
        self.version
    }
}

impl DocWrite for InPlaceDoc {
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError> {
        for change in &patch.changes {
            let slot = self.root.pointer_mut(&format!("/{}", change.path));
            if let (Some(slot), Some(after)) = (slot, &change.after) {
                *slot = after.clone();
            }
        }
        self.version = DocVersion(self.version.0 + 1);
        Ok(self.version)
    }
}

/// A session for the bench's person, claiming `fence`, with limits no bench run reaches: the buckets are still
/// counted on every call.
fn session(client: &str, fence: Vec<DocPath>) -> SessionSpec {
    let unlimited = Limits {
        edits_per_minute: u32::MAX,
        edit_burst: u32::MAX,
        runs_per_minute: u32::MAX,
        run_burst: u32::MAX,
        max_pending: 16,
    };
    SessionSpec {
        transport: Transport::Stdio,
        client: client.into(),
        model: None,
        on_behalf_of: Actor::Person { name: "bench".into() },
        grants: Grants::person(),
        budget: Budget::default(),
        limits: unlimited,
        parent: None,
        role: None,
        fence,
    }
}

fn executor(doc: Box<dyn DocWrite>, services: Services) -> Executor {
    let exec = Executor::new(doc, Arc::new(FakeClock::new(Seconds(0.0)))).unwrap().with_services(services);
    // The agent the requests below name: agents without a session are refused.
    exec.open(session("bench", Vec::new())).expect("the bench session opens");
    exec
}

/// Like [`executor`], with the bench agent fenced to `nodes` and eight other sessions holding fences elsewhere.
fn fenced(doc: Box<dyn DocWrite>) -> Executor {
    let exec = Executor::new(doc, Arc::new(FakeClock::new(Seconds(0.0)))).unwrap();
    exec.open(session("bench", vec![DocPath::parse("nodes").unwrap()])).expect("the bench session opens");
    for other in 0..8 {
        let fence = vec![DocPath::parse(&format!("other/s{other}")).unwrap()];
        exec.open(session("other", fence)).expect("a fenced session opens");
    }
    exec
}

/// Review policies that never ask for a review.
struct NoReview;

impl ReviewPolicies for NoReview {
    fn requirement(&self, _scope: Scope, _grant: bool) -> Option<ReviewRequirement> {
        None
    }

    fn may_approve(&self, _person: &Actor, _principal: &Actor, _policy: &str) -> bool {
        false
    }
}

fn request(op: &str, mode: Mode, i: u64, grants: &Grants) -> Request {
    let node = u32::try_from(i % u64::from(NODES)).unwrap();
    let args = match op {
        "bench.nudge" => json!({"node": node, "x": i + 1}),
        "bench.open" => json!({"path": "model.vrm"}),
        "bench.spread" => json!({"start": node}),
        _ => json!({"node": node}),
    };
    Request {
        op: op.into(),
        args,
        mode,
        actor: Actor::Agent { id: "bench#1".into(), model: None },
        grants: grants.clone(),
        cause: None,
        tx: None,
        base: None,
    }
}

/// Times `iters` runs of `op`, in chunks over fresh executors. Building requests and executors is not timed.
fn time_runs(iters: u64, op: &str, mode: Mode, make_exec: &dyn Fn() -> Executor, grants: &Grants) -> Duration {
    let mut total = Duration::ZERO;
    let mut done = 0;
    while done < iters {
        let n = CHUNK.min(iters - done);
        let exec = make_exec();
        let requests: Vec<Request> = (done..done + n).map(|i| request(op, mode, i, grants)).collect();
        let start = Instant::now();
        for req in requests {
            black_box(exec.run(req).unwrap());
        }
        total += start.elapsed();
        done += n;
    }
    total
}

/// Like [`time_runs`] for `bench.nudge`, each request carrying the document's current version as its `base`, so the
/// stale-write guard checks it.
fn time_based_runs(iters: u64, make_exec: &dyn Fn() -> Executor, grants: &Grants) -> Duration {
    let mut total = Duration::ZERO;
    let mut done = 0;
    while done < iters {
        let n = CHUNK.min(iters - done);
        let exec = make_exec();
        let base = exec.log(0, usize::MAX, grants).unwrap().last().map(|r| r.version);
        let requests: Vec<Request> =
            (done..done + n).map(|i| Request { base, ..request("bench.nudge", Mode::Apply, i, grants) }).collect();
        let start = Instant::now();
        for req in requests {
            black_box(exec.run(req).unwrap());
        }
        total += start.elapsed();
        done += n;
    }
    total
}

/// Times `iters` job starts, each from `Jobs::start` to `work` entry, on a lane that is idle at every start.
fn time_job_starts(iters: u64, make_exec: &dyn Fn() -> Executor, grants: &Grants) -> Duration {
    let (tx, rx) = channel();
    *WORK_ENTRY.lock().unwrap() = Some(tx);
    let mut total = Duration::ZERO;
    let mut done = 0;
    while done < iters {
        let n = CHUNK.min(iters - done);
        let exec = make_exec();
        for i in done..done + n {
            let req = request("bench.job", Mode::Apply, i, grants);
            let start = Instant::now();
            let info = exec.start(req).unwrap();
            let entered = rx.recv().unwrap();
            total += entered.saturating_duration_since(start);
            exec.wait(info.id, None).unwrap();
        }
        done += n;
    }
    *WORK_ENTRY.lock().unwrap() = None;
    total
}

fn bench_run(c: &mut Criterion) {
    let mem = || Box::new(MemDoc::from_value(tree())) as Box<dyn DocWrite>;
    let in_place = || Box::new(InPlaceDoc { root: tree(), version: DocVersion(0) }) as Box<dyn DocWrite>;
    let agent = Grants::agent_default();

    // A project folder with one file, for the file-access benchmarks, and grants that carry file roots.
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("model.vrm"), b"model").unwrap();
    let project_dir = ProjectDir::open(project.path()).unwrap();
    let rooted = Grants::agent_default().with_fs_root("/granted/one").with_fs_root("/granted/two");

    let with_mem = || executor(mem(), Services::none());
    let plain = || executor(in_place(), Services::none());
    let with_services = || executor(in_place(), services());
    let with_project = || executor(in_place(), Services::none()).with_project_dir(project_dir.clone());
    let with_fences = || fenced(in_place());
    let with_journal = || executor(in_place(), Services::none()).with_journal(Arc::new(NoopJournal));
    let with_index = || {
        let exec = executor(in_place(), Services::none());
        let fill = Request {
            actor: Actor::Person { name: "bench".into() },
            ..request("bench.fill", Mode::Apply, 0, &Grants::person())
        };
        exec.run(Request { args: json!({}), ..fill }).unwrap();
        exec
    };
    let with_review = || executor(in_place(), Services::none()).with_review_policies(Arc::new(NoReview));

    let mut group = c.benchmark_group("run/1000_nodes");
    group.bench_function("apply/mem_doc", |b| {
        b.iter_custom(|n| time_runs(n, "bench.nudge", Mode::Apply, &with_mem, &agent));
    });
    group.bench_function("apply/in_place_doc", |b| {
        b.iter_custom(|n| time_runs(n, "bench.nudge", Mode::Apply, &plain, &agent));
    });
    group.bench_function("apply/in_place_doc/100_changes", |b| {
        b.iter_custom(|n| time_runs(n, "bench.spread", Mode::Apply, &plain, &agent));
    });
    group.bench_function("apply/in_place_doc/100_changes/8_fenced_sessions", |b| {
        b.iter_custom(|n| time_runs(n, "bench.spread", Mode::Apply, &with_fences, &agent));
    });
    group.bench_function("apply/in_place_doc/review_policies", |b| {
        b.iter_custom(|n| time_runs(n, "bench.nudge", Mode::Apply, &with_review, &agent));
    });
    group.bench_function("apply/in_place_doc/guard_10k_paths", |b| {
        b.iter_custom(|n| time_based_runs(n, &with_index, &agent));
    });
    group.bench_function("apply/in_place_doc/1kib/no_journal", |b| {
        b.iter_custom(|n| time_runs(n, "bench.rename", Mode::Apply, &plain, &agent));
    });
    group.bench_function("apply/in_place_doc/1kib/noop_journal", |b| {
        b.iter_custom(|n| time_runs(n, "bench.rename", Mode::Apply, &with_journal, &agent));
    });
    group.bench_function("dry_run/in_place_doc", |b| {
        b.iter_custom(|n| time_runs(n, "bench.nudge", Mode::DryRun, &plain, &agent));
    });
    group.bench_function("query/in_place_doc", |b| {
        b.iter_custom(|n| time_runs(n, "bench.peek", Mode::Apply, &plain, &agent));
    });
    group.bench_function("query/in_place_doc/services_unused", |b| {
        b.iter_custom(|n| time_runs(n, "bench.peek", Mode::Apply, &with_services, &agent));
    });
    group.bench_function("query/in_place_doc/service_lookup", |b| {
        b.iter_custom(|n| time_runs(n, "bench.lookup", Mode::Apply, &with_services, &agent));
    });
    group.bench_function("query/in_place_doc/grants_unused", |b| {
        b.iter_custom(|n| time_runs(n, "bench.peek", Mode::Apply, &with_project, &rooted));
    });
    group.bench_function("query/in_place_doc/fs_access", |b| {
        b.iter_custom(|n| time_runs(n, "bench.open", Mode::Apply, &with_project, &agent));
    });
    group.finish();

    c.benchmark_group("job_start").bench_function("idle_cpu_lane", |b| {
        b.iter_custom(|n| time_job_starts(n, &plain, &agent));
    });
}

criterion_group!(benches, bench_run);
criterion_main!(benches);
