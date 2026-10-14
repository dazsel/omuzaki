# 0036: Jobs plan under the lock, work off it and finish as one undo step; effects outside the document are undone by paired ops (2026-10-13)

**Status:** accepted, with the conditions in "Contract". It adds a `omuzaki-ops-api` change (hub crate) and two
`omuzaki-history-api` journal entries.
**Context:** ADR 0027 (automation's job lane), ADR 0030 (connectors),
ADR 0020 (agent sessions), ADR 0021 (journal and recovery), spec v3 §6. **Design:**
`docs/design/ops-api.md` section 8. The connectors design is ADR 0030.

## Context

`OpKind::Job` and `Undo::Explicit` exist in the contract and mean nothing at run time. The executor plans and applies
a job like an edit under its one document lock (so a render would hold every other actor for minutes), and it undoes
an Explicit op by reverting only its patch (so a stream started by an op keeps running after "undo" says it stopped).
Connectors, renders, exports, model downloads, the automation's effect nodes and the assistant's tools all wait on a
job path, and nothing else defines one. `omuzaki-io-api` and `omuzaki-ai-api` each grew a stop-gap `Progress` trait that
says it will merge "when `omuzaki-ops-api` grows jobs".

## Decision

1. **A job is three calls.** `Op::plan` runs under the lock as today: it validates, prices (`Plan.cost`) and changes
   nothing; a dry run is the plan only and never starts work. `Op::work` runs on a job worker **without** the lock and
   returns a JSON result; it reads the document only through short `JobCtx::read` calls. `Op::finish` runs under the
   lock against the document as it is then and returns the `Plan` the executor applies. A job's plan must have an
   empty patch (otherwise `Internal`): **the document changes once, at finish, as one one-op undo step** on the
   requesting actor's history. A plain job may not join a transaction (`Request.tx` set is `Invalid`); a driver job
   (decision 10) is the one shape that edits through a transaction.
2. **`Runner::run` of a job waits.** With `Mode::Apply` it starts the job and blocks (no lock held) until it ends, so
   every existing caller (the CLI, scripts, the automation's job workers) sees a job as a slow run with the same
   `Outcome`. A new `Jobs` service (implemented by the executor, like `Sessions`) starts without waiting, lists,
   waits on and cancels jobs; MCP maps it to tasks and progress notifications.
3. **Caps.** Time: `4 × Plan.cost.seconds`, at least 60 s; 30 min without an estimate; never over 24 h. Money: the
   plan's `CostHint.usd` is **reserved** at start against the session budget and is the job's hard cap;
   `JobCtx::spend` reports actual spend as it happens and fails once it would pass the cap. **A reservation is
   settled only when the job's `work` has returned**, not when the job's state ends: the actual amount is charged
   and the rest released then. A detached thread (decision 4) keeps its whole reservation counted against the
   budget, and its late `spend` reports are still charged, so cancelling a paid call can never free money that the
   call may still spend. A reservation still held at project close is charged in full. Memory, CPU and process count cannot be capped inside the process;
   work that needs them runs out of process (the exec runner), under OS limits. On a cap the job is cancelled.
4. **Cancellation is cooperative with a deadline.** `JobCtx::checkpoint` fails with the new `ErrorCode::Cancelled`.
   A job may instead return what it completed; its finish then applies like any other. A job still running 10 s
   after cancellation is ended as `Cancelled` anyway, its thread detached and its late result discarded; a lane
   lends at most two replacement threads, then refuses new jobs (`Unavailable`) until a detached one returns.
5. **Lanes and limits.** Three lanes, from the plan: `Gpu` (`cost.gpu_mib` set) 1 wide, `Net` (scope `net` or
   `cost.usd` set) 8 wide, `Cpu` otherwise `clamp(cores / 2, 1, 4)` wide. Job threads run below normal priority and
   never touch the real-time threads. At most 256 jobs queued in all; per agent session 4 running and 16 queued
   (matching the automation's four job workers). Over a limit is `Limited` at once; nothing blocks. A job start
   counts against `Limits::edits_per_minute`.
6. **Sessions.** A paused session's queued jobs do not start, and its running jobs keep running but **hold** their
   finish until it resumes (a paused session changes nothing). Stop is pause plus `Jobs::cancel_session`. Ending or
   revoking a session cancels its jobs first (`Ended.cancelled`) and discards their results; money already reported
   stays charged. A finish re-checks the op's scope with the actor's grants at that moment, so narrowing a session's
   grants mid-run stops a finish it no longer allows (`Denied`).
7. **Journal (ADR 0021).** `JobStarted` (op, args, actor, cause) is durable **before `work` begins** and before the
   start is acknowledged, so no paid call or written file ever lacks a record. Progress is events only, never
   journalled. The finish is an ordinary `Op` entry, followed in the same lock hold by `JobEnded`. Recovery never
   re-runs a job: a `JobStarted` without `JobEnded` gets `JobEnded { Interrupted }`, its scratch folder
   (`.omuzaki/jobs/<id>/`, a derived path) is deleted, and the relaunch notice offers "Run again", which is a new job
   with fresh grants and budget.
8. **`Undo::Explicit` is a paired inverse op, not a hook.** An Explicit op that changed something outside the document
   returns `Plan.effect: Some(Effect { summary, undo, redo })`, where `undo` and `redo` are both required `OpCall`s
   (a registered op name and its arguments, written by the forward op with exactly what it did; `redo` puts the
   effect back as it was). The executor stores it in `OpRecord.effect`, so it is in the journal; the history export
   carries every step's and every open transaction's effects in apply order (`StepExport.effects`,
   `OpenTxExport.effects`), so a step is undoable and redoable after a snapshot and a restart. Undo, rollback, session end and
   `undo_session` run the step's `undo` calls newest first, as the undoing actor with its grants, **then** apply the
   inverse patch; redo runs the `redo` calls oldest first, then the patch. Inverse ops are fast, local and
   idempotent (deleting a file that is already gone succeeds), run under the lock, and plan an empty patch.
   **Compensation:** if one call fails, the calls already run are reversed with their opposite (`redo` after a failed
   undo, `undo` after a failed redo), newest first; the step stays where it was and the original error is returned.
   If a compensating call also fails, the step's effects are mixed: the executor stops, drops the step from the
   stack (journalled as `Dropped`, so history never wedges), leaves the document untouched, and returns the original
   error with a hint naming which effects are reversed and which are still in place. An effect that cannot be both
   reversed and put back quickly and locally (an upload, an export, a sent message) is declared `Undo::None`, never
   Explicit.
9. **Today, and for records without an effect.** Until the executor implements decision 8, undoing a step that
   contains an Explicit op is refused with `Unsupported`, and the step is dropped under the no-wedge rule (journalled
   as `Dropped`), naming the op; it is never half-reverted. `undo_session` lists such steps in `SessionUndo.outside`,
   which the contract already has. After decision 8 lands, an Explicit record whose `effect` is `None` changed
   nothing outside the document that run and is undone by its patch alone.
10. **Driver jobs** (for scripting and macro replay). An op declares
    `Op::job_shape() -> JobShape::Driver` (default `Plain`); its `plan` still returns an empty patch. Its `work`
    edits through `JobCtx::call`, which sends an inner request through the executor as the job's actor, with the
    job's grants, cause and transaction; each call takes the lock for that call only and is an ordinary `OpRecord`
    with that `tx`. The executor opens the transaction at start (labelled with the job's label) and commits it after
    `finish`, whose patch, if any, joins it: **the run is one undo step**. Error, cancellation or a cap rolls it back;
    a rollback that conflicts is kept as "<label> (unfinished)", as a session end does. If the caller passed
    `tx`, a driver job joins it instead and never begins, commits or rolls back that transaction: on error,
    cancellation or a cap it reverts **only its own inner edits**, all or nothing, and stops, leaving the caller's
    edits and the open transaction to its owner. The executor builds one revert patch for the job's records (newest
    first) and checks it for conflicts like an undo. **Clean:** it applies the patch, removes those records from the
    transaction, and journals one physical entry, `HistoryEntry::TxRevert { tx, seqs, patch, remaining }`, where
    `remaining` is the whole open transaction as it stands after the revert (its patch, scopes and effects, the same
    `OpenTxExport` a snapshot holds). Replay **replaces** the transaction's state with `remaining` rather than
    subtracting records, so it is right whether the journal tail starts before the job's records or after a snapshot
    that already merged them; recovery, the history export and a later undo or redo of the caller's committed step all
    see the transaction without the job's edits or effects.
    **Conflict** (another actor changed a path since, for example `x: 1→2` by the job, then `2→3` by someone else):
    it reverts nothing, never overwrites the other actor, and leaves all the job's edits in the caller's transaction;
    the job ends `Failed` with `Conflict` and `ErrorDetail::Unreverted`, listing every conflicting path (the job's
    value, the current value, who changed it) and every retained non-conflicting path, so the caller sees the whole
    picture and decides: commit as is, fix up, or roll the whole transaction back under the usual rollback rules.
    Reverting nothing avoids leaving half of a set of edits that were written together. A crash after `TxRevert` and
    before the caller commits recovers the open transaction as any other (ADR 0021: rolled back whole). So "rolls back everything" always means everything the job
    did, and never the caller's work or another actor's.
    An inner request for a job op is `Invalid` (a driver holding a lane while waiting on another lane's job could
    deadlock it); an inner dry run and an inner query are allowed. Decisions 3 to 7 apply unchanged: a paused
    session's inner call waits with the transaction open; `JobStarted` and `JobEnded` bracket the inner records, and
    recovery marks the job `Interrupted` and loses the open transaction by design (`docs/design/journal.md` 8.3).
    Driver jobs run on the `Cpu` lane. **The Runner contract holds:** every inner request runs
    inside the one transaction; the run is one undo entry (part of the caller's, when joined); the inner `OpApplied`
    events are held and emitted only when the transaction that holds them commits: the job's own at its commit, a
    joined caller's when the caller commits (with the caller's own events), so neither a rolled-back run nor a job
    that ended `Done` inside a transaction its caller later rolls back ever emits document events; cancel and error
    roll back everything the job did. Each inner request is checked like any
    other against the job's effective grants **at that call**, so `Scope::Script` lets an actor run a script but
    never reaches past its grant: an inner edit still needs its own scope.
11. **`Sessions::request_budget(id, needed)`** (the assistant runtime design §5). A session
    that knows it will need more money or time asks before it runs out. It pauses the session with
    `PauseReason::Budget` and queues a `Need::Budget { needed, left }` approval through the normal queue, shown in
    the Theatre like any other; there is no panel-only path. The pause is session state, so the agent sees it: its
    next request gets `Waiting` with `ErrorDetail::Paused { reason: Budget }`, and `session_of` shows
    `SessionState::Paused { reason: Budget }` ("waiting on budget", not "stuck"). A person's `RaiseBudget` decision
    resumes it; a rejection leaves it paused for the person to stop or resume.

## Contract (`omuzaki-ops-api`, additive; plus `omuzaki-history-api`)

```rust
// lib.rs
pub struct OpCall { pub op: String, pub args: Value }                      // Clone, Debug, PartialEq, serde, JsonSchema
pub struct Effect { pub summary: String, pub undo: OpCall, pub redo: OpCall }           // same derives
pub struct Plan { /* patch, output, cost as today */
    #[serde(default, skip_serializing_if = "Option::is_none")] pub effect: Option<Effect> }
pub trait Op { /* meta, plan as today */
    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> { Err(OpError::unsupported(/* "<name> is not a job" */)) }
    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> { Ok(Plan::answer(result)) }
    fn job_shape() -> JobShape where Self: Sized { JobShape::Plain }
}
pub trait DynOp { /* as today */      // defaults as on Op; the blanket impl for T: Op overrides all three
    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> { /* Unsupported */ }
    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> { Ok(Plan::answer(result)) }
    fn job_shape(&self) -> JobShape { JobShape::Plain }
}

// scope.rs
pub enum Scope { /* … */ Script }              // serialised "script"; Grants::person() holds it, agent_default() not

// error.rs
pub enum ErrorCode { /* … */ Cancelled }       // CLI exit code 11, MCP "cancelled"
pub enum ErrorDetail { /* … */ Cancelled { job: JobId, reason: CancelReason },
    Unreverted { conflicts: Vec<RevertConflict>, retained: Vec<DocPath> } }   // a joined driver's revert conflicted
pub struct RevertConflict { pub path: DocPath, pub job_value: Value, pub current: Option<Value>, pub actor: Actor }

// runner.rs
pub struct OpRecord { /* … */ #[serde(default)] pub effect: Option<Effect> }
pub enum Event { /* … */
    JobChanged { job: JobInfo },                                   // every state change, in order
    JobProgress { job: JobId, stage: String, fraction: f32 },      // coalesced, at most 10 per second per job
}

// session.rs
pub struct Ended { /* … */ #[serde(default)] pub cancelled: Vec<JobId> }
pub trait Sessions { /* as today */
    fn request_budget(&self, id: SessionId, needed: Spend) -> Result<(), OpError> { Err(/* Unsupported */) }
}

// jobs.rs (new)
pub struct JobId(pub u64);                     // project-wide, never reused (recovery continues the count)
pub enum Lane { Cpu, Gpu, Net }
pub enum CancelReason { Requested, Stop, SessionEnded, Timeout, Budget, Shutdown }
pub enum JobState {
    Queued, Running,
    Held,                                      // work done; the session is paused, finish waits
    Done { tx: TxId, seq: Option<u64> },       // seq: the finish's record, None if it changed nothing
    Failed { error: OpError },
    Cancelled { by: Option<Actor>, reason: CancelReason },
    Interrupted,                               // running at a crash; set by recovery
}
pub struct JobInfo {
    pub id: JobId, pub op: String, pub label: String, pub actor: Actor, pub on_behalf_of: Option<Actor>,
    pub session: Option<SessionId>, pub cause: Option<Cause>, pub lane: Lane, pub state: JobState,
    pub stage: Option<String>, pub fraction: f32, pub estimate: Option<CostHint>, pub spent_usd_micros: u64,
    pub queued_at: Seconds, pub started_at: Option<Seconds>, pub ended_at: Option<Seconds>,
}
pub trait Jobs: Send + Sync {
    fn start(&self, req: Request) -> Result<JobInfo, OpError>;      // Apply only; returns once JobStarted is durable
    fn job(&self, id: JobId, grants: &Grants) -> Result<Option<JobInfo>, OpError>;     // needs read
    fn jobs(&self, grants: &Grants) -> Result<Vec<JobInfo>, OpError>;  // live, then the last 100 ended
    fn cancel(&self, id: JobId, by: &Actor) -> Result<JobInfo, OpError>;               // idempotent once ended
    fn cancel_session(&self, session: SessionId, by: &Actor) -> Result<Vec<JobId>, OpError>;
    fn wait(&self, id: JobId, timeout: Option<std::time::Duration>) -> Result<JobInfo, OpError>;
}
pub enum JobShape { Plain, Driver }             // Copy, Eq, Hash
pub trait JobCalls: Send + Sync {               // the executor's handle for a driver job's inner requests
    fn call(&self, op: &str, args: Value, mode: Mode) -> Result<Outcome, OpError>;
}
pub trait JobDoc: Send + Sync { fn read(&self, f: &mut dyn FnMut(&dyn DocRead)); }   // takes the lock briefly
pub trait JobSignal: Send + Sync {
    fn progress(&self, stage: &str, fraction: f32);
    fn is_cancelled(&self) -> bool;
    fn spend(&self, usd_micros: u64) -> Result<(), OpError>;         // Limited once past the job's money cap
}
#[non_exhaustive] pub struct JobCtx<'a> { /* private */ }
impl<'a> JobCtx<'a> {
    pub fn new(id: JobId, actor: &'a Actor, doc: &'a dyn JobDoc, signal: &'a dyn JobSignal) -> Self;
    pub fn with_services(self, s: &'a Services) -> Self;   pub fn with_grants(self, g: &'a Grants) -> Self;
    pub fn with_project_dir(self, d: &'a ProjectDir) -> Self;
    pub fn with_calls(self, c: &'a dyn JobCalls) -> Self;                     // set for driver jobs only
    pub fn id(&self) -> JobId;                    pub fn actor(&self) -> &'a Actor;
    pub fn read<R>(&self, f: impl FnOnce(&dyn DocRead) -> R) -> R;
    pub fn service<S: ?Sized + Service>(&self) -> Result<&'a S, OpError>;
    pub fn fs_access(&self, path: &str, mode: FsMode) -> Result<CanonicalPath, OpError>;  // as OpCtx::fs_access
    pub fn scratch(&self) -> Result<CanonicalPath, OpError>;                  // .omuzaki/jobs/<id>/, Write mode
    pub fn progress(&self, stage: &str, fraction: f32);
    pub fn is_cancelled(&self) -> bool;
    pub fn checkpoint(&self, stage: &str, fraction: f32) -> Result<(), OpError>;  // progress, then Cancelled
    pub fn spend(&self, usd_micros: u64) -> Result<(), OpError>;
    pub fn call(&self, op: &str, args: Value, mode: Mode) -> Result<Outcome, OpError>;  // Unsupported in a plain job
}

// omuzaki-history-api entry.rs (exhaustive enum; ships in the first journal format, since none has shipped)
HistoryEntry::JobStarted { job: JobId, op: String, args: Value, actor: Actor, on_behalf_of: Option<Actor>, at: Seconds }
HistoryEntry::JobEnded { job: JobId, state: JobState }
HistoryEntry::TxRevert { tx: TxId, seqs: Vec<u64>, patch: Patch, remaining: OpenTxExport }
    // a joined driver's own records left `tx`; `patch` was applied; `remaining` is `tx` afterwards
// export.rs: effects of the step's (or the open transaction's) ops, in apply order
StepExport { /* … */ #[serde(default)] effects: Vec<Effect> }      OpenTxExport { /* … */ #[serde(default)] effects: Vec<Effect> }
```

**Conditions:** additive only: every new field is `#[serde(default)]`, every new trait method
has a default, and no existing method changes. A golden test pins a document, a `Request`, an `OpRecord`, a `Plan`
and an `Ended` serialised before the change and shows they still deserialise to the same values. The T1 PR opens with
an API diff summary and needs a maintainer's approval before merge.

Every new type derives `Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema` (`Copy, Eq, Hash` where it
can), snake_case like its neighbours. Adding the `Plan`, `OpRecord` and `Ended` fields changes their struct literals
in the workspace (they gain `..`, as `on_behalf_of` did), and the new `Event`, `ErrorCode` and `Scope` variants need an
arm in the exhaustive matches in `omuzaki-mcp` and `omuzaki-cli`. `Scope::Script` is the scripting design's,
folded in here so the hub crate changes once; the wire formats stay compatible.

## Consequences

- `omuzaki-io-api::Progress` and `omuzaki-ai-api::Progress` stay (they are used outside jobs too); an op passes a small
  adapter over its `JobCtx`. Their "will merge" notes are updated, not acted on.
- A job's result appears in history when it lands, not when it was asked for; history shows the newest change on top.
- A crash between running an inverse op and journalling its undo leaves the effect undone while the document still
  shows the step; inverse and redo calls are idempotent, so undoing or redoing again converges. Accepted.
- A job cancelled while a paid call is in flight keeps its money reserved until the call returns, so a budget can
  look fuller than what was spent for up to the call's own timeout; the panel shows it as "reserved".
- Rejected: an `Op::undo(&self, ctx) -> Plan` hook (it would run outside the registry's scopes, approvals and log, and
  needs the forward op's in-memory state after a restart); half-reverting Explicit steps with a warning (a stream
  that keeps running while the UI says it stopped is a privacy failure); applying a job's patch at start (two undo
  steps, and undoing the first while the job runs has no meaning); `Runner::run` returning a job id at once (every
  `Outcome` literal and caller would change); progress in the journal (volatile, high-rate, useless after a crash);
  per-job OS memory caps for in-process work (not possible without a process boundary).
