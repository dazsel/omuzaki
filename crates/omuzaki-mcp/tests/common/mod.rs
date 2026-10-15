//! Test support: a small but faithful in-memory `Runner` (dry-run, grants, transactions, undo/redo, events) and a few
//! ops registered with `register_op!`, standing in for `omuzaki-ops` until it merges.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

pub mod http;

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::id::Slug;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, CostHint, Event, Grants, Mode, Op, OpCtx, OpError, OpInfo, OpKind, OpMeta, OpRecord, Outcome, Plan, Request,
    Runner, Scope, TxId, Undo, find, registrations,
};
use rmcp::model::{CallToolResult, Tool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ---- reading tool results ------------------------------------------------------------------------------------

/// The first text block of a tool result: what a text-only client shows its model.
pub fn text(result: &CallToolResult) -> String {
    result.content[0].as_text().expect("text content").text.clone()
}

/// A tool result's `structuredContent`.
pub fn data(result: &CallToolResult) -> &Value {
    result.structured_content.as_ref().expect("structured content")
}

/// The tool called `name`, or a panic naming it.
pub fn tool<'a>(tools: &'a [Tool], name: &str) -> &'a Tool {
    tools.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no tool {name}"))
}

// ---- what a server offers ------------------------------------------------------------------------------------

/// Ops a server holds back unless its host wired the service they read (`McpOptions::with_connections`). The test
/// servers wire none, so `tools/list` is the runner's ops minus these.
pub const WITHHELD_WITHOUT_SERVICE: [&str; 1] = ["agent.connection"];

/// The runner's ops that a server with no host services offers as tools: all but [`WITHHELD_WITHOUT_SERVICE`].
pub fn offered_ops(runner: &dyn Runner) -> Vec<OpInfo> {
    runner.ops().into_iter().filter(|op| !WITHHELD_WITHOUT_SERVICE.contains(&op.name)).collect()
}

// ---- ops ---------------------------------------------------------------------------------------------------

/// Set the blend between a character's base face and its face-shape profile.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetBlend {
    /// The character to change.
    character: Slug,
    /// 0 = base face, 1 = full profile.
    #[schemars(range(min = 0.0, max = 1.0))]
    blend: f32,
}

impl Op for SetBlend {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_blend",
            summary: "Set how far a character's face follows its face-shape profile.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(0.0..=1.0).contains(&self.blend) {
            return Err(
                OpError::invalid("blend", format!("{} is out of range", self.blend)).hint("use a value from 0 to 1")
            );
        }
        let path = face_path(&self.character).join("blend");
        let face = face_path(&self.character);
        ctx.doc
            .get(&face)
            .ok_or_else(|| OpError::not_found(format!("no character {}", self.character)).hint("create it first"))?;
        Ok(Plan::edit(Patch::new().with(path.clone(), ctx.doc.get(&path).cloned(), Some(json!(self.blend)))))
    }
}

/// Read a character's face settings.
#[derive(Serialize, Deserialize, JsonSchema)]
struct DescribeFace {
    /// The character to describe.
    character: Slug,
}

impl Op for DescribeFace {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.describe_face",
            summary: "Describe a character's face settings.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let face = ctx
            .doc
            .get(&face_path(&self.character))
            .ok_or_else(|| OpError::not_found(format!("no character {}", self.character)))?;
        Ok(Plan::answer(face.clone()))
    }
}

/// List a character's parts (a big answer, for testing that answers are never cut).
#[derive(Serialize, Deserialize, JsonSchema)]
struct ListParts {
    /// How many parts to list.
    count: u32,
}

impl Op for ListParts {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.list_parts",
            summary: "List the parts of a character.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({"parts": (0..self.count).map(|i| format!("part-{i:05}")).collect::<Vec<_>>()})))
    }
}

/// Render a preview image.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Preview {
    /// The character to render.
    character: Slug,
}

