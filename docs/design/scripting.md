---
title: "Scripting: the script console, its sandbox, grants and journal, and macros (PL-02)"
summary: "the script console, its sandbox, grants and journal, and macros (PL-02)"
area: design
---

# Scripting: the script console, its sandbox, grants and journal, and macros (PL-02)

**Status:** proposed. **Feature:** PL-02. **Decision record:** [ADR 0034](../decisions/0034-scripting.md). **Context:** spec v3 §4.1,
§6, §30, §39 rule 2; ADR 0005 (bindings come from the registry), 0009 (determinism), 0013 (file roots), 0020 (agent
sessions), 0021 (journal); `agent-sessions.md` 3.2 and 4.5; `journal.md` 5.5 and 8.3; `library.md` D3; `evals.md`
4 and 6.3; `ux/shell.md` §3 (the Script console row).

Already decided, and not reopened here: a script is one more surface over the same `Runner` as the UI, MCP and the CLI,
with no private door; script bindings come from the op registry; paths typed inside a script add no file roots; every
call acknowledged to a script is durable first; replays are deterministic.

## 0. The decisions in brief

1. **One language: Rhai** (1.26.1, MIT OR Apache-2.0), chosen on the measurements of section 1. The deciding facts:
   every op a Rhai script can call is visible in its syntax tree before it runs (Lua cannot promise that: functions are
   values in tables), errors carry a line and a column, the language has no file, network, process or clock access to
   strip, and it builds for `x86_64-pc-windows-gnu` and `wasm32-unknown-unknown` with no C toolchain.
2. **A new implementation crate, `omuzaki-script`,** holds the engine, the limits, the generated bindings, the static
   check, the macro recorder and the `script.*` ops. No new API crate: the contract changes are two additive
   items in `omuzaki-ops-api`, the scope `Scope::Script` and the provided method `Op::coalesces` (section 7).
3. **A script runs as its caller,** with the caller's effective grants narrowed to the scopes of the ops it names, and
   never wider. A person's inner calls are `Actor::Person` with `Cause { kind: Script, text: <script name> }`; an
   agent's are its own session's.
4. **One run is one transaction,** labelled with the script's name: one undo step, rolled back on any uncaught error.
   The result is acknowledged through the calling surface's usual `sync` (ADR 0021).
5. **`script.run` is a driver job** (ADR 0036's `JobShape::Driver`; scope `script`, which no default agent ceiling
   holds), so an MCP agent runs a loop in one tool call. `script.check` is a `Query` that parses and resolves without running.
6. **Macros** are recorded from the person's committed transactions on `Runner::subscribe`, coalesced, stored as JSON
   lines of `{op, args}` (the step shape of eval fixtures), replayed as one transaction, and turned into Rhai source by
   "Save as script".

## 1. The language, by evidence

Measured 2026-10-13 on the build box (4-core Xeon @ 2.8 GHz, Rust 1.99.0, release). The bench is a scratch crate (the
PR carries its source): each engine gets the limits of section 5 and one binding, `ops.face.set_profile`, that turns
its map argument into `serde_json::Value`, calls a no-op standing in for `Runner::run`, and turns the result back, as a
generated binding will. Memory is the live Rust heap after building the engine, through a counting allocator (mlua
allocates Lua's memory through Rust's allocator).

