---
title: "`omuzaki-ops-api`: what an op can reach (`OpCtx`: injected services, file access)"
summary: "what an op can reach (`OpCtx`: injected services, file access)"
area: design
---

# `omuzaki-ops-api`: what an op can reach (`OpCtx`: injected services, file access)

**Status:** merged contract: services, file access, and jobs and
explicit undo (ADR 0036, section 8). **Spec:** v3 §6, §39.
**Code:** `crates/omuzaki-ops-api/src/{ctx.rs,services.rs,file_access/}` (the contract), `crates/omuzaki-ops` (the executor that supplies it).

This page explains the decisions about `OpCtx`; the types are documented in the code and this page doesn't repeat them.

## 1. The problem

Ops are registered by implementation crates (`omuzaki-io-ops`, `omuzaki-char`, `omuzaki-face`, ...) that must not depend on each
other (`crates/README.md`). Yet `io.import`, `render.*`, `head.trace`, `fit.reference`, `face.underlay` and `sheet.*` need
the asset store, the renderer, the character builder or the io registry at run time. The answer is the same as for the
document: the op sees an API-crate contract, and the app decides what stands behind it.

## 2. The shape

```
 app (wiring)                          executor                          op (impl crate)
 ServicesBuilder                       Executor::new(..)
   .insert::<dyn RenderService>(r) ──► .with_services(services) ──►  OpCtx::new(doc, actor, clock)
   .insert::<dyn Store>(s)                                              .with_services(&services)
   .build() -> Services (frozen)                                        .with_dry_run(mode == DryRun)
                                                                        ctx.service::<dyn RenderService>()?
```

- **`Services`** is a type map keyed by the `TypeId` of the trait object, built once by the app. It is `Send + Sync`,
  cheap to clone, and has no mutating method, so it is immutable once built. `Executor::with_services(self, ..)` takes
  it by value before the executor is shared, so the set cannot change while ops run.
- **`OpCtx::service::<S>() -> Result<&S, OpError>`** returns the implementation or fails with
  `ErrorCode::Unavailable`, whose message names the trait (`OpError::unavailable`). `Service` is `Send + Sync +
  'static`, blanket-implemented: it is what makes the set shareable and the type a key.
- **`OpCtx` is `#[non_exhaustive]`** and built with `OpCtx::new(doc, actor, clock)` plus `with_*` methods, so adding the
  next field does not break every caller again.
- **Zero cost without services.** `OpCtx::new` points at a `static` empty set; an op that never asks pays for two more
  words in a stack struct. The numbers are in the PR (`cargo bench -p omuzaki-ops --bench run`).

## 3. Rules

1. **Only API-crate traits are services** (`omuzaki-core`, `omuzaki-*-api`). The layering rule already enforces it: an
   implementation crate may depend on API crates only, so the traits it can name are theirs (or its own, which no other
   crate can use). Concrete implementations are chosen in the app crates (`omuzaki-cli`, `omuzaki-ui`), which may depend on
   anything. No separate `lint-deps` rule was added because it would check nothing the Cargo rule does not.
2. **A service trait is `Send + Sync`** (a `Services` is shared by every thread that runs ops).
3. **One implementation per trait per `Services`;** inserting the same trait twice keeps the last, so a test can override.
4. **A missing service is `Unavailable`, not `Unsupported`.** `Unsupported` means the op cannot do that here (do not
   retry); `Unavailable` means the app did not wire a service the op needs (report it, or retry once it is). The CLI
   exits with 8 and MCP reports `code: "unavailable"`.
5. **Listing and describing ops never needs a service.** Only running an op that asks for one can fail.
6. **A dry-run sees the same services and must not change anything through them.** The executor sets
   `OpCtx::dry_run`; an op checks it before any service call that has an effect (`if !ctx.dry_run { store.put(..) }`).
7. **Long work goes through the job path (section 8), not through `plan`.** Renders, imports and traces are
   `OpKind::Job`. The executor calls `plan` under its single document lock, so a slow service call belongs in the
   op's `work`, which runs without the lock, and never in `plan`, or it blocks every other actor.