impl Op for Preview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.preview",
            summary: "Render a preview image of a character.",
            kind: OpKind::Query,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({"image": format!("omuzaki://render/{}.png", self.character)})))
    }
}

/// Measure something numeric.
#[derive(Serialize, Deserialize, JsonSchema)]
struct MeasureBlend {
    /// The character to measure.
    character: Slug,
}

impl Op for MeasureBlend {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.blend",
            summary: "Measure the face blend.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(ctx.doc.get(&face_path(&self.character).join("blend")).cloned().unwrap_or(Value::Null)))
    }
}

/// Export a model file (needs the `publish` scope).
#[derive(Serialize, Deserialize, JsonSchema)]
struct ExportVrm {
    /// The character to export.
    character: Slug,
}

impl Op for ExportVrm {
    fn meta() -> OpMeta {
        OpMeta {
            name: "export.vrm",
            summary: "Export a character as a VRM file.",
            kind: OpKind::Job,
            scope: Scope::Publish,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut plan = Plan::answer(json!({"file": format!("{}.vrm", self.character)}));
        plan.cost = Some(CostHint { seconds: Some(12.0), gpu_mib: Some(2048), usd: None });
        Ok(plan)
    }
}

/// An op with no arguments at all.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Ping;

impl Op for Ping {
    fn meta() -> OpMeta {
        OpMeta {
            name: "project.ping",
            summary: "Check that the project is open.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!("pong")))
    }
}

/// An op with a bug in it, to prove one bad op can't take the session down.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Crash;

impl Op for Crash {
    fn meta() -> OpMeta {
        OpMeta { name: "debug.crash", summary: "Panics.", kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        panic!("deliberate panic in debug.crash (expected in the test log)")
    }
}

omuzaki_ops_api::register_op!(Crash);
omuzaki_ops_api::register_op!(ListParts);
omuzaki_ops_api::register_op!(SetBlend);
omuzaki_ops_api::register_op!(DescribeFace);
omuzaki_ops_api::register_op!(Preview);
omuzaki_ops_api::register_op!(MeasureBlend);
omuzaki_ops_api::register_op!(ExportVrm);
omuzaki_ops_api::register_op!(Ping);

fn face_path(character: &Slug) -> DocPath {
    DocPath::parse("characters").expect("valid").join(character.as_str()).join("face")
}

// ---- runner ------------------------------------------------------------------------------------------------

struct Step {
    tx: TxId,
    label: String,
    patch: Patch,
    /// Undo is per actor: only the owner can undo or redo this step.
    owner: Actor,
}

struct OpenTx {
    label: String,
    owner: Actor,
    patch: Patch,
}

struct Inner {
    doc: MemDoc,
    clock: FakeClock,
    log: Vec<OpRecord>,
    open: HashMap<TxId, OpenTx>,
    next_tx: u64,
    undo: Vec<Step>,
    redo: Vec<Step>,
    subscribers: Vec<Sender<Event>>,
}

/// An in-memory runner over a `MemDoc` holding one character, `mira`, with an empty face.
pub struct FakeRunner(Mutex<Inner>);

impl FakeRunner {
    pub fn new() -> Self {
        Self::with_doc(json!({"characters": {"mira": {"face": {}}}}))
    }