| | Rhai 1.26.1 | mlua 0.12.2 + Lua 5.4 (vendored) | mlua 0.12.2 + Luau (vendored) |
|---|---|---|---|
| Engine start (build, limits, one binding, run a one-liner), median of 200 | 285 to 325 µs | 131 to 155 µs | 184 µs |
| Heap after start | 203 KiB | 24 KiB | 430 KiB (sandboxed globals) |
| 10 000 binding calls, median of 15 (all in the op's argument conversion) | 7.4 to 8.0 ms (0.8 µs a call) | 19.5 ms (2.0 µs) | 15 to 21 ms (1.5 to 2.1 µs) |
| `cargo build --target x86_64-pc-windows-gnu` | builds; pure Rust | builds; needs mingw `gcc` | builds; needs mingw `g++` |
| `wasm32-unknown-unknown` | builds with the `wasm-bindgen` feature (3.7 MB unoptimised bench) | fails: "don't know how to build Lua for wasm32-unknown-unknown" (lua-src) | fails: `string.h` not found (no libc for that target) |
| Error location | `Variable not found: nope (line 3, position 34)`: line and column on every error, limit errors included | `[string "script"]:3: attempt to index a nil value`: line only, in the message text | as Lua 5.4: line only |
| Operation limit (an endless loop) | built in (`set_max_operations`); stopped in 48 ms at 10⁶ operations | a count hook every 1 000 instructions; stopped in 67 ms | an interrupt callback (loop back-edges and calls); stopped in 22 ms |
| Call depth | built in (`set_max_call_levels`); stopped at 64 levels in 0.6 ms | no limit but the C stack; stopped at its overflow after 419 ms | interpreter's own stack limit; 3 ms |
| String, array and map sizes | built in, each with its own error and position | only a total memory cap (`set_memory_limit`); stopped at 64 MiB in 37 ms | as Lua 5.4; 34 ms |
| What has to be removed for a sandbox | nothing: no file, network, process or clock access exists unless registered. Its time package (`timestamp()`) is simply not loaded | `io`, `os`, `package`, `require`, `load`, `dofile`, `loadfile`, `debug`, `string.dump`, `collectgarbage` (mlua's default library set loads `io` and `os`) | `sandbox(true)` makes globals read-only; Luau ships no `io` or `os.execute` |
| Can every op a script calls be listed before it runs? | **yes**: calls are namespace-qualified (`ops::face::set_profile`), `ast.walk` finds each with its position, and `eval`, `Fn`, `call`, `curry` and `import` can be disabled at the parser (all five refused in the bench) | **no**: `ops[a][b](...)` with computed keys, and functions passed as values, defeat any static list | **no**, as Lua |
| How creators read it | Rust- and JavaScript-like (`let`, `fn`, `#{ key: value }` maps); new to most streamers | known from OBS scripting, Roblox and game mods | Roblox's Lua; gradual types |
| Maintenance | releases 2026-05-24, 05-29, 08-25, 09-10 | mlua 2026-07-05, 08-29, 10-03; lua-src 2026-09-06 | luau0-src 2026-08-29, 09-27 |
| Licence | MIT OR Apache-2.0 (register row B3) | MIT; vendored Lua is C built by `cc` (row B3) | MIT; vendored Luau is C++ (row B3) |
| Release binary of the bench (Linux, not stripped) | 4.5 MB | 1.3 MB | 2.9 MB |

Reading the table:
- **Speed and memory do not decide it.** A real op takes the executor's lock, plans and appends to the journal: tens
  of microseconds at least, against a binding cost of 0.8 to 2 µs. Start-up is under a third of a millisecond for all
  three. Rhai is the slowest at pure computation (a tree-walking interpreter), which op scripts do not do.
- **The static op list decides it.** Section 3 bases the default ceiling, the consent card and `script.check` on
  knowing every op a script can reach before it runs. With Lua that list would be a guess; with Rhai it is exact.
- **Errors a creator can act on:** Rhai gives line and column for every error, including limit errors, so the console
  can underline the exact call. Lua gives a line inside a message string that the host would have to parse.
- **Builds:** Rhai needs no C or C++ cross-compiler on Windows and builds for the web player's target. The
  web player does not run scripts in v1 (scripts edit projects; the player plays them), but nothing stops it later.
- **The cost is familiarity.** Streamers who wrote OBS Lua will meet a new syntax. Three things soften it: most people
  record a macro and edit the generated script (section 7), the console completes every op and field (section 8),
  and agents writing scripts get `script.check`'s exact errors. If a person-facing Lua is wanted later, it comes as a
  plugin (PL-01), not a second built-in language.
- **Rejected: Lua 5.4** (no static op list; a denylist sandbox; a C toolchain per target; no wasm32). **Rejected:
  Luau** (the same static-analysis gap, plus C++ and no wasm32-unknown-unknown build). **Rejected: both** (two binding
  generators, two sandboxes and two test suites for one feature).

**How scripts relate to the other automation surfaces.** The automation graph (BR-11, BR-26) is for things that happen
by themselves: event in, condition, action, live-safe, persistent, waiting allowed. A script is a batch a person or
agent starts now: no event handlers, no sleeping, finished within seconds. A BR-26 "run script" action node may later
call `script.run` in its `Automation` session; it is not part of this cut. Plugins (PL-01) are WASM components that
live as long as the app, can add ops and nodes (PL-03), and have their own sandbox; a script can only call ops, and it
sees plugin ops in its bindings as soon as they are registered.

## 2. The surface

```rhai
// omuzaki: grants = ["edit:character"]          (optional ceiling, section 3)
let face = get("characters/aki/face");          // read the document (Runner::get)
for c in ["aki", "ren"] {
    let r = try_op(|| ops::face::set_profile(#{ character: c, profile: "egg-oval", blend: 0.5 }));
    if r.ok { print(`${c}: ${r.value}`); } else { log(`skipped ${c}: ${r.error.hint}`); }
}
ops::measure::likeness(#{ character: "aki" })   // the last value is the result
```

- **Bindings are generated from `Runner::ops()`:** each registered op `area.verb_noun` becomes
  `ops::area::verb_noun(args)`, taking one map that is checked against the op's JSON Schema by the op itself.
  Nothing is hand-written per op; a plugin's ops appear when it registers them. Bindings are built once per
  `ScriptHost` and shared by every run.
- **Values:** an `Edit` returns its plan's output (the summary shown in history) or `()`; a `Query` returns its answer.
  JSON maps to Rhai maps (sorted by key, so iteration is deterministic), arrays, `i64`, `f64`, strings and `()`.
  `get(path)` reads the document through `Runner::get`.
- **Errors:** a failed op raises a script error carrying the `OpError` as a map: `code`, `message`, `field`, `hint`,
  plus `op`, `line` and `column`. Uncaught, it ends the run (section 4). `try_op(f)` runs one call and returns
  `#{ ok, value, error }`, so a loop can skip what fails. A failed op changed nothing (it fails in planning), so
  catching never leaves half an op behind. Rhai's own `try`/`catch` behaves the same.
- **Output:** `print(x)` adds a line to the run's output; `log(x)` adds a line marked as a note. Both are capped
  (section 5) and returned with the result.
- **Not in the language, on purpose:** sleep or waiting (that is jobs and the automation graph), files, network,
  processes, environment variables, the clock, randomness, `eval`, `import`, function pointers by name (`Fn`, `call`,
  `curry`). Calling `script.run` from a script is refused (no nesting).
- **Ops a script may call in v1:** `Edit` ops whose undo is `Auto`, and `Query` ops. `Job` ops and ops with
  `Undo::Explicit` (files written, streams started) are refused with `Unsupported` and a hint: under ADR 0036 a plain
  job never joins a transaction, so it cannot be one of a run's inner calls, and an external effect cannot be rolled
  back with the run. Lifting this is a follow-up of the ops contract.
- **Dry run:** every inner call is sent with `Mode::DryRun`; the result lists each call's planned patch and output.
  Calls see the document as it was before the script, so a script whose later calls read what earlier ones wrote may
  plan differently from a real run; the console says so on the dry-run result. A true dry run needs a scratch fork of
  the runner (`Runner::fork`, a research follow-up).

**The crate outline** (`omuzaki-script`, implementation; depends on `omuzaki-ops-api`, `omuzaki-doc-api` and `rhai` only):

```rust
pub struct ScriptHost { bindings: Arc<Bindings>, limits: Limits }
impl ScriptHost {
    pub fn new(ops: &[OpInfo], limits: Limits) -> Self;            // builds bindings from Runner::ops()
    pub fn check(&self, source: &str) -> CheckReport;               // parse, resolve, list ops (section 6)
    pub fn run(&self, request: RunRequest, calls: &dyn Calls, cancel: &dyn Cancel)
        -> Result<ScriptOutcome, ScriptError>;
}
/// Where a run's inner requests go, and who owns its transaction (section 4).
pub trait Calls: Send + Sync {
    fn call(&self, request: Request) -> Result<Outcome, OpError>;
    fn get(&self, path: &DocPath) -> Result<Option<Value>, OpError>;
}
pub struct RunnerCalls { /* Arc<dyn Runner>, actor, grants, cause; begins and commits its own transaction */ }
// script.run's driver job passes ADR 0036's JobCtx handle instead (the executor owns the transaction).
pub struct RunRequest {
    pub name: String, pub source: String, pub actor: Actor, pub grants: Grants,
    pub tx: Option<TxId>,            // join the caller's open transaction instead of opening one (section 4)
    pub params: Map<String, Value>,  // exposed to the script as `params`
    pub dry_run: bool,
}
pub struct ScriptOutcome { pub value: Value, pub output: Vec<OutputLine>, pub calls: u32,
                           pub tx: Option<TxId>, pub plans: Vec<PlannedCall> /* dry run only */ }
pub struct ScriptError { pub kind: ScriptErrorKind, pub line: u32, pub column: u32,
                         pub op_error: Option<OpError>, pub message: String, pub output: Vec<OutputLine> }
pub enum ScriptErrorKind { Parse, Op, Runtime, Limit(LimitKind), Cancelled, Refused }
pub trait Cancel: Send + Sync { fn is_cancelled(&self) -> bool; }
```

Changes from the issue's sketch: `ScriptHost` holds no runner. A run sends its requests through `Calls`: the console
and the CLI use `RunnerCalls` over the `Runner`, and `script.run` uses the driver job's `JobCtx` handle (ADR 0036), so
the same engine serves both and never holds a borrow across a job. `run` is a method; the request gains `tx` and
`params`; the error gains a kind and the output printed before it, so the console can show both.

## 3. Identity and grants

- **A person** running a script from the console or the CLI: inner requests carry `Actor::Person` (the same person),
  `Cause { kind: Script, text: Some(<script name>) }`, and no `base` (persons' requests are not guarded, ADR 0020).
- **An agent** calling `script.run`: inner requests carry the session's own agent actor, so the executor checks each
  one against the session's effective grants, limits, budget and fence, exactly as if the agent had sent it. The
  script's own grants never enter that check. `on_behalf_of` is stamped by the executor as usual.
- **Effective grants of a run:** `caller's grants ∩ declared ceiling ∩ (read + the scopes of the ops the script
  names)`. The last term comes from the static op list (section 6), so a script that only names `face.*` ops cannot use
  `publish` even when its caller holds it. The optional ceiling is a first-line comment,
  `// omuzaki: grants = ["edit:character"]`; it can only narrow. File roots: the host adds none (ADR 0013); a path
  argument inside a script reaches only roots the caller already holds.
- **Person-only and needs-a-person ops.** `OpError::person_only` stays refused for agent callers. For a person,
  `require_person` passes, because the person is the actor. To keep a script written by someone else from borrowing
  that, the console shows a **consent card** before a person's first run of a script whose last author was not this
  person (an agent, an import) or that names an op with `broadcast`, `publish`, `spend`, `net`, `fs` or `native`: the
  op list with each op's summary and scope. "Run" is remembered per (project, script content hash).
- **`Waiting` from an approval: scripts never queue.** A queued request runs later as its own transaction (ADR 0020
  4.4), which would apply one call of a script whose run already ended. So the executor refuses, instead of queueing,
  any request whose cause is `Script`, as it does with no approver present (agent-sessions 7.6): `Denied` with the
  need as detail and the hint "grant `<scope>` to this session before running the script". Keying this on the cause is
  safe because it can only make a request stricter. Macro replay (section 7) sends its calls with the `Script` cause
  too, so a replay never queues either; `Replay` stays the cause of the Build Theatre's re-runs (ADR 0020 9), whose
  approvals are unchanged.
- **`Waiting` from a pause:** the job waits with its transaction open (that is what pausing means, ADR 0020 4.3) and
  its wall clock stops. Resume continues it; ending the session abandons the transaction (4.2) and the run ends
  `Cancelled`.
- **`Limited`:** the host waits the `RetryAfter` and retries, as `omuzaki-mcp` does, but up to the run's wall-clock cap
  rather than 2 s; the waits count toward that cap. Persons have no rate limits.

## 4. Journal, transactions and undo

- **One transaction per run.** In the console and the CLI, `RunnerCalls` calls `begin(<script name>, actor)` before
  the first edit, sends every inner request with that `tx`, and `commit`s when the script returns; a run that applied
  no edit opens none. For `script.run` the driver job owns it (ADR 0036): the executor opens the transaction at the
  job's start, labelled with the script's name, every request sent through `JobCtx` carries it, and the executor
  commits it at `finish`. On both paths any uncaught error, limit, cancel or timeout rolls it back.
- **Inner calls are ordinary op records:** each has its own `OpRecord` with the script's actor, cause and `tx`. History
  shows the run as one step ("aki-ren-oval: 4 ops"); Ctrl+Z undoes it whole; undo-session (ADR 0020 6) treats an
  agent's run as one step of that session.
- **Joining the caller's transaction:** an agent that has a transaction open passes it in `script.run`'s request (the
  executor's `Request.tx`, which ADR 0036 accepts for driver jobs only). The driver job then neither begins nor
  commits. On error, cancel or timeout it reverts its own inner edits (the run's calls), all or nothing (ADR 0036
  decision 10): one revert patch, journalled as `TxRevert`, and the caller's earlier edits and the open transaction
  stay with their owner. **On conflict it reverts nothing:** if another actor changed one of those paths since, it
  never overwrites them; every edit stays in the caller's open transaction and the job fails with `Conflict` and
  `ErrorDetail::Unreverted` (each conflicting path with the job's value, the current value and the actor, and each
  retained path), so the caller commits, fixes up or rolls back. Its `OpApplied` events are held until the owner
  commits, so a rolled-back transaction emits none. Without it, the session's limit of one open transaction (agent-sessions 8.2)
  applies to the run's own: an agent already holding one is refused, with the hint to commit first or pass it.
- **Acknowledgement (ADR 0021 decision 3).** What a script receives from an inner call is not an acknowledgement: it
  belongs to a transaction that is still open, and a crash loses open transactions by design (`journal.md` 8.3). The
  acknowledgement is the run's result, given after the commit, through the surface: MCP and the CLI `sync` before
  answering (journal 5.5); the console is a UI and shows the result at once. So a 10 000-call script costs one flush,
  not 10 000.
- **Determinism (ADR 0009):** with the same source, `params` and document, a run sends the same requests in the same
  order. The engine has no clock or randomness, maps iterate in key order, and limits are counted in operations, not
  time, except the wall-clock cap, which only stops a run and never changes what it does.

## 5. Sandbox and limits

| Limit | Console (person) | `script.run` (job) | Enforced by |
|---|---|---|---|
| Source size | 64 KiB | 64 KiB | the host, before parsing |
| Operations (every tick, callbacks included) | 10⁶ | 10⁷ | `on_progress`, counted by the host |
| Inner op calls | 10 000 | 10 000 | the bindings |
| Call depth / expression depth | 64 / 64 (32 in functions) | same | `set_max_call_levels`, `set_max_expr_depths` |
| String / array / map size | 1 MiB / 100 000 / 10 000 | same | Rhai's size limits |
| Memory (heap held by the run's thread) | 256 MiB | 256 MiB | `MemoryBudget`, a counting global allocator, read in `on_progress` every operation |
| Output (`print`, `log`) | 1 000 lines and 64 KiB; then one "output truncated" line | same | the host |
| Wall clock | 10 s | 120 s default, 600 s at most (`limits.seconds`) | `on_progress`, checked every operation |
| Result value | 64 KiB of JSON | 64 KiB | the host |

- **Memory** is bounded by a counting allocator, not by the size limits. Rhai's size limits count the values held in
  arrays and maps and the strings in them, but not a map's keys, so a script could hold gigabytes in keys without
  reaching one (2,000 half-megabyte keys is 1 GiB). `omuzaki_script::MemoryBudget` wraps the system allocator, counts
  what the run's own thread holds, and `on_progress` stops the run at `Limits::memory_bytes` (256 MiB) with the limit
  `memory`. A binary that runs scripts installs it with `#[global_allocator]`, and a run refuses to start without it.
  It costs under 1 ns per allocation on the run's thread and nothing measurable on other threads. The count is read
  once per operation, so a single native call could pass the limit by what it allocates; the two functions that
  multiply a value (`array.pad`, `string.replace`) are checked before they copy, and every other one is bounded by
  what the run already holds. The check of `pad` measures the heap of every kind of value Rhai has (strings, arrays,
  blobs, maps by their B-tree nodes, function pointers and what they capture, timestamps, ranges, each shared value
  once) and refuses one it can't measure (a locked shared value, an unknown custom type). The peak is read again
  before the run commits, so a last call that passed the limit and freed its memory again still fails the run.
- **Turning a value into text** (`print`, `debug`, `log`, `to_string`, `to_debug`, `${...}` interpolation, `+`, `+=`
  and `append` with a string, a map's `to_json`, the message of an uncaught `throw`) is built into a sink that stops at
  the output or string limit, so formatting a value costs that limit and not the size of the value. The host registers
  its own version of each for every type Rhai's packages have one for (and for any value), and each one never fails:
  it answers the text cut at the limit and flags the run, which ends with `Limit(StringSize)`; a run that must stop
  (already over its memory limit, or with any other reason found) gets an empty text and ends at its next operation.
  Never failing matters because Rhai formats an interpolated part by calling `to_string` and, if that call fails in any
  way, formats the whole value itself, unbounded. The call's first step is an operation tick, the first after Rhai
  clones its call state for a call from native code, which the host sees (cloning the engine's tag marks the run). Every
  call a native function makes back into the engine starts the same way (`contains` comparing with `==`, a closure
  called by `map`), so a stop due at such a first tick is held once: the formatting function confirms the call was its
  own and answers an empty text, and the run stops at the next tick; a hold nobody confirmed is delivered at the next
  tick, so a native loop stops at its next element. A loop must not end with a stop still held, or the next first tick
  could be a template's `to_string` call: the package functions whose calls have no tick of their own after the first
  (`contains`, `index_of`, `==` and `!=` on arrays and maps) and those that ignore their calls' errors (`sort` and
  `dedup` with a comparer, `dedup()`) are replaced by versions that check the run before each element and before they
  return, and end with the first error a call back raises, unchanged (a limit inside a comparer ends the run and rolls
  it back); and a closure must have a body. Rhai's own operation limit is off (it fires inside that first tick, and
  Rhai's count drops what a callback of `map` or `filter` did); the host counts every tick instead.
- **Closures** are accepted only as the callback of `try_op` or of an array function (`map`, `filter`, `reduce`, ...), so
  none outlives its call: what a closure captures is not counted by Rhai's limits either. A closure with an empty body
  (`|x| {}`) is refused too: a call of it has no operation after its first, where a stop may be held (above).
- **Values that cross in** (`params`, op results) are measured as a whole before they are converted: all slots, all
  entries, all string bytes and all key bytes are held to the array, map and string limits together.
- **Cancellation:** the console's Stop button and the job manager's cancel both set the `Cancel` the host polls in
  `on_progress` (every operation) and before every inner call. A cancelled run rolls back.
- **No `import`** in v1. One file is the unit people paste, review and share; reuse comes from macros and saved
  scripts. Revisit when someone needs a shared helper twice.
- **The engine per run:** a fresh `Engine` with the shared bindings module, the limits, and only Rhai's core,
  logic, string, array, map and math packages. Nothing survives from one run to the next.

## 6. `script.*` ops

| Op | Kind | Scope | What it does |
|---|---|---|---|
| `script.check {source}` | Query | `read` | Parses and resolves, without running: the ops named with line and column; unknown ops; for argument maps written as literals, unknown and missing fields against each op's schema; the scopes the script needs; the effective ceiling; parse errors. The console runs it as you type |
| `script.run {source \| id, params?, dry_run?, seconds?}` | Job (driver) | `script` | Runs a script (section 4). Source capped at 64 KiB; refused if `script.check` finds errors. Returns `ScriptOutcome` or the `ScriptError` as an `OpError` with code `invalid` and a `detail` carrying line and column |
| `script.save {id, name, source \| steps}` / `script.delete {id}` | Edit | `script` | Write or remove the document node `scripts/<id>` in the project: a real patch (`Undo::Auto`), so it is journalled, branchable and undone with Ctrl+Z, and a script may call them |
| `script.save_mine {id, name, source \| steps}` / `script.delete_mine {id}` | Edit | `script` | Write or remove an item in My scripts through a `ScriptStore` service (`Undo::Explicit`, empty patch, like the settings ops) (section 7); a script cannot call them, because a rollback could not undo the write |
| `script.list {}` | Query | `read` | Project scripts and macros, with last author and content hash |

- **A `script` scope, held by no default agent ceiling.** An agent gets it per session or per credential from the
  person, like `publish`. `script.save` and `script.save_mine` need it too: otherwise an agent could write a script for the person to run
  with the person's grants. The scope (`Scope::Script`, serialised `script`) lands with the ops contract change, so the
  hub crate changes once.
- **Why a driver job (ADR 0036):** a run can take seconds and must be cancellable; the job manager gives progress,
  cancel, caps and lanes, and the driver shape gives the run's inner requests one executor-owned transaction, each
  taking the lock for that call only, never across the run. Its `plan` validates (size cap, `script.check`) and
  returns an empty patch; its `work` runs the engine with the `JobCtx` handle as its `Calls`. The journal records
  `JobStarted` and `JobEnded` around the inner records, and recovery marks an interrupted run `Interrupted` (its open
  transaction is lost by design). Until that lands, the console and `omuzaki script run` run `ScriptHost` over
  `RunnerCalls`; only `script.run` waits for it.
- The MCP tool `script_run` costs an agent one call for a loop over twenty characters, instead of twenty tool calls and
  their tokens.

## 7. Macros

- **Recording** (the console's Record button, the palette, or the shortcut): the recorder subscribes to
  `Runner::subscribe` and keeps the person's own work: `OpApplied` records whose actor is the recording person and
  whose cause is `Direct`, grouped by transaction. A step enters the macro on `TxCommitted`; `TxRolledBack` drops it;
  `Undone` by its owner removes it and `Redone` puts it back, so the macro is what the person kept. Agents' work,
  replays and other scripts are never recorded.
- **Coalescing, only for ops declared absolute setters:** nothing in a record proves that a call sets a value rather
  than adjusts it (a relative `x += amount` called with 1 then 2 replaces the same scalar twice, and keeping only the
  second would replay to 2 instead of 3). So an op opts in: a new provided method on the `Op` trait,
  `fn coalesces() -> bool { false }`, surfaced on `OpInfo`, means "applying this op with arguments A and then B, which
  differ only in number values, ends in the same state as applying it with B alone". Two consecutive records merge
  into the later one only when the op declares it, the arguments differ only in number values, **and** both patches
  only replace values at the same set of document paths (no node added or removed, no other path touched), a second
  guard against a wrong declaration. A slider drag on `face.set_profile` (90 records setting `face/blend`) becomes one
  call with the final value. Every other call is kept as recorded: two `face.add_mark` calls differing only in size
  make two marks, removals at indices 1 then 0 stay two calls, and a relative op stays one call per gesture. The
  default is `false`, so until an op crate opts its setters in (each with a test that A-then-B equals B), nothing
  merges. Within a transaction this applies always; across transactions only when they are 1 s or less apart on the
  runner's clock.
- **Provenance survives coalescing:** a merged step keeps, per source transaction, that transaction's arguments. An
  `Undone` of one of them removes only its contribution (the step falls back to the latest remaining source's
  arguments, and disappears when none remain); `Redone` restores it. So undo and redo after a drag leave the macro
  matching what the person kept.
- **The format** (language-neutral JSON lines; one header line, then one call per line; the step shape is the eval
  fixture's `{op, args}`, `evals.md` 4):

  ```json
  {"omuzaki_macro": 1, "name": "oval and blush", "app": "0.4.0", "recorded_by": "Alex"}
  {"op": "face.set_profile", "args": {"character": "aki", "profile": "egg-oval", "blend": 0.5}, "step": "Face shape"}
  {"op": "face.set_blush", "args": {"character": "aki", "style": "hatched", "amount": 0.3}}
  ```

  `step` keeps the transaction's label on its first call, for reading. Op names are registry names; a macro naming an
  op this build lacks is listed as unavailable, naming the op, like a library item (D3 rule 3).
- **Where macros live:** in **My scripts** by default (`<config dir>/omuzaki/scripts/<id>.macro.jsonl`, next to
  `<id>.rhai`; a personal habit travels across projects, like layouts), or in the project ("Save to project") as a
  document node `scripts/<id>` (`{name, kind: macro, steps}`), where it is journalled, branchable and
  visible to agents through `script.list`. Saved scripts live in the same two places.
- **Replay** runs the steps as one transaction labelled with the macro's name, through the same `Calls` path as a
  script (grants, limits, refusals; from the console over `RunnerCalls`, as a driver job when an agent or the
  automation graph replays it), with `Cause { kind: Script, text: "macro: <macro name>" }`. It is the `Script` cause on
  purpose: a replay is a scripted batch, so it gets the scripts' never-queue rule (section 3), and a failed replay
  leaves no pending call behind.
- **Save as script** writes Rhai source: one `ops::<area>::<verb>(#{ ... })` line per step, literal arguments, the
  `step` labels as comments, and the macro's character ids lifted into `let` bindings at the top so the script is easy
  to point at another character. The generated source must pass `script.check` (tested).
- **A macro as a library recipe:** offered only when every step meets library D3's three safety rules (edits of
  `edit:character` or `edit:scene` ops, all confined to one target, known ops); the library's own save path (LB-03)
  replaces the target id with `$character` and checks D3 again. The macro format never relaxes D3, and nothing else
  becomes a recipe.

## 8. UI contract: the Script console workspace

Layout (`ux/shell.md` row "Script console"; phase 2): left **Scripts** (Project and My scripts, with macros marked by
a ⏺ glyph); centre **Console** (editor above, output below, splitter between); right **Macros** (record, list, steps,
replay) and an **Ops** reference tab (search the registry, insert a call). All three are `register_panel!`s.

- **Editor:** line numbers, Rhai highlighting, the `script.check` diagnostics underlined at line and column as you type
  (debounced 300 ms), an op's summary on hover.
- **Completions generated from the registry and the op schemas (P8):** after `ops::` the areas; after `ops::face::`
  each op with its summary and scope; inside `#{` the op's fields with type, range, enum values and doc, required
  ones first; `get("` completes document paths from the selection.
- **Output:** `print` lines, `log` lines in the muted style, the error with its line (click to jump), the run's step
  ("Undo: aki-ren-oval, 4 ops") linking to History. A dry run lists each planned call with its patch.
- **Keyboard:** Ctrl+Enter run; Ctrl+Shift+Enter dry run; Esc stops a running script (when the console has focus);
  Ctrl+Space completes; F8 / Shift+F8 next and previous problem; Ctrl+Shift+M starts and stops macro recording
  anywhere. The status bar shows a red "● REC" while recording; recording stops when the project closes.
- **Palette entries:** "Open Script console", "Run script…", "Dry-run script…", "Start recording a macro" / "Stop
  recording", "Replay macro…", and one "Run: <name>" entry per saved script and macro, so each can be bound to a hotkey
  like any palette command.
- **Consent card** (section 3) as a modal in the console: op list, scopes, author, "Run" and "Cancel".

## 9. Tests

- **Sandbox escapes:** a corpus of scripts that must fail with a refusal, never succeed: `eval`, `Fn("ops::...")`,
  `call`, `curry`, `import`, a computed op name, an op called inside a closure or `fn` that `script.check` must still
  list, `script.run` from a script, a `Job` op, an `Explicit` op, a path outside every root, an op outside the ceiling,
  `require_person` from an agent, an approval need (refused, not queued).
- **Limits:** one test per row of section 5, each asserting the kind, line and column, and that the transaction rolled
  back.
- **Parser fuzzing:** `cargo fuzz` targets for `ScriptHost::check` and for a run against a fake runner (no panic, no
  hang past the limits, every error positioned); a seed corpus of the examples and the escape corpus.
- **Determinism:** a property test runs generated scripts twice against the same document and asserts identical
  request sequences and op logs.
- **Macros:** recorder unit tests driven by synthetic event streams (coalescing, undo and redo, rollback, other
  actors ignored), including the cases that must not merge (two `face.add_mark` calls differing only in size stay
  two; removals at indices 1 then 0 stay two and replay valid; a relative `x += amount` op called with 1 then 2 stays
  two calls and replays to 3; a numbers-only pair of an op that does not declare `coalesces` stays two) and undo or redo of one source of a merged drag;
  a replay by an agent whose step needs approval fails with nothing left pending; format round-trip; replay equals the original transactions' end state; save-as-script output passes
  `script.check` and replays to the same document.
- **Shared fixture format (QG-03):** an eval fixture's `steps` and a macro's lines are the same `{op, args}` record, so
  the eval runner accepts a `.macro.jsonl` as a fixture and the scripted driver gains an `op` line kind. Eval tasks for
  `script_run`: "set every character's blend to 0.5 in one call" (pass: one `script_run`; fail: the scope refused
  without the grant).

## 10. Task cut

Each task has a fence, a contract, acceptance checks and a `Blocked by` list. `Scope::Script` is not here: it lands in
the ops contract change (ADR 0036), with the driver job shape. The `Op::coalesces` provided method (section 7) is an
additive `omuzaki-ops-api` change, best folded into the same contract change. In order:
1. `omuzaki-script`: engine, limits, cancel, output and errors, and `RunnerCalls`, over a fake runner.
2. `omuzaki-script`: registry bindings, `try_op`, `get`, and the static check.
3. `omuzaki-ops` executor: requests caused by a script never queue.
4. `script.*` ops (`script.run` as a driver job) and `omuzaki script run|check` in the CLI.
5. The macro recorder, format, replay and save-as-script (PL-02 part 2).
6. The Console panel. 7. The Macros panel. 8. User docs and examples. 9. Fuzzing and the escape corpus.
10. QG-03 eval tasks and the shared fixture format.