8. **An `f32` reaches JSON through `omuzaki_doc_api::f32_value`** (#69): the shortest decimal that reads back as the
   same `f32`, so an agent that asked for `0.6` sees `0.6` in the patch, the op log and the result, not
   `0.6000000238418579`. `json!(x)` on an `f32` widens it to the nearest `f64` and is never used for document values.

## 4. Alternatives considered

- **Fields on `OpCtx` per service** (`ctx.renderer`, `ctx.store`). Rejected: the contract crate would name every
  service, and each new one would change `OpCtx` for all ops.
- **A global registry** (`inventory`, like ops). Rejected: services are chosen by the app at run time (a GPU renderer
  or the mock), and two executors in one process (tests) must not share them.
- **Opt-in marker (`impl Service for dyn RenderService`).** Rejected: it would add a `*-api` to `omuzaki-ops-api`
  dependency to every crate that defines a service, for a rule the Cargo layering already enforces.

## 5. File access: `OpCtx::fs_access`

An op that opens a file must know whether *this* actor may touch *this* path. `Scope::Fs` alone cannot say: it is one bit,
and the roots an actor was granted (`Grants::with_fs_root`) are a list of paths. So an op does not read the grants. It
asks one question, and gets back either a path it may open or a structured refusal:

```
 app:       ProjectDir::open(dir)? ─► Executor::with_project_dir(..)
 executor:  Request.grants ─► OpCtx::new(..).with_grants(&req.grants).with_project_dir(&dir)
 op:        ctx.fs_access("assets/model.vrm", FsMode::Read)? ─► CanonicalPath
```

`fs_access(path, mode)` takes the path as the op received it (a string, so Windows forms stay visible) and works in
two stages. **The first needs no disk; the second runs only for a path the first passed.**

1. **Decide by name.**
   - *Read the text* in this machine's path grammar and refuse what it cannot compare exactly: empty or NUL, a Windows
     form on Linux (`C:\x`, `\\server\share`), a POSIX root on Windows, drive-relative (`C:x`) and root-relative
     (`\x`) paths, `\\.\` device paths, and Windows names that are not files (`CON`, `x.txt::$DATA`, trailing dots).
     Those are `Invalid` with a hint. A `..` that climbs above the root of the path is `Invalid` too.
   - *Place it.* A relative path starts at the project folder (never the process's current directory; with no project
     folder it is `Invalid`). `.` and `..` are collapsed **by name**, so `a/link/../b` is `a/b` whatever `link` is.
     Collapsing by name is what makes the gate independent of the disk; the second stage checks where the path really
     ends up.
   - *Compare* the place with the project folder and the granted roots:

     | Mode | Inside the project folder | Anywhere else |
     |---|---|---|
     | `Read` | `read` | `fs` and a root that covers the path |
     | `Write` | `fs` | `fs` and a root that covers the path |

     The project folder is a root of its own for reads and writes (ADR 0013), but a write still needs the `fs` scope: a
     file written behind the document's back is not in the op log and cannot be undone. A root covers whole names
     (`/a/b` does not cover `/a/bc`), and on Windows compares ASCII case-insensitively across `C:\x`, `c:/x` and
     `\\?\C:\x`. **A root on a whole disk (`/`, `C:\`) covers nothing** (ADR 0013), and roots must be
     real paths. A person holds `fs` but no roots (`Grants::person()`); surfaces add roots as ADR 0013 describes.
   - *Refuse with `Denied`* if neither covers it. **Nothing has touched the disk or the network**: on Windows,
     handing `\\attacker\share\x` or `\\host@80\DavWWWRoot\x` to the OS connects to that server with the user's
     credentials before any answer comes back, and on every OS a lookup updates access times. A UNC share or a `WebDAV`
     name is reachable only if the user granted a root on it.
2. **Walk it and decide again.** A request under the name the project folder was opened by is first moved onto the
   folder's real path (the folder that was opened, wherever that name points later). Then the path is walked on the
   disk one name at a time, asking "what is here?" without following a link at that name. Before each question the
   place is checked against what this request may reach: inside the project folder or a root, or a folder on the way
   to one. A symbolic link (on Windows, a junction too) is replaced by its target and the walk goes on, so **a link is
   followed only into places the request may reach; a link that leads out of them is refused from its own text, and
   its target is never looked up**, not even its metadata (a link in the project to a UNC share never reaches the
   network). The real place the walk ends at is compared with the project folder's real path and the roots again.
   Anything that cannot be walked safely is refused: a link whose target is missing (a write would create the target),
   more than 40 links, a name that is not valid UTF-8. A path the actor may read that does not exist is `NotFound`.

Every `Denied` has the same text for the same grants and mode, whether the file exists or not, so a refusal does not
reveal which files exist outside the actor's reach. The answer depends on the request's grants, the project folder and
the disk, and never on `dry_run`, so a dry-run and the real run agree. A context nobody gave grants to (`OpCtx::new`
alone) holds none: forgetting to wire either grants or the project folder refuses access instead of widening it.

**The project folder** is a `ProjectDir`, validated when the app opens it (`ProjectDir::open`): it must exist, be a
folder, and *not be a filesystem root* (`/`, `C:\`, a drive, `\\server\share`), or `read` would mean everything on the
disk (`ProjectDirError::FilesystemRoot`). It keeps the real path and the path it was opened by, because a request may
name the folder either way (the by-name gate accepts both; the resolved path must match the real one).

**Opening it.** The answer is a `CanonicalPath`: the real place, the allowed folder it lies in (the project
folder, or the root that covers it) and the mode it was checked for. It has no `&Path`. Ops open it with its methods,
`open_read`, `open_write(WriteMode)`, `replace` and `create_dir_all`, each refusing a mode the path was not checked
for. `replace` writes a new file in the same folder and `Replacement::commit` renames it over the old one: on Linux the
new file has no name until then (`O_TMPFILE`, named with `linkat`); elsewhere, and on file systems without
`O_TMPFILE`, its name is 128 random bits from the OS, so nobody can plant a file or link under it in advance (a bounded
number of names is tried, then `Conflict`). On Unix the new file gets the old file's permission bits (not set-user-ID,
set-group-ID or sticky), set before it is ever visible, so a private file stays private; owner, extended attributes and
ACLs are not carried over. On Windows it gets what its folder passes on to any new file, so an explicit ACL on the old
file is lost. The commit flushes the file, then (on Unix, where the file system can) the folder. Every open starts at the allowed folder and
refuses a link at every step:

- *Linux:* `openat2(RESOLVE_NO_SYMLINKS)` opens the allowed folder, `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`
  the folders below it. Where the kernel (before 5.6) or a sandbox lacks `openat2`, a walk with
  `openat(O_NOFOLLOW | O_DIRECTORY)` per folder does the same. The file is opened with `O_NOFOLLOW` relative to its
  folder's handle, and must be a regular file (opened `O_NONBLOCK`, so a planted FIFO cannot hang it).
- *macOS and the other Unixes:* the same walk.
- *Windows:* every step is opened with `FILE_FLAG_OPEN_REPARSE_POINT` and refused if it is a name surrogate (a symbolic
  link, a junction, a WSL link). Every handle that is used is checked with `GetFinalPathNameByHandleW` to lie inside the
  allowed folder; nothing is emptied before that check, and what a refused open created is deleted again.

So a file or folder swapped for a link after the check makes the open fail with `Denied`; it cannot redirect it. The
race harness (`file_access/open/tests.rs`) swaps a folder, a file and the allowed folder itself for links with
`RENAME_EXCHANGE` while it opens them: a plain `File::open` of the same text escapes thousands of times a second, the
verified open never.

**The fence.** Every crate that registers ops carries a `clippy.toml` whose `disallowed-methods` forbids reaching files
by path (`std::fs::read`, `File::open`, `OpenOptions::open`, `create_dir_all`, `rename`, `Path::exists`, ...; clippy
sees through `use std::fs as f`). A legitimate use, a test fixture or the CLI reading the file the person typed, is
allowed where it is with a one-line reason. A crate's `clippy.toml` replaces the root one, so it repeats the workspace
settings; `omuzaki-ops-api/tests/fs_fence.rs` fails an op crate without the fence, a fence that lost a method, and a copy
that dropped a workspace setting.

**What it does not do.**
- *Hard links* (the threat model is in ADR 0013: file grants confine ops, they are not a sandbox against processes that
  can already reach a file). A hard link is not a link to a file but a second name of it, and nothing says which name
  came first. Reading one reads the file. Writing one in place (`WriteMode::CreateOrTruncate`) is refused, since it
  would change the file under its other names too; `replace` writes a new file under this name only. The guard reads
  the link count, which drvfs and 9p (`/mnt/*` under WSL) always report as 1, so it cannot see a hard link there.
- *A folder moved while an open walks it* takes the open with it: each folder is held by its handle, so a folder renamed
  to somewhere else midway is opened where it now is. Moving a folder out needs write access to both places.
- *Windows during a race.* Win32 has no handle-relative open, so a junction swapped in while the walk is under way can
  be followed by the OS before the handle check refuses the result: its target is touched (and, for a write, an empty
  file or folder can be made there and is deleted again), but never read or written.
- *Windows roots are compared in their canonical form.* An open must land under the allowed folder as
  `GetFinalPathNameByHandleW` names it, so a root on a mapped drive (`Z:\`) or named by an 8.3 short name refuses every
  open: grant its canonical path (the UNC path, the long name), as ADR 0013 asks. Native Windows tests (junctions, 8.3
  names, UNC, case-sensitive folders) are follow-up work. Windows case-folding is ASCII only, so it can over-match in a directory
  made case-sensitive (by WSL).
- *Mount points differ by platform.* On Linux and macOS a mount point is a folder to the walk and the open, so a bind
  mount inside the project is followed, wherever it is mounted from. On Windows a volume mount point is a junction, and
  refused. (`RESOLVE_NO_XDEV` would refuse mounts on Linux too, but would also break roots on other mounts below the
  project; the native Windows tests are meant to confirm the Windows side.)
- *A root that is itself reached through a symlink* covers nothing (grant the real path). Roots are compared as written,
  so a case difference on a case-insensitive Linux or macOS volume refuses, never allows.
- *Who the grants belong to.* `fs_access` uses the grants the executor gives the context. Until the sessions
  contract lands (section 6) those are the request's own, so transports must set `Request.grants` server-side.

**Alternatives considered.** `OpCtx::grants()` plus `project_dir()` (the issue's first form): rejected, every file op would
repeat the path maths, and one of them would get it wrong. A service trait (`dyn FsPolicy`): rejected, the check must not
depend on what the app wired in, and a missing service would make it `Unavailable` rather than closed. Resolving first and
deciding after (the first version of this PR): rejected in review, because the lookup itself is the access.

## 6. Sessions, approvals and "needs a person" (ADR 0020)

Agent sessions are designed in `docs/design/agent-sessions.md`; this section only says what reaches an op.

- **The executor computes the grants an op sees.** For an agent, they are the session's ceiling ∩ its person's grants
  ∩ `Request.grants`. For a person, they are `Request.grants` ∩ the person's grants. `OpCtx` gets the result, so
  `fs_access` and every scope check see what the caller may really do, whatever a transport put in the request.
- **`ctx.require_person(reason)`** is the declared "needs a person" rule. It passes when the actor is a person, or a
  person confirmed this run through the approval queue (`ctx.confirmed_by()`). Otherwise it fails with
  `OpError::needs_person(reason)`, and the executor queues the request for the person. It answers the same in a
  dry-run, like `fs_access`. Use it for value-dependent checks: a model nobody acknowledged (ADR 0016), audio that
  would reach a new egress (ADR 0018).
- **`OpError::person_only(reason)`** is for actions an agent may never take, even with a confirmation (binding an
  audio device). It is `Denied`, and it is never queued.
- **An op never queues, waits or charges by itself.** The executor queues, pauses and charges the plan's
  `CostHint.usd` against the session's budget, so ops stay pure functions of the context and the document.

## 7. Localised text (ADR 0023)

The full design is `docs/design/l10n-ops.md`. For op authors it comes down to this:

- **Write English where you write it today:** the summary in `OpMeta`, doc comments on the argument struct, every field
  and every enum variant. Their message ids are derived (`op-face-set_profile`, `op-face-set_profile--blend`), so you
  never write one. The crate's `tests/l10n.rs` (`check_crate!`) fails on a missing summary or argument description and
  regenerates `locales/en/<crate>.ftl` with `OMUZAKI_BLESS=1`.
- **Errors are declared messages, not `format!`:**
  - Declare each in the crate's `messages!` table, with a translator note and typed arguments.
  - Build the error with the generated function: `OpError::not_found(msg::part_not_found(&id, &ch))`.
  - `message` and `hint` still hold the English, for agents and logs; `text` and `hint_text` carry the id and the
    arguments for people.
- **Ids and data are never prose:** op ids, part ids, paths and scope ids go in as `Arg::Id`, numbers as `Int`, `Num`
  or `Qty`. Never join translated pieces or choose English words for a count.
- **Agents always read English** (MCP); people read their language (UI, CLI). A schema is never translated; surfaces
  look its words up by id.

## 8. Jobs and effects outside the document (ADR 0036)

The decisions and the exact signatures are in ADR 0036. This section explains how the pieces fit, what each consumer
gets, and where the earlier docs disagreed.

### 8.1 A job's life

```
 caller            executor (lock held)                  job worker (no lock)             executor (lock held)
 run / Jobs::start ─► plan: validate, price, scope ─► JobStarted durable ─► work(&JobCtx) ─► finish(&OpCtx, result)
                     admission: lane, limits, budget   (journal, ADR 0021)   read() briefly    re-check scope, apply
                     Queued ─────────────────────────► Running ────────────► progress, spend   one-op step, JobEnded
```

- **Why three calls.** `plan` must stay fast (it runs under the one lock), the long work must not hold that lock, and
  the change must be computed against the document as it is when the work ends, not as it was when it began. Splitting
  into `plan`, `work` and `finish` gives each its own context: `OpCtx` under the lock, `JobCtx` without it.
- **Why a plain job's change lands only at finish.** One undo step per job, and nothing to undo while a job is still running.
  The UI shows running jobs from `Event::JobChanged` and `JobProgress`, not from the document. A connector's
  "pending" candidate is the job itself until its finish writes the tray records.
- **Why `run` waits.** The automation's job workers, the CLI and scripts call `Runner::run` and expect an
  `Outcome`. A blocking `run` keeps all of them correct with no change; MCP and the UI use `Jobs::start` and events.
  A cancelled or failed job makes `run` return the job's error (`Cancelled`, `Limited`, or the op's own).
  A job that polls instead (`Jobs::job`, `Jobs::wait`) finds the same `Outcome` in `JobState::Done` (in memory only, never
  journalled), so an MCP task renders what the blocking call would.
- **What a finish sees.** The document now, and the actor's grants now. If the target was deleted meanwhile, the
  finish fails (`NotFound` or `Conflict`) and the job ends `Failed`; anything `work` put in the store is left to the
  store's garbage collection. If a session's grants narrowed, a finish they no longer allow is `Denied`.

### 8.2 Caps, lanes and cancellation

- **Time** is the only cap the manager can enforce on in-process work, so it is always set: four times the plan's
  estimate, at least a minute, 30 minutes without an estimate, never over a day. A job that needs more says so in
  `Plan.cost.seconds`, which is also what the person sees before it starts.
- **Money** is reserved, not charged, at start: two concurrent jobs cannot both pass a budget check that only one
  fits. The estimate is a promise to the person (the connector preview says "at most $X"), so it is also the job's
  hard cap; `JobCtx::spend` fails once actual spend would pass it, and the actual amount is what is charged.
  This refines `docs/design/agent-sessions.md` 8.1 ("charged at apply ... corrected with the actual cost"): the
  correction only ever lowers the charge, so `Sessions::charge` needs no refund. A reservation is settled when `work` returns, not
  when the job's state ends, so a cancelled job whose paid call is still in flight (its thread detached) keeps its
  money reserved and is charged whatever it reports late; nothing it might still spend is handed to another job.
- **Memory, CPU and process count** cannot be limited for a thread. Spec §6 asks that "a runaway job can't take the
  machine down", so any job that runs foreign code or a heavy tool (Blender, training, `exec.run`) starts it as a child
  process through the exec runner under OS limits (a Job Object on Windows, rlimits and a cgroup on Linux).
- **Lanes** keep one kind of work from starving another: one GPU job at a time (the live renderer owns the frame
  budget), eight network jobs (latency-bound), and up to four CPU jobs (leaving half the cores to the app). The lane is
  derived from the plan and the op's scope, so `OpMeta` does not change.
- **Cancellation** sets a flag that `checkpoint` turns into `Cancelled`. Rust cannot stop a thread, so a job that
  ignores the flag for 10 s is ended anyway and its thread detached; the lane lends a replacement thread twice, then
  refuses work, so a stuck job slows the app down rather than taking it over.

### 8.3 Sessions (ADR 0020)

| Session event | Queued jobs | Running jobs | A finished job's change |
|---|---|---|---|
| Pause (person or budget) | do not start | keep running | held (`Held`) until resume |
| Stop (pause + `cancel_session`) | cancelled | cancelled (`Stop`) | discarded |
| End or revoke | cancelled (`SessionEnded`) | cancelled | discarded; listed in `Ended.cancelled` |
| Project close | cancelled (`Shutdown`) | cancelled, 10 s grace | discarded |

A paused session changes nothing (design 4.3), which is why a finish is held rather than applied. A person's jobs
belong to no session: only an explicit cancel, a timeout or shutdown stops them. An agent may cancel only its own
session's jobs; a person may cancel any.

A session that will run out of money or time can ask first with `Sessions::request_budget` (ADR 0036 decision 11):
it pauses with `PauseReason::Budget` and queues a `Budget` approval like any other, so the agent sees "waiting on
budget" in its session state and in the `Waiting` reply to its next request, never a silent stall.

### 8.4 Explicit undo

An Explicit op says what it did outside the document as data (`Effect`), naming two registered ops: one that reverses
it and one that does it again. Undo is then an ordinary op run, with the registry's schema, scope checks and
approvals, by the actor who asks for the undo; and because the calls are stored in the op record, they survive a
restart. The executor checks the inverse patch for conflicts first, then runs the inverse calls, then applies the
patch, so a refused undo has changed nothing. If an inverse call fails part-way, the calls already run are put back
with their opposite, so the step stays whole; only when that compensation also fails is the step dropped, with an
error naming which effects are reversed and which are still in place. Both directions are required data, which is
why an effect that cannot be put back is `Undo::None`. The effects travel with each step in the history export, so
undo and redo work after a snapshot and a restart. An Explicit edit performs its effect in `plan` (rule 6), so the executor makes every
check it can (scope, session, budget, rate) before planning it, and if the apply still fails it runs the effect's
`undo` call at once. Inverse ops are fast and idempotent and plan an empty patch; anything
slower or remote is `Undo::None`, and MCP already tells agents "not undoable: be sure before applying".

Until the executor implements this, a step with an Explicit op is refused and dropped, never half-reverted (ADR 0036
decision 9). No registered op outside tests is Explicit today, so nothing users rely on changes.

### 8.5 What each consumer uses

- **Connectors.** `connect.generate` and `regenerate` are jobs on the `Net` lane: the plan is the preview and
  the price (a dry run calls nothing); `work` calls the connector with an adapter over `JobCtx` for progress and
  cancellation, reports spend with `JobCtx::spend` as each paid call returns, and puts the bytes in the store; `finish`
  writes the tray records as one undo step (`Undo::Auto`: undoing removes the candidates, the money stays spent). A
  cancelled run returns the outputs it completed, and its finish keeps them.
- **Render jobs.** `Gpu` lane with `cost.gpu_mib` and `cost.seconds`; frames go to `JobCtx::scratch`, the finished
  file is moved into the project's renders folder (a derived path) and recorded by the finish's patch. `Undo::Auto`:
  undo drops the record and the file is collected with the other unreferenced renders.
- **`io.export` (`docs/design/io-api.md`).** `Cpu` lane; writes outside the project with `fs` and `publish`. It is
  `Undo::None`, not Explicit: deleting a file the person exported, possibly already sent on, is not an undo anyone
  expects, and `replace` keeps no copy of what it overwrote. Its dry run lists the files it would write or overwrite.
- **Model downloads (ADR 0017).** `Net` lane; the download goes to the machine's model cache under a `.part` name,
  verified by hash, then renamed; a cancel keeps the `.part` so the next start resumes. The cache is machine state,
  not project state, so a download is `Undo::None`; adding the model to a project is a separate edit.
- **Automation effects (ADR 0027).** `net.http`, `file.*` and `exec.run` are jobs that the show's
  four job workers start through `Runner::run`, so they need nothing beyond decision 2; the per-session limit of four
  running jobs matches those workers.
- **The assistant's tools** call ops through the same `Runner` and see jobs as MCP does.

### 8.6 Driver jobs (scripts, macro replay)

A script run or a macro replay is long, but its work *is* edits: many ordinary op calls that must land as
one undo step and may join the caller's open transaction. A plain job cannot express that, so ADR 0036 decision 10
adds a second shape rather than a second path. A driver job's `work` calls `JobCtx::call`; each call is a normal
request (scope checks, approvals, rate limits, the op log) made as the job's actor inside the job's transaction, and
takes the lock only for that call, so other actors keep editing between calls. Cancelling a driver job rolls its
transaction back (or, inside a caller's transaction, only its own inner edits, leaving the caller's transaction to
its owner), so a half-finished script leaves nothing behind. That partial revert is all or nothing and journalled
as one physical `TxRevert` entry that carries the caller's transaction as it stands afterwards (so replay after a
snapshot never needs to know which records the snapshot merged); if someone else has since changed a path the job edited, nothing is reverted, the
job's edits stay in the caller's transaction, and the job fails with a `Conflict` that lists every conflicting path
(the job's value, the current value, who changed it) and every retained one, leaving the decision to the caller. Its inner edits reach subscribers only when the transaction
holding them commits (a caller's, when joined), so the UI and agents never see a run that is later rolled back. `Scope::Script` only lets an actor start a
script; each inner call is checked against the job's grants at that moment, like any request. A driver cannot start other jobs: it already
holds a lane slot, and waiting on a second lane from inside one is how a pool deadlocks.

### 8.7 Where the earlier docs disagreed

- `crates/omuzaki-cli/tests/surface.rs` describes a render job that "returns at once with a job id". Under this design
  the CLI's `run` waits, showing progress; `Jobs::start` is the returns-at-once path.
- `omuzaki-io-api::Progress` and `omuzaki-ai-api::Progress` stay as they are, because they are used outside jobs (the CLI
  importing a file). `omuzaki_io_ops::ops::progress::JobProgress` adapts a job's `JobCtx` to the io trait (#875).
- `omuzaki-mcp`'s tool descriptions say `history_undo` reverses an Explicit op's effects. That is true once decision 8
  lands; until then the executor refuses those undos, so the text is corrected with the interim task.