    /// A runner over `doc` instead of the default one, for tests that need other characters.
    pub fn with_doc(doc: Value) -> Self {
        let doc = MemDoc::from_value(doc);
        Self(Mutex::new(Inner {
            doc,
            clock: FakeClock::new(Seconds(0.0)),
            log: Vec::new(),
            open: HashMap::new(),
            next_tx: 1,
            undo: Vec::new(),
            redo: Vec::new(),
            subscribers: Vec::new(),
        }))
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        // An op that panics while planning poisons the lock; the document itself is untouched, so carry on.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The `blend` of `mira`'s face, as stored in the document.
    pub fn blend(&self) -> Option<Value> {
        self.get(&DocPath::parse("characters/mira/face/blend").expect("valid"), &Grants::person())
            .expect("a person may read")
    }

    /// The op log so far.
    pub fn log(&self) -> Vec<OpRecord> {
        self.inner().log.clone()
    }

    /// A person (with every scope) sets `mira`'s blend directly through the runner, as the UI would.
    pub fn person_sets_blend(&self, blend: f32) {
        self.sets_blend_as(Actor::Person { name: "tester".to_owned() }, blend);
    }

    /// `actor` (with every scope) sets `mira`'s blend directly through the runner, outside MCP.
    pub fn sets_blend_as(&self, actor: Actor, blend: f32) {
        let request = Request {
            op: "face.set_blend".to_owned(),
            args: json!({"character": "mira", "blend": blend}),
            mode: Mode::Apply,
            actor,
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        self.run(request).expect("the edit applies");
    }

    /// True while a transaction is open.
    pub fn has_open_tx(&self) -> bool {
        !self.inner().open.is_empty()
    }
}

impl Inner {
    /// Checks that `tx` is open and belongs to `actor`.
    fn check_open(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        match self.open.get(&tx) {
            None => Err(OpError::not_found(format!("transaction {} isn't open", tx.0))),
            Some(open) if &open.owner != actor => {
                Err(OpError::denied(format!("transaction {} belongs to another actor", tx.0)))
            }
            Some(_) => Ok(()),
        }
    }

    /// Removes an open transaction, enforcing that only its owner may finish it.
    fn take_open(&mut self, tx: TxId, actor: &Actor) -> Result<OpenTx, OpError> {
        self.check_open(tx, actor)?;
        self.open.remove(&tx).ok_or_else(|| OpError::internal("checked open a moment ago"))
    }

    /// Adds an applied patch to `tx` if it is open, or makes it a one-step transaction of its own.
    fn attach(&mut self, tx: Option<TxId>, actor: &Actor, label: &str, patch: &Patch) -> TxId {
        if let Some((tx, open)) = tx.and_then(|tx| self.open.get_mut(&tx).map(|open| (tx, open))) {
            open.patch.extend(patch.clone());
            return tx;
        }
        let tx = TxId(self.next_tx);
        self.next_tx += 1;
        self.commit_step(Step { tx, label: label.to_owned(), patch: patch.clone(), owner: actor.clone() });
        tx
    }

    /// Records a finished step and clears its owner's redo history.
    fn commit_step(&mut self, step: Step) {
        self.redo.retain(|s| s.owner != step.owner);
        self.undo.push(step);
    }

    fn emit(&mut self, event: &Event) {
        self.subscribers.retain(|s| s.send(event.clone()).is_ok());
    }

    fn apply_undo_step(&mut self, step: &Step, forward: bool) -> Result<(), OpError> {
        let patch = if forward { step.patch.clone() } else { step.patch.inverse() };
        self.doc
            .apply(&patch)
            .map(drop)
            .map_err(|e| OpError::conflict(e.to_string()).hint("later changes touch the same values"))
    }
}

fn require_read(grants: &Grants) -> Result<(), OpError> {
    if grants.allows(Scope::Read) { Ok(()) } else { Err(OpError::denied("needs the `read` scope")) }
}

impl Runner for FakeRunner {
    fn ops(&self) -> Vec<OpInfo> {
        let mut ops: Vec<OpInfo> = registrations().map(|r| OpInfo::new((r.meta)(), (r.schema)())).collect();
        ops.sort_by_key(|o| o.name);
        ops
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let reg = find(&req.op).ok_or_else(|| OpError::invalid("op", format!("unknown op {}", req.op)))?;
        let op = (reg.decode)(req.args.clone())?;
        let meta = op.meta();
        if !req.grants.allows(Scope::Read) {
            return Err(OpError::denied(format!("{} needs the `read` scope", meta.name))
                .hint("ask the user to grant the `read` scope for this session"));
        }
        let mut inner = self.inner();
        let plan = op.plan(&OpCtx::new(&inner.doc, &req.actor, &inner.clock))?;
        // A dry-run needs only `read`, except for queries, which have nothing to preview and are gated like a call.
        if req.mode == Mode::DryRun && meta.kind != OpKind::Query {
            return Ok(Outcome { plan, applied: None });
        }
        if !req.grants.allows(meta.scope) {
            return Err(OpError::denied(format!("{} needs the `{}` scope", meta.name, meta.scope))
                .hint(format!("ask the user to grant the `{}` scope for this session", meta.scope)));
        }
        if meta.kind == OpKind::Query {
            return Ok(Outcome { plan, applied: None });
        }
        if let Some(tx) = req.tx {
            inner.check_open(tx, &req.actor)?;
        }
        let version =
            inner.doc.apply(&plan.patch).map_err(|e| OpError::conflict(e.to_string()).hint("re-plan and retry"))?;
        let tx = inner.attach(req.tx, &req.actor, meta.summary, &plan.patch);
        let record = OpRecord {
            seq: inner.log.len() as u64 + 1,
            op: req.op,
            args: req.args,
            actor: req.actor,
            cause: req.cause,
            tx,
            at: omuzaki_core::time::Clock::now(&inner.clock),
            patch: plan.patch.clone(),
            version,
            on_behalf_of: None,
            approval: None,
            effect: None,
        };
        inner.log.push(record.clone());
        inner.emit(&Event::OpApplied { record: record.clone() });
        Ok(Outcome { plan, applied: Some(record) })
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        let mut inner = self.inner();
        let tx = TxId(inner.next_tx);
        inner.next_tx += 1;
        inner.open.insert(tx, OpenTx { label: label.to_owned(), owner: actor.clone(), patch: Patch::new() });
        tx
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let mut inner = self.inner();
        let open = inner.take_open(tx, actor)?;
        inner.commit_step(Step { tx, label: open.label.clone(), patch: open.patch, owner: open.owner });
        inner.emit(&Event::TxCommitted { tx, label: open.label, actor: actor.clone() });
        Ok(())
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let mut inner = self.inner();
        let open = inner.take_open(tx, actor)?;
        let version = inner.doc.apply(&open.patch.inverse()).map_err(|e| OpError::conflict(e.to_string()))?;
        inner.emit(&Event::TxRolledBack { tx, actor: actor.clone(), version });
        Ok(())
    }

    fn undo(&self, actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        let mut inner = self.inner();
        let Some(at) = inner.undo.iter().rposition(|s| &s.owner == actor) else { return Ok(None) };
        let step = inner.undo.remove(at);
        inner.apply_undo_step(&step, false)?;
        let version = inner.doc.version();
        inner.emit(&Event::Undone {
            tx: step.tx,
            label: step.label.clone(),
            actor: actor.clone(),
            owner: step.owner.clone(),
            version,
        });
        let label = step.label.clone();
        inner.redo.push(step);
        Ok(Some(label))
    }

    fn redo(&self, actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        let mut inner = self.inner();
        let Some(at) = inner.redo.iter().rposition(|s| &s.owner == actor) else { return Ok(None) };
        let step = inner.redo.remove(at);
        inner.apply_undo_step(&step, true)?;
        let version = inner.doc.version();
        inner.emit(&Event::Redone {
            tx: step.tx,
            label: step.label.clone(),
            actor: actor.clone(),
            owner: step.owner.clone(),
            version,
        });
        let label = step.label.clone();
        inner.undo.push(step);
        Ok(Some(label))
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        require_read(grants)?;
        let (tx, rx) = channel();
        self.inner().subscribers.push(tx);
        Ok(rx)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        require_read(grants)?;
        Ok(self.inner().doc.get(path).cloned())
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        require_read(grants)?;
        let skip = usize::try_from(after_seq).unwrap_or(usize::MAX);
        Ok(self.inner().log.iter().skip(skip).take(limit).cloned().collect())
    }
}
