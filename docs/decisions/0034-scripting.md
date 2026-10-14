# 0034: Scripts are Rhai, run as their caller inside one transaction, and macros are recorded op lines (2026-10-13)

**Status:** proposed. It adds one `omuzaki-ops-api` scope. **Context:** PL-02;
spec v3 §4.1, §6, §30, §39 rule 2; ADR 0005, 0009, 0013, 0020, 0021, 0036 (jobs). Design:
`docs/design/scripting.md`.

## Context

The spec promises a script console ("run op sequences (Rhai or Lua) ... record macros from UI actions") and leaves the
language open. Already settled: scripts are one more surface over the `Runner`, with bindings generated from the op
registry; paths typed in a script add no file roots; anything acknowledged to a script is durable first; replays are
deterministic. Open until now: what "scripts run as the person" means for grants and approvals, how a run maps onto
transactions and the journal, the limits, whether agents may run scripts, and how macros are recorded and stored.

## Decision

1. **Rhai is the one script language,** on the measurements of design section 1 (2026-10-13): only Rhai lets the host
   list every op a script can call before it runs (namespace-qualified calls in the syntax tree, with `eval`, `Fn`,
   `call`, `curry` and `import` disabled at the parser); it reports line and column for every error, limit errors
   included; it has no file, network, process or clock access to strip; and it builds for `x86_64-pc-windows-gnu` and
   `wasm32-unknown-unknown` without a C toolchain, where vendored Lua 5.4 and Luau do not build for wasm32 at all. Speed
   and memory did not decide it: a binding call costs 0.8 µs in Rhai and 1.5 to 2 µs in Lua, against tens of
   microseconds for a real op. Lua's familiarity to streamers is the price, softened by recording and completions.
2. **`omuzaki-script`, an implementation crate,** holds `ScriptHost` (the generated bindings and the limits), `check`,
   `run` over a `Calls` handle (the `Runner` from the console and CLI, ADR 0036's `JobCtx` from a driver job), the
   macro recorder and the `script.*` ops. No new API crate.
3. **A script runs as its caller.** A person's inner calls are `Actor::Person` with `Cause { kind: Script, text: name }`;
   an agent's are its own session's, each checked against the session's effective grants, limits, budget and fence.
   A run's grants are the caller's, narrowed by an optional declared ceiling and by the scopes of the ops the script
   names; never wider. The host adds no file roots.
4. **Scripts never queue for approval.** The executor refuses, rather than queues, a request whose cause is `Script`
   (`Denied` with the need and a hint), because an approved request would later run alone, outside a run that has
   already ended. Macro replay uses the `Script` cause for the same reason; the Theatre's `Replay` keeps its
   approvals. Person-only ops stay refused for agents. A person sees a consent card before first running a script
   another author wrote, or one naming `broadcast`, `publish`, `spend`, `net`, `fs` or `native` ops.
5. **One run, one transaction,** labelled with the script's name: one undo step, rolled back on any uncaught error,
   limit or cancel. From the console the host opens and commits it; for `script.run` the executor does, as for every
   driver job (ADR 0036). An agent may pass its open transaction to join it instead; a failed run then reverts its
   own edits, all or nothing, and on a conflict reverts none and reports them (ADR 0036 decision 10), leaving the
   rest to the transaction's owner. The run's result is the acknowledgement, made durable by
   the calling surface's `sync`; inner results belong to an open transaction and are not.
6. **Limits:** source 64 KiB; 10⁶ operations (10⁷ for jobs); 10 000 op calls; call depth 64; strings 1 MiB; output
   1 000 lines or 64 KiB; wall clock 10 s in the console and 120 s (at most 600 s) for jobs; cancel checked every
   operation. No sleep, no `import`, no nesting. In v1 a script calls `Edit` ops with automatic undo and `Query` ops
   only; `Job` and `Undo::Explicit` ops wait for the job manager (ADR 0036).
7. **`script.run` is a driver job (ADR 0036's `JobShape::Driver`) with a new scope, `script`,** held by no default agent ceiling; `script.save` needs it
   too, so an agent cannot plant a script for a person to run. `script.check` is a `Query` that parses and resolves
   ops and literal argument fields without running.
8. **Macros** record the person's own committed `Direct` transactions from `Runner::subscribe`, drop rolled-back and
   undone work, and coalesce only ops that declare themselves absolute setters (a new provided method
   `Op::coalesces() -> bool`, default `false`), and then only when the arguments differ only in numbers and both
   patches replace values at the same paths, keeping each source transaction for undo and redo; every other call is
   kept as recorded. They are stored as JSON lines
   of `{op, args}`, the step shape of eval fixtures. They live in My scripts by default and in the project on request,
   replay as one transaction, and become Rhai source through "Save as script". A macro becomes a library recipe only
   through the library's own save path and D3's rules.

## Consequences

- A contract change, `Scope::Script` in `omuzaki-ops-api` (serialised `script`, not in `Grants::agent_default()`),
  made by the job contract task together with the driver job shape, so the hub crate changes once.
- A second, additive contract change: the provided method `Op::coalesces() -> bool` (default `false`) and its value
  on `OpInfo`, best made in the same contract task; op crates then opt their setters in, each with a test.
- One executor change: script-caused requests are refused instead of queued.
- New dependency `rhai` 1.26.1 (MIT OR Apache-2.0, already cleared in register row B3); its task adds the version to
  `[workspace.dependencies]`. `mlua` is not used.
- `script.run` waits on the job manager and its driver shape (ADR 0036); until then the console and
  `omuzaki script run` run `ScriptHost` over the `Runner`. Macro replay by an agent or the automation graph uses the
  same driver shape.
- A dry run plans each call against the document as it was before the script, so it can differ from a real run whose
  later calls read earlier ones' writes. A runner fork would fix it (research among follow-ups).
- The task cut is design section 10.
- **Rejected:** Lua 5.4 and Luau (no static op list, a denylist sandbox, a C or C++ toolchain per target, no wasm32
  build); both languages (two generators, sandboxes and test suites); a transaction per inner call (no single undo
  step, a flush per call); syncing every inner call (10 000 flushes for a run whose open transaction a crash discards
  anyway); queueing scripts' approval needs; scripts acting as a separate script actor (undo and history are per
  actor, and the person must be able to undo their own run); `import` in v1.
