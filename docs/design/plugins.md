---
title: "Plugins: the WASM plugin host, its WIT contract, manifest, grants, sandbox, budgets and reload (PL-01)"
summary: "the WASM plugin host (ADR 0058): crates, the WIT package and its compatibility window, the generic op bridge, the plugin's session, ceiling and live revocation, the sandbox and its numbers, deterministic node instances, threads and the budget table, lifecycle, identity-keyed data and hot reload, the manifest and its runtime settings, rights and how plugins relate to scripts."
area: design
---

# Plugins: the WASM plugin host, its WIT contract, manifest, grants, sandbox, budgets and reload (PL-01)

**Status:** proposed. **Feature:** PL-01. **Decision record:** [ADR 0058](../decisions/0058-plugins.md). **Spec:** v3 §30
(PL-01, PL-03, PL-04), §6 (scopes), §9 (`omuzaki-plugin`, `omuzaki-plugin-api`), §10 (budgets), §39 rules 1, 2, 6, 10.
**Builds on:** ADR 0020 and `agent-sessions.md` (a plugin is an `Actor::Plugin` with a `Transport::Plugin` session),
`omuzaki-ops-api` (`Scope`, `Grants`, `ErrorCode`, `Request`), ADR 0015 (linking), ADR 0016 (user-supplied things:
licence and disclaimer), ADR 0042 (publisher keys), `l10n-ops.md` §3.1, `settings.md` D2, `automation.md` D5, D11 and
D15 (plugin nodes: engine thread, 1 ms fuel cap), `scripting.md` and ADR 0034 (PL-02), `connectors.md` D13 (one HTTP
contract), the licence register's wasmtime and extism row.

No plugin code exists. This page fixes the base contract every plugin is built against; the extension worlds (nodes,
trackers, exporters, part generators, adapters: PL-03) are the next design's and only their seam is fixed here.
**Note (ADR 0064):** they are designed in `plugin-extensions.md`, which settles the worlds, the instance kinds, threads
and budgets per extension, and the `command` world of 14.1.

The decisions in brief are ADR 0058's; this page gives the reasons and the numbers.

## 1. Crates and layering

| Crate | Kind | Holds | Depends on |
|---|---|---|---|
| `omuzaki-plugin-api` | API | `wit/plugin.wit` as `WIT_PLUGIN: &str` (and the files under `wit/`), `Manifest` with `parse` and `check`, `PluginId`, `PluginInfo`, `PluginState`, `PluginError`, `Requests`, `CallKind` and its `Budget` table, the `Plugins` service trait | `omuzaki-core`, `omuzaki-ops-api`, `omuzaki-store-api` (`ContentHash`), `omuzaki-library-api` (`Version`, `VersionReq`), `omuzaki-identity-api` (`KeyId`, `Domain::Plugin`, `verify`; §7, §11), `omuzaki-l10n-api` |
| `omuzaki-plugin-mock` | Mock | `MockPlugins` (in-memory `Plugins`), `MockGuest` (a plugin written as Rust closures against the same guest interface, no wasm), `conformance` | the API crates |
| `omuzaki-plugin` | Implementation | the component host (wasmtime 49, Cranelift), the WASI stubs (§4.1), the bridge, the compiled-code cache, the watcher, the `plugin.*` ops; `[package.metadata.omuzaki] wasm = false` (a JIT host has no place in a browser) | the API crates, `omuzaki-http-api` |

- **Manifests.** Each crate's `Cargo.toml` carries `[package.metadata.omuzaki]` with the `kind` above and a one-line
  `summary` (`cargo xtask lint-crate-table`); until the crates land, `crates/README.md` lists them as planned.
- **wasmtime, not extism.** Both are cleared in the register. extism's own ABI passes byte buffers through a plugin
  kernel and has no component model, so it gives no WIT, no typed `result`, and no versioned imports: the three
  things the compatibility window (1.1) rests on. wasmtime's component model gives all three, plus fuel, epochs and
  `StoreLimits`. The host task pins `wasmtime` and `wasmtime-wasi` to 49 and adds the version to the register row.
- **How Apps link the host (ADR 0015).** `omuzaki-plugin` registers the `plugin.*` ops, so it defines `link()`; the
  desktop App (`omuzaki-ui` through `omuzaki-stage`) and `omuzaki-cli` call `omuzaki_plugin::link()` and add a
  `Plugins` service to their `Services`. `omuzaki-mcp` never links it: plugin ops reach agents through the runner like
  any op. Plugins are not crates and never use `register_op!`; what they add at run time (nodes, event kinds) goes
  through the extension worlds' own run-time catalogues (automation D15's `DynNodeKind`).

### 1.1 The WIT package, its version and the compatibility window

The package is `omuzaki:plugin@0.1.0`. Each interface is versioned with it, and a component's imports name the
version it was built against, so the binary, not the manifest, is the authority.

- **0.x:** a host supports its own minor and the one before (a 0.2 host serves 0.2 and 0.1). A plugin built for 0.1
  on a 0.2 host loads through a host-side adapter that implements the 0.1 interfaces over 0.2's. On a 0.3 host it is
  refused: `Disabled { reason: Incompatible { built_for: "0.1", host: ["0.3", "0.2"] } }`, hint "this plugin needs an
  update". A minor may remove or change functions; patches only fix documentation.
- **1.0 and after:** semver. A 1.x host links every 1.y plugin with y ≤ x (wasmtime matches semver-compatible
  imports); a minor only adds functions or interfaces; a major gets the same one-back window as 0.x.
- **The manifest's `api`** (`"^0.1"`) must agree with the component's imports; a mismatch is `PluginError::Manifest`
  at install, so a wrong manifest can't hide what the binary imports.
- **One world per extension kind.** The base world is `plugin` (below). Each extension world (`node-plugin`,
  `adapter-plugin`, `tracker-plugin`, `exporter-plugin`, `part-plugin`, `command-plugin`; manifest keys `node`,
  `adapter`, `tracker`, `exporter`, `part`, `command`, per ADR 0064) `include`s it and adds its own exports, so every plugin speaks the same base
  interfaces and a host checks one world per kind. The manifest's `worlds` key (§7) says which a component targets.
- **Node activations never reach the base imports** (automation D15). The engine instance (§5.1) links `ops`, `data`
  and `http` to refusals (`denied`, detail "not in a node activation"); a node's only effect is the node world's
  `run-op`, which mirrors `NodeCx::run_op`: asynchronous, in the show's session, with the ceiling show session ∩ the
  plugin's ceiling ∩ the needs that show approved for that node. Its clock and randomness (`host.monotonic-seconds`
  included, which returns `cx.now()`) come only from `NodeCx` (§4.1), so a show stays deterministic (automation D6).

```wit
package omuzaki:plugin@0.1.0;

interface types {
    enum level { trace, debug, info, warn, error }
    enum error-code { invalid, not-found, denied, conflict, unsupported, unavailable, internal, waiting, limited, cancelled }
    /// `OpError` one to one: `detail` is `ErrorDetail` as JSON, `hint` the localised hint.
    record op-error { code: error-code, message: string, detail: option<string>, hint: option<string> }
}

interface ops {
    use types.{op-error};
    /// Runs an op (`Mode::Apply`). `args` and the result are JSON; the result is the op's output.
    call: func(op: string, args: string) -> result<string, op-error>;
    /// Dry-runs an op (`Mode::DryRun`): the `Plan` as JSON (patch, output, cost). Needs `read` only.
    plan: func(op: string, args: string) -> result<string, op-error>;
    /// The op's argument schema (JSON Schema), or none if no such op is registered.
    describe: func(op: string) -> option<string>;
    /// The names of the ops this plugin's grants could ever allow, sorted.
    list: func() -> list<string>;
}

interface host {
    use types.{level};
    log: func(level: level, message: string);
    /// Seconds on the runner's clock (`FakeClock` in tests), so a plugin's timing is deterministic under test.
    monotonic-seconds: func() -> float64;
}

interface data {
    /// The plugin's own folder: flat names `[a-z0-9._-]{1,64}`, no paths. Refusals unless the manifest says `data = true`.
    read: func(name: string) -> option<list<u8>>;
    write: func(name: string, bytes: list<u8>) -> result<_, string>;   // "over quota" past the byte or file quota (§4.2)
    remove: func(name: string) -> bool;
    list: func() -> list<string>;   // at most 4,096 names (§4.2)
}

interface http {
    record request { method: string, url: string, headers: list<tuple<string, string>>, body: list<u8> }
    record response { status: u16, headers: list<tuple<string, string>>, body: list<u8> }
    /// Only to approved hosts (§3.3). Errors: "host not approved", "timeout", "too large", or the transport's message.
    fetch: func(req: request) -> result<response, string>;
}

interface lifecycle {
    use types.{op-error};
    /// Called once per worker instance (ADR 0064: pure extension instances never run it), with the plugin's
    /// settings as JSON (§7). An error faults the plugin with that text.
    init: func(config: string) -> result<_, string>;
    /// A queued request (§2.2) was decided: the op's output, or why it did not run.
    approval-resolved: func(approval: u64, outcome: result<string, op-error>);
    shutdown: func();
}

world plugin {
    import ops;
    import host;
    import data;   // linked to refusals unless the manifest asks for `data`
    import http;   // linked to refusals unless the manifest asks for `net`
    export lifecycle;
}
```

The Rust guest SDK gives `approval-resolved` and `shutdown` empty defaults, so the smallest plugin writes `init` only.

## 2. The op bridge

### 2.1 Generic `call`, not a WIT function per op

- **A typed WIT function per op means a WIT file listing every op:** kept by hand, it is the central list §39 rule 2
  forbids; generated, it changes the package whenever any op's arguments change, voiding the window (1.1).
- **Op schemas are JSON Schema already** (spec §6). `describe` hands the same schema to a plugin that MCP hands to an
  agent, so the two can't drift, and a plugin can check its arguments before it calls.
- **Typing lives in the guest, where it is optional:** `omuzaki-plugin-sdk ops` writes typed Rust wrappers from a
  `describe` dump at build time, which still run against a host whose op gained an optional field.
- **Cost:** one JSON encode and decode per call. The budget (§5.2) is 20 µs round trip for 1 KiB of arguments, small
  next to the op itself; the bench gates it.
- `list` names only ops whose scope is inside the plugin's ceiling, so a plugin can't map ops it may never use.

### 2.2 Errors, dry runs, transactions, cause and undo

- **Errors** map `OpError` one to one (`op-error` above). The cases a plugin must handle:

| Code | When | What the plugin should do |
|---|---|---|
| `denied` | the scope is outside its ceiling (detail `MissingScope`), `person-only` op, no session | nothing: retrying never helps; the host logs one `warn` line |
| `waiting` | queued for a person (detail `Queued { approval, need }`), or the session is paused (`Paused`) | queued: wait for `approval-resolved`; paused: try later |
| `invalid` | arguments fail the schema or the op's checks | fix the arguments |
| `not-found` | no such op, or the op's target is missing | check `describe` |
| `limited` | a rate limit (detail `RetryAfter`), the host-call budget, or the plugin's CPU share (§5.2) | wait `seconds` |

- **Dry runs** are `plan`, which needs `read` only; an agent's dry-run rule (ADR 0020 7.1) is the same.
- **Every lifecycle export is one of two kinds.** A *transactional entry* (`init`, `approval-resolved`, the
  extension worlds' worker entries, and a `plugin.run` command, whose transaction the driver owns) runs as below.
  `shutdown` is the only other export, and it runs with **effectful imports denied**: the host ends the session and
  swaps `LiveGrants` to empty (3.4) before calling it, so `ops.call` and `plan` return `denied` (no session), and
  `data` and `fetch` return the revoked values of 3.4 (`err("revoked")`, `none`, `false`, an empty list). `log`, the
  clocks and randomness still work. A new export joins one of the two kinds in the world that adds it.
- **One transaction per entry call.** The host `begin`s a transaction labelled `<plugin name>: <entry>` before the
  first edit of a transactional entry, sends each
  `call` in it, and commits when the entry returns `ok`. An `err`, a trap or a breached deadline rolls it back. The
  plugin has no `begin`, `commit` or `undo` of its own: one entry is one step, which is what a person can follow in
  History. **Node activations are the exception:** they follow automation, where every call is its own one-op
  transaction (automation D5), because a show's steps are not undo units.
- **Cause:** `Cause { kind: Plugin, text: "<id>@<version> <entry>" }`, for example `example@0.1.0 init`. It names
  the entry, never arguments, so no plugin data lands in History labels. `CauseKind::Plugin` is a new variant (§11);
  reusing `Script` would make every plugin call refuse to queue (§10), and `Rule` belongs to automation.
- **Queued requests.** A request that waits for a person (`publish`, `broadcast`, `spend`, an op that calls
  `require_person`) is queued and runs, once approved, as its own transaction (ADR 0020 4.4); the entry call has
  long returned, so the host delivers the outcome later through `approval-resolved`. A request queued inside
  `plugin.run` keeps its caller's limit: the executor records the originating caller session and the intersection
  `caller's grants ∩ declared scope ∩ ceiling` with the pending approval, rechecks it against that session's live
  grants when the person approves, and withdraws the request (outcome `denied`) when the caller's session ends or its
  grants stop covering it. The `approval-resolved` entry that delivers the outcome runs under the same recorded
  intersection, rechecked live, never the plugin's whole ceiling (test `queued_command_call_keeps_caller_limit`).
- **Undo.** Each entry is one step; Ctrl+Z covers it and "Undo session" reverts the session (ADR 0020 5.4).

## 3. Identity and grants

### 3.1 The session

| Field | Value |
|---|---|
| `transport` | `Transport::Plugin` |
| actor | `Actor::Plugin { id: "<plugin>#<session>" }`, built by the executor like an agent's; the chip shows the plugin's name |
| `on_behalf_of` | the person who enabled the plugin on this machine |
| `client` | `plugin:<id>@<version>` |
| ceiling | §3.2 |
| `budget` | `usd_micros` 0 unless the person sets a cap; tokens and time uncapped |
| `limits` | the agent defaults (agent-sessions 8.2): edits 600 a minute (burst 100), runs 3,000 a minute (burst 300), 16 pending approvals, 1 open transaction |

It opens when the instance is started (enable, launch with the plugin enabled, restart after a fault, reload) and
ends with a named reason: `Closed` on disable, unload or reload, `Revoked` by the person, `ProjectClosed`, or
`Faulted { reason }` (new, §11) on a trap. Ending rolls back the open entry transaction (ADR 0020 4.2).

### 3.2 The ceiling

```
 ceiling = manifest requests ∩ the person's approval (this hash, this key) ∩ the person's grants (Principals)
```

- **Never granted to a plugin:** `native`, `fs`, `process`, `input`, `script`. A manifest that requests one is
  refused at install (`PluginError::Manifest`). `fs` is replaced by the plugin's own data folder (`data`), which is
  not a file root and reaches nothing else; `script` would let a plugin run Rhai as itself, a second door.
- **`net` is never in the session's grants.** It means "may use `http.fetch`" and is approved per host (3.3), so the
  plugin's only network path is the host function, never an op of scope `net` and never a socket.
- **`publish`, `broadcast` and `spend` are per use.** They can be in the ceiling, but for a `Transport::Plugin`
  session the executor queues every request whose op needs one as `Need::Person` even though the scope is granted,
  and offers only "Approve this request", never "for this session" or "for this project" (a one-line note in
  agent-sessions 7.1).
- **Approvals attach to the content hash and the publisher key** (agent-sessions 3.3: a name is chosen by its
  author). An update with a new hash signed by the same key keeps the approval for the scopes and hosts it already
  covers; anything new shows a card with only the difference. An unsigned plugin's approval covers its one hash.
- **Dev keys** (14.2): a person's own development key, which the build script signs with, never leaves their machine;
  an approval made against it covers that machine only, and anything published is signed with the publisher key.
- **A command runs inside its caller's grants.** `plugin.run` carries the caller's effective grants into the
  invocation; every inner `ops.call` and `plan`, queued ones included, is checked against `caller's grants ∩ the
  command's declared scope ∩ the plugin's ceiling`, as `script.run` is (`scripting.md` §3), and `list` shows that
  set. `fetch` also needs a declared `net` and the caller holding it; `data` writes need a declared `data`.
- **Changing a plugin's grants** is `plugin.grant`, person only (§9).

### 3.3 Network: hosts, not sockets

`net_hosts` names exact host names (`example.com`, `api.example.com:8443`; no wildcards, no IP literals except
`localhost:<port>`). Each is a line on the card with its own tick, like automation's `Need::Host` (D11). `fetch` goes
through `omuzaki-http-api`'s `HttpTransport` (connectors D13: https only, except `http://localhost:<port>`; no
redirects followed; `User-Agent: Omuzaki/<version> plugin/<id>`) with a 10 s timeout, a 1 MiB request body, a 4 MiB
response, and 4 requests a second (burst 8) per plugin. No secret is ever handed to a plugin: a plugin that needs an
account uses a connector, not `fetch`.

### 3.4 Revocation bites at once

`ops.call` and `plan` are checked by the executor against the live session on every call (ADR 0020). `http` and
`data` are host imports outside the executor, so the bridge gives them the same rule:

- **Every call checks the live grant.** The bridge holds the plugin's `LiveGrants` (session state, approved hosts,
  `data` on or off) behind one atomic pointer, swapped whole by every change. Each `fetch`, `read`, `write`, `remove`
  and `list` loads it on entry; there is no copy taken at `init` or cached per instance. A refused call returns
  `err("revoked")` (or `none`, `false`, an empty list) and adds one `warn` line.
- **What narrows it:** the person revoking the session (`Revoked`), `plugin.grant` removing a host or `data`,
  `plugin.disable`, `plugin.remove`, the project closing, the session ending for any other reason, and the person
  revoking a publisher key (identity.md D4: its trust-list entry becomes `revoked`). A revoked key disables every
  plugin it signed (`Disabled { reason: PublisherRevoked }`): each one's session ends `Revoked`, with its in-flight
  calls cancelled as below, and installs and updates signed by that key are refused (`PluginError::Signature`) until
  the person trusts it again.
- **Outstanding requests are cancelled.** Every `fetch` in flight registers its `CancelFlag` with the session. A
  narrowing swaps `LiveGrants` first, then sets the flag of every request the new grants no longer cover (all of them
  when the session ends); `HttpTransport` stops within 50 ms and the guest gets `err("revoked")`, never the response.
  A `data` call runs under the folder lock (§4.2), and the swap takes the same lock, so no write lands after the
  revoking op returns.
- **The running entry stops.** When the session ends, the host also moves the entry's deadline to now: the next
  import refuses and the next epoch traps, and the entry's transaction rolls back (3.1).

Tests (conformance, both hosts, on `FakeClock` with a loopback `HttpTransport` that holds each response until told):

| Test | Proves |
|---|---|
| `revoke_session_cancels_fetch_in_flight` | a `fetch` is waiting on the transport; the person revokes the session; the transport sees the cancel within 50 ms, the guest gets `err("revoked")` and none of the response bytes, and the entry rolls back |
| `grant_drops_host_mid_fetch` | `plugin.grant` removes `example.com` while a `fetch` to it waits: that request is cancelled, a concurrent `fetch` to a host still approved completes, the next `fetch` to `example.com` is "host not approved" |
| `disable_and_remove_cancel_fetch` | the same as the first, triggered by `plugin.disable` and by `plugin.remove` |
| `data_grant_removed_between_calls` | `write` succeeds; `plugin.grant { data: false }`; the very next `read`, `write`, `remove` and `list` are refused, and the folder is unchanged |
| `revoke_races_write` | 1,000 rounds of a `write` racing a revocation: each write either completed before the revoking op returned or was refused; no file changes after it returns |
| `revoked_publisher_key_disables_running_plugin` | two running plugins signed by one key, one with a `fetch` waiting; the person marks the key revoked; both are `Disabled { PublisherRevoked }`, both sessions end `Revoked`, the `fetch` is cancelled as in the first row, and installing or updating a package signed by that key is refused |
| `revoked_entry_stops` | an entry looping over `log` stops at the next import or epoch after the session is revoked, with its transaction rolled back |

## 4. The sandbox

### 4.1 WASI: a fixed stub subset that grants nothing

The base world imports no WASI. But Rust's standard library on `wasm32-wasip2` imports a few WASI interfaces even
for a plugin that never reads a file, so a host that links none would refuse every `std` plugin. The host links
exactly these, and refuses a component that imports any other (`PluginError::Imports { unexpected }`). The worker
instance and the node instance (§5.1) link them differently: a node instance is part of a show, and a show is
deterministic (automation D6), so it reaches no clock and no entropy except through `NodeCx`.

| Interface | Worker instance | Node instance |
|---|---|---|
| `wasi:cli/environment` | no variables, no arguments, cwd none | same |
| `wasi:cli/exit` | a trap: the plugin is `Faulted { reason: "exited" }` | a trap: the node faults |
| `wasi:cli/stdout`, `stderr` | written into the plugin's log at `debug`, under the same caps as `log` (§7); a write never blocks | same |
| `wasi:cli/stdin` | an empty stream, always ready | same |
| `wasi:io/streams`, `error` | what the three streams above need | same |
| `wasi:io/poll` | deadline-aware: the host's own `poll` and `pollable.block` wait at most until the entry's deadline (§4.3), and a wait still unready there traps "deadline in `<entry>`" | non-blocking: a pollable is ready or not at the step's time, which doesn't move during an activation, so a `poll` or `block` on an unready set traps "would block in a node activation" at once |
| `wasi:clocks/monotonic-clock` | the runner's clock, as `monotonic-seconds` | `cx.now()`, the step's show time; `subscribe-*` pollables are ready when their instant ≤ that time |
| `wasi:clocks/wall-clock` | the runner's clock since 1970-01-01 at session start, so tests stay deterministic | `cx.utc()`, the step's wall time, which D6 pins as part of the item; never the OS clock |
| `wasi:random/random` | OS entropy: it backs the guest's `getrandom`, so keys and nonces must not be predictable | `cx.rng()`, the node's own stream (D6: the show seed forked by `"<graph>/<node>"`); never OS entropy |
| `wasi:random/insecure`, `insecure-seed` | a ChaCha stream seeded from the content hash and the session id (deterministic, ADR 0009); tests also seed `random` through a test-only host option | `insecure` draws from `cx.rng()`; `insecure-seed` is 128 bits drawn from the same node stream (the show seed forked by `"<graph>/<node>"`, §5.1), so two nodes of one plugin get different seeds |
| `wasi:filesystem/types`, `preopens` | no preopens: every file call fails cleanly; files go through `data` | same |

`wasi:sockets` and `wasi:http` are never linked.

- **Why poll is deadline-aware in the worker and non-blocking in nodes.** `std::thread::sleep` is a monotonic-clock
  `subscribe-duration` and a `poll`, and a worker may wait inside its deadline, so the host implements `poll` itself
  and caps every wait at the entry's deadline: the wait sits in a host import, where an epoch is never seen (§4.3),
  so only a cap in the import stops it. A node activation has 2 ms and a frozen clock: any wait is either a
  deadlock or a read of time the show doesn't control, so it is refused outright and the engine thread never blocks.
- **Random requests are capped before the host allocates.** `get-random-bytes(len)` and `get-insecure-random-bytes(len)`
  build the list on the host before it is lowered into guest memory, where `StoreLimits` would catch it, so a `len`
  over 4,096 bytes traps "random request too large" before any allocation, in every instance kind. Rust's `getrandom`
  asks for 8 to 32 bytes at a time; a guest that wants more calls again, each call paying its fuel (§4.3).
- **Seeded randomness in a node is not secret.** A node instance has no `http`, `data` or `ops` (1.1), so a
  predictable value has nowhere to leak; a plugin that needs secrets does that work in its worker.

### 4.2 Limits on the instance

| Limit | Value | Enforced by |
|---|---|---|
| Component file | 16 MiB | the installer, before compiling |
| Linear memory | 64 MiB per instance, one memory | `StoreLimits` (`memory_size`); `memory.grow` past it fails, and the usual guest abort traps |
| Tables | 1 table, 10,000 elements | `StoreLimits` (`table_elements`, `tables`) |
| Instances | 32 core instances per component | `StoreLimits` (`instances`) |
| Wasm stack | 512 KiB | `Config::max_wasm_stack`; deep recursion traps |
| Host-call rate | 10,000 a second, burst 1,000, across every import | a token bucket on the runner's clock, like #244's; over it, `call` and `plan` return `limited` and other imports trap |
| Data folder | `data_quota_mb` bytes and 4,096 files (flat names); a `write` past either is "over quota" | `data`: both checked and updated under one lock; `list` is bounded by the count |
| Export lists, op bytes | `list()` results and `describe` strings over 1 MiB, an `init` config over 64 KiB; `ops.call` arguments and `plan` input over 1 MiB (before decoding) and results over 1 MiB (before returning) are refused with a fault | the bridge; a bad or oversized return value faults the plugin |

### 4.3 Fuel and deadlines per call kind

Fuel counts work deterministically (wasmtime's fuel is close to one unit per instruction), so the same plugin on the
same input stops at the same point on every machine. An epoch deadline is the wall-clock backstop; it ticks every
1 ms on a host timer thread. **Fuel counts host work too:** each import charges a fixed 2,000 fuel (about 2 µs on
`R`) plus 1 per byte passed either way, so the caps and the CPU share (§5.2) see host calls deterministically; the
bench fails if a charge falls below that import's measured p95.

An epoch is only seen when control is back in wasm, so **blocking host calls carry the deadline themselves**: the
bridge refuses every import once the entry's deadline has passed; `fetch` runs with timeout min(10 s, time left) and a
`CancelFlag` the timer thread sets at the deadline (`HttpTransport` stops within 50 ms); `wasi:io/poll` waits at most
until the deadline (§4.1); an `ops.call` can't be cancelled mid-op, so it is checked on return, and the overrun is at
most one op, which its own budget bounds. Node activations make no blocking call: `run-op` only enqueues (1.1), and
`poll` never waits (§4.1). The same `CancelFlag` is set by revocation (3.4).

| Call kind | Fuel | Epoch deadline | On breach |
|---|---|---|---|
| `init` | 10⁹ (about 1 s on `R`) | 5 s | fault |
| `approval-resolved`, worker entries of extension worlds | 10⁹ | 30 s (fetch time counts) | fault |
| `shutdown` | 10⁸ | 1 s | the instance is dropped; logged, not a fault |
| node activation (engine thread, PL-03) | 10⁶ (about 1 ms on `R`; automation D15) | 2 ms | the node faults (automation D5); 3 in a minute faults the plugin |

The fuel per millisecond is a measured constant: the bench (§12) runs a calibration loop on `Dev`, scales it to `R`
by the §10 single-thread ratio (1.43), and fails if 10⁶ fuel is outside 0.5 to 1.5 ms on `R`.

### 4.4 Traps, faults and restarts

- **A trap** (out of fuel, deadline, out-of-bounds, `unreachable`, stack overflow, a guest panic, a bad return value)
  drops the instance, sets `PluginState::Faulted { reason, at }` and ends the session with `Faulted { reason }`. The
  reason names the trap kind and the entry: "out of fuel in init".
- **Restart:** the next time it is needed (the next node activation, or straight away for a plugin with no extension
  world), with a backoff of 1 s, then 10 s, then 60 s.
- **Auto-disable:** 3 faults within 10 minutes set `Disabled { reason: Faults }`. The manager shows the last three
  reasons; only the person turns it back on.

## 5. Threads and budgets (§10)

### 5.1 Where plugin code runs

- **The plugin worker:** one thread per running plugin (`plugin:<id>`), owning its `Store`. Every entry call runs
  there, in order; a queue of 64 waiting calls, beyond which new ones are refused `limited`.
- **The automation engine thread** runs node activations only (PL-03, automation D15), in a second instance of the
  same compiled component with its own `Store` and a 16 MiB memory cap. They share only the compiled code: a node
  activation acts in the show's session, never the plugin's, and has no `ops`, `http` or `data` (1.1; D15).
- **Node instances are deterministic.** A node gets its clock and randomness only through `NodeCx`: show time, the
  step's wall time and the node's seeded stream (§4.1); OS entropy and the OS clocks are not linked into a node
  instance at all, so the same show, seed and items give the same effects (automation D6). Fuel is deterministic too,
  so a node that runs out does so at the same point in both runs. Only the 2 ms epoch backstop depends on the
  machine; it fires on a machine far slower than `R`, and the node then faults (D5), which the run reports.
- **Never on the render, audio or tracking thread.** ADR 0064 adds no exception: trackers run on their own
  `plugin-track:<id>` thread, exporters in a job, part generators on the character builder's thread, each with its
  number and gate (`plugin-extensions.md` §3). Compiling runs on the worker, not the UI thread.

### 5.2 The budget table

Measured on `R` unless the row says otherwise; the plugin bench (§12) runs nightly on `Dev` and scales by 1.43.

| Path | Budget | Statistic | Gate | On breach |
|---|---|---|---|---|
| Install (`plugin.install`'s `work`, off the document lock): hash, signature check, manifest check, copy, compile of a 2 MiB component | ≤ 1.5 s | worst of 5 | bench `install_2mib`, nightly | reported; nothing stops |
| Load from the compiled-code cache, to `init` called | ≤ 20 ms | p95 | bench `load_cached`, nightly | reported |
| `init` | 10⁹ fuel, 5 s | per call | §4.3, conformance test | fault |
| Host-call overhead, `call` round trip with 1 KiB of JSON each way, excluding the op | ≤ 20 µs | p95 | bench `call_1kib`, nightly; same-session A/B on bridge PRs | reported |
| `log`, `monotonic-seconds` | ≤ 2 µs, ≤ 0.5 µs | p95 | bench `host_small` | reported |
| A node activation (engine thread) | 10⁶ fuel ≈ 1 ms, 2 ms deadline | per activation | §4.3, calibration test | node fault |
| A plugin's CPU share, all its calls | ≤ 100 ms of work a second (10 % of one core, 1.25 % of `cpu.total`'s 8 cores) | per second, fuel-counted, host-call charges included (§4.3) | conformance test on `FakeClock` | an entry call's fuel is the smaller of its cap and the remaining budget, so a long call is interrupted at the share; later calls `limited` until the next second |
| Memory per plugin | 64 MiB worker + 16 MiB engine instance + ≤ 8 MiB host side (code, store) | max | conformance test (`memory.grow` past the cap fails) | trap: fault |
| Memory, all plugins | ≤ 256 MiB in the editor (of `ram.editor`'s 1.5 GB), ≤ 128 MiB in performance mode (of `ram.runtime`'s 400 MB; provisional, 14.3) | linear memory in use, summed | the host, on every `memory.grow` | that grow fails (the plugin faults); the manager names the largest plugins |
| Reload: drain and swap | ≤ the running call's deadline + 1 s shutdown + load | per reload | conformance test | the old instance is dropped |

The §10 budget scene has no plugins, so these rows add to it and never move its numbers; the Performance HUD shows
plugin CPU and memory as their own line.

## 6. Lifecycle

### 6.1 Where plugins live

- **Installed packages:** `<local data dir>/plugins/<id>/<hash>/` (the settings design's machine directory: code and
  approvals are this machine's, so they never roam). The last two versions are kept for rollback.
- **Data folders belong to an identity, not a name** (6.4): `<local data dir>/plugin-data/<owner>/`, where `<owner>`
  is the installation's `DataOwner`, never the author-chosen id. Its versions share it.
- **Compiled code cache:** `<cache dir>/plugins/<hash>-<wasmtime version>-<cpu features>.cwasm`, a cache only: a
  mismatch or a bad file recompiles.
- **Enabled:** the `Machine` setting `plugins.enabled` (`AgentWrite::Never`), each entry "on for every project" or "on
  for project `<real path>`". A project from a friend, a template or a git pull can't turn on code: a project only
  says which plugins its nodes use, and a missing or disabled one leaves those nodes inert with a finding (automation
  D15).

### 6.2 States

`PluginState`: `Installed` (on disk, off), `Starting`, `Running`, `Faulted { reason, at }`, `Disabled { reason }`
(`Person`, `Faults`, `Incompatible`, `PublisherRevoked`), `Reloading`. **Nothing loads at launch that the person has not enabled**
(SE-04): a disabled plugin's manifest is read for the list, its component is never compiled.

### 6.3 Update, reload and removal

- **Install is a job** (`OpKind::Job`, ADR 0036). `plan` checks the path and the id under the lock and changes
  nothing. `work` runs on a `Cpu` job thread without the lock: extraction into staging, hashing, the signature check,
  the manifest check, the copy into `plugins/<id>/<hash>/` and the component compile into the cache, with a
  `JobCtx::checkpoint` between steps. `finish` takes the lock only to commit the registration (the installed record and
  the approval card). It is `Undo::None`, confirmed first like `plugin.remove`: the copied package, the compiled code
  and the installed record are outside the document, so no inverse patch removes them; the dialog says the way back is
  `plugin.remove`. Its time cap and cancellation are ADR 0036's (decisions 3 and 4): a cancelled
  or capped install deletes its staging and copied files, registers nothing, and leaves the cache only what it can
  reuse.
- **Update** installs the new hash beside the old, carries approvals per 3.2, and swaps with a reload.
- **Hot reload** (`plugin.reload`, or the watcher of a plugin installed with `watch`, which re-reads its source folder): the trigger is a changed
  content hash, never a timestamp. It is a job (`OpKind::Job`): `plan` checks under the lock and changes nothing;
  `work`, on the plugin's worker thread and off the lock, does the drain and the instance work: new entry calls
  queue; the running one finishes or meets its deadline; the old session ends `Closed`; `shutdown` runs (1 s, with
  effectful imports denied, 2.2); the new instance starts with a new session and `init`. `finish` takes the lock
  only to flip the record's active hash. It is `Undo::None`: the person confirms a manual reload, and the install
  dialog of a `watch` plugin says that changes in its folder reload it without asking. Node instances
  swap between activations. **No state is handed over:** a hand-off needs a format both versions agree on, and a
  plugin that wants to keep something writes it to its data folder, which survives.
- **Removal** (`plugin.remove {id}`) disables, ends the session (3.4) and deletes the packages; the data folder stays
  under its owner (6.4). Under the lock it only ends the session, marks the record removed and renames
  `plugins/<id>/` to a tombstone; dropping the instance and deleting the files run as a job off the lock, and
  start-up sweeps tombstones as for data (below). Its approvals and settings values are dropped; History keeps its steps under its actor. It
  is `Undo::None`, confirmed first like `plugin.remove_data`: deleted package files, approvals and settings are
  outside the document, so no inverse patch restores them. The dialog says so: reinstalling the package restores the
  plugin, and its approvals are asked for again on the card.
- **Deleting the data** (`plugin.remove_data {owner}`) deletes a data folder for good. It is `Undo::None`: the bytes
  are gone and no inverse patch brings them back, so the person confirms it first, on a dialog that names the plugin,
  the publisher and the folder's size. A data folder whose plugin is still installed is refused (remove it first).
  Under the lock the op only renames `plugin-data/<owner>/` to `plugin-data/.tombstone-<owner>-<nonce>/` (one atomic
  rename on the same volume), after which the folder is gone from `kept_data` and no import resolves it. The recursive
  delete then runs as a `Cpu` job off the lock: a folder can hold up to 1 GiB in 4,096 files (§4.2), far too long to
  hold the edit lock for. Start-up sweeps any `.tombstone-*` folder a crash left behind, with the same job.
  The manager's "Remove and delete its data" runs the two ops in that order, behind the one confirmation. Deleting
  the data stays its own op so that removal keeps the data unless the person asks otherwise.

### 6.4 Data ownership and migration

- **The owner is the authenticated publisher plus the installation.** At first install the host mints a
  `DataOwner { publisher, installation }`: `publisher` is the key that verified the package's signature (`dev:` and
  the dev key for 3.2's development builds, `unsigned` for an unsigned plugin), and `installation` is a random 128-bit
  id. `<owner>` on disk is `<publisher fingerprint or "unsigned">-<installation>`. The plugin's id is a display label
  on the record, never part of the key.
- **Updates keep the owner** only when the new package is verified by the same publisher key; an unsigned plugin's
  update (a new hash, so a new approval, 3.2) gets a new owner, as does any update whose key differs.
- **A different identity never sees the data.** A plugin installed later with the same id from another publisher, or
  a reinstall of the same plugin after removal, gets a fresh owner and an empty folder. The `data` imports resolve
  only the current installation's owner; there is no lookup by id.
- **Moving data between identities needs the person's consent.** `plugin.migrate_data {from, to}` (person only,
  never queued, `Undo::None`, confirmed) moves a kept folder to an installed plugin's owner with one atomic rename
  under the lock, refused if the target's folder holds any file. It is not `Undo::Explicit`: moving back after the
  target plugin has written files would overwrite or strand them, so the way back is a second migration the person
  starts. The manager
  offers it on the install card only when the new installation has the same publisher key as the kept folder ("Use the
  data kept from Example by Example, 12 MiB?", default off); a folder from another publisher is moved only from the
  kept-data list, where the dialog names both publishers. Nothing moves without that click.
- **Tests:** `data_owner_is_identity` (remove with data kept, install a plugin with the same id signed by another key:
  its `list` is empty and the old folder is untouched); `reinstall_starts_empty_until_migrated` (same key: empty until
  `plugin.migrate_data`, then the old files); `update_same_key_keeps_data`, `update_other_key_gets_new_owner`;
  `migrate_is_person_only` (an agent or plugin session gets `person_only`; the op is `Undo::None` and refuses a
  target folder with files); `remove_data_is_undo_none` (the op's
  `OpMeta::undo` is `None`, it refuses an installed plugin's folder, and History shows no undo for it);
  `remove_data_tombstones_under_lock` (the lock is held only for the rename; the delete runs as a job; a tombstone
  left at start-up is swept).

## 7. The manifest

```toml
id = "example"                 # ^[a-z][a-z0-9_]{1,31}$, no reserved word (below)
version = "0.1.0"              # semver
api = "^0.1"                   # must agree with the component's imports (1.1)
licence = "MIT"                # an SPDX expression
name = "Example"               # fallback for the Fluent `plugin-example-name`
description = "Waves when the project opens."
worlds = ["plugin"]            # the base world; PL-03's keys and sections: plugin-extensions.md §4.7
component = "plugin.wasm"
publisher = { name = "Example", key = "ed25519:<fingerprint>" }   # optional: unsigned plugins omit it

[requests]
scopes = ["read", "perform", "publish", "net"]
net_hosts = ["example.com"]
data = true                    # its own folder only
data_quota_mb = 64             # 1 to 1024; default 64

[[settings]]
key = "greeting"               # stored as plugin.example.greeting
type = "string"                # string, bool, integer, number, or a JSON Schema in `schema`
default = "hello"
scope = "user"                 # settings.md D2: user or machine
agent = "confirm"              # free, confirm or never; default never
```

- **Id rules:** the same slug as automation's `plugin.<plugin>.<name>` segment, so node and event ids never collide;
  reserved: `omuzaki`, `core`, `plugin`, `system`, and every first-party op family (`face`, `io`, ...), checked
  against the registry at install. Two plugins with one id can't both be installed.
- **Package:** a folder or a `.zip` of one: `plugin.toml`, the component, `l10n/<locale>/*.ftl`, `plugin.sig`. The
  content hash is BLAKE3 over every regular file except `plugin.sig`, fed as one exact preimage: each path is
  relative to the package root, UTF-8 in NFC with `/` separators, under the component path's rules below (no root,
  prefix, `.`, `..` or empty segment; folders are not entries); entries are sorted by path bytes; each is fed as a
  `u64` little-endian path length, the path bytes, a `u64` little-endian content length, then the contents. The
  install task's acceptance check `package_hash_golden` is a committed package (a nested path and a non-ASCII name
  among its files) whose digest is committed and must match on Linux, macOS and Windows CI. The signature is an ADR 0042
  envelope over that hash under a new `Domain::Plugin`, tag `omuzaki-plugin-v1\0` (ADR 0042's `omuzaki-<use>-v<N>\0`
  shape). `Domain` is `omuzaki-identity-api`'s closed enum, so the variant is a cross-area contract change: an ADR 0042
  amendment the Director approved (§11), never a tag the plugin crates define. **Extraction** is into a fresh staging folder and
  refuses the whole package on: an absolute path, a drive prefix or a `..` component; a symlink, hardlink or any entry
  that is not a regular file or folder; two entries equal after normalising (Unicode NFC, case-folded, `/` only); more
  than 256 entries or 32 MiB uncompressed. The hash is taken over the staged files, so what is hashed is what loads.
- **The component path:** `Manifest::check` refuses a `component` that is not a normalised relative path: no root, no
  drive prefix, no `..` or `.` component, no empty segment. Staging then resolves it inside the staged package and
  refuses it unless it is a regular file (not a symlink) whose canonical path lies under the staging root. The bytes
  the host compiles are therefore one of the hashed entries, covered by the content hash and the signature; nothing
  outside the package is ever read as code (test `component_path_stays_in_package`: `../x.wasm`, `/x.wasm`,
  `./a/../x.wasm`, `a//x.wasm` and a symlinked component are all refused).
- **Settings** (at most 64, as are commands) register at enable under `plugin.<id>.*` with the manifest's scope and
  agent rule (default `never`), and leave at disable; `init` receives them as JSON. `settings.md` D2's registry is
  static (`register_setting!` at link time), so this needs a **runtime settings registry** in the settings contract
  first: its own contract task, which the Director approves before the plugin settings work (#1571) builds on it.
  What that contract must provide:
  - `register(owner, prefix, defs) -> Result<RuntimeRegistration, SettingError>` and `unregister(registration)`, each
    one atomic snapshot swap and one `SettingsEvent` (new causes `Registered` and `Unregistered`), so a reader sees
    all of a plugin's keys or none; `forget(owner)` drops one installation's stored values (plugin removal, 6.3).
  - A def carries what a static `SettingMeta` does, as data: key, summary, scope (`User` or `Machine`), `AgentWrite`,
    `Apply::Live`, a JSON Schema and a default that passes it. `InProfile::Excluded` always, so a profile never holds a
    key that can vanish.
  - Namespaces: a runtime prefix must sit under one a static registration reserves (`plugin.` for the host); it can
    never shadow or extend a static key; two live registrations can't overlap; D2's prefix rule holds across both
    kinds; at most 64 keys a registration.
  - While registered, the keys behave as static ones: `get`, `list`, `set`, `reset`, the agent rules of D4, the Settings
    page and the agent's schema. After `unregister`, `get` and `set` fail `NotFound` and the keys leave `list`.
  - Stored values outlive `unregister` (D5's unknown keys are kept) and carry the registration's owner tag (for a
    plugin, its installation's `DataOwner` (6.4): publisher key plus installation id, so two plugins from one
    publisher never share values, and removal's `forget(owner)` drops only that plugin's); a registration with a
    different owner never reads them and starts at the defaults (6.4's rule for settings). A
    stored value the new schema rejects falls back to the default with a `SettingFinding`.
  - Conformance tests in the settings mock's suite: register and unregister round trip with one event each; a
    collision with a static key, a reserved prefix or another live registration is refused; values survive
    unregister and register by the same owner; a different owner gets defaults; an invalid stored value falls back
    with a finding; agent rules hold on runtime keys; a reader never sees half a registration; `forget` leaves no
    value behind; the 64-key cap.
- **Log lines:** `log` and stdout/stderr, at most 1 KiB each (longer is cut), 20 a second with a burst of 100, then
  one "N lines dropped" line. They appear in the Theatre and the plugin's page under its actor chip, never as raw
  stdout, and every refused call adds one `warn` line.
- **Fluent resources** load into the `Catalog` at enable and leave at disable; every id must start
  `plugin-<id>-` (`l10n-ops.md` §3.1), and a resource that defines another id is refused at install.

## 8. Rights

- **The approval card** shows the declared licence, the publisher (or "unsigned") and the line "Not from Omuzaki:
  this plugin is made by someone else; Omuzaki has not reviewed it", as ADR 0016 does for user-supplied models.
- **A non-commercial or unknown licence is shown, not refused,** when a person installs the plugin themselves: we do
  not ship it, and `policy.md` covers what we ship. The card says "Non-commercial licence: you may not be allowed to
  use what it makes commercially". The store (PL-04) refuses such plugins.
- **What a plugin's output carries:** every op record already names the plugin's actor; the licence record (VF-08)
  adds one row per plugin whose ops touched the exported thing (`plugin example@0.1.0, MIT, <hash>`), and export warns
  on a non-commercial or unknown one, as it does for models.

## 9. Ops

Two rules hold for every row:

- **Off the lock:** an op that drains or calls into a guest instance, or moves or deletes files, does that work off
  the edit lock, as a job or a step of the worker's lifecycle; under the lock it does at most an atomic rename or a
  flag or pointer flip.
- **Undo:** an op is `Undo::Explicit` only where its row names its paired idempotent undo and redo `OpCall`s and
  their conflict policy (ADR 0036); every other mutating op is `Undo::None` and confirmed first.

| Op | Kind | Scope | Who | What it does |
|---|---|---|---|---|
| `plugin.list {}` | Query | `read` | anyone, agents included | installed plugins: `PluginInfo` (id, version, hash, publisher, licence, state, grants, worlds) |
| `plugin.describe {id}` | Query | `read` | anyone | one plugin: manifest, requests, approvals, last faults, memory and CPU |
| `plugin.install {path, watch?}` | Job (`OpKind::Job`, `Undo::None`, confirmed first) | `edit:settings` | person only | checks and copies a package off the lock, then registers it (6.3); shows the card; does not enable; the way back is `plugin.remove` |
| `plugin.enable {id, project?}` / `plugin.disable {id, project?}` | Edit (`Undo::Explicit`: each is the other's undo with the same `{id, project}`; both set a value, so they are idempotent; an undo whose plugin is no longer installed is skipped and reported) | `edit:settings` | person only | flips `plugins.enabled` under the lock; the worker starts or stops the instance off the lock (6.3's drain and `shutdown`) |
| `plugin.grant {id, scopes?, hosts?, data?}` | Edit (`Undo::Explicit`: undo and redo are `plugin.grant` with the whole approval recorded before and after, so idempotent; an undo whose current approval is not the recorded "after" is skipped and reported) | `edit:settings` | person only | swaps the approval and `LiveGrants` under the lock, never beyond the requests; cancellations (3.4) follow off the lock |
| `plugin.reload {id}` | Job (`OpKind::Job`, `Undo::None`, confirmed first) | `edit:settings` | person only | drain, `shutdown` and `init` in `work` off the lock; `finish` flips the active hash (6.3) |
| `plugin.remove {id}` | Edit (`Undo::None`, confirmed first) | `edit:settings` | person only | 6.3: ends the session and renames the package folder to a tombstone under the lock; a job drops the instance and deletes it; the data folder stays; reinstalling restores the package, and approvals are asked again |
| `plugin.remove_data {owner}` | Edit (`Undo::None`, confirmed first) | `edit:settings` | person only | renames a removed plugin's data folder to a tombstone under the lock; a job deletes it (6.3) |
| `plugin.migrate_data {from, to}` | Edit (`Undo::None`, confirmed first) | `edit:settings` | person only | one atomic rename of a kept data folder to an installed plugin's owner (6.4) |
| `plugin.run {id, command, args}` | Driver job (as `script.run`) | `read`; the driver checks the command's declared scope first | anyone holding that scope | runs a plugin command (14.1) inside the caller's grants (3.2); the guest runs off the document lock and its inner calls go through `JobCtx::call` in one executor-owned transaction |

The management ops (install, enable, disable, grant, reload, remove, remove_data, migrate_data) return
`OpError::person_only` to any agent or plugin and are **never queued**: an agent that installs, enables or grants a
plugin hands its own work to an actor whose grants it chose, which is escalating its own privileges with extra steps. `plugin.run` is not one: before
starting the guest the driver calls `JobCtx::authorize(scope)` (additive, `omuzaki-ops-api`) for the command's declared scope and refuses if the caller's grants don't cover it.

## 10. Scripts (PL-02) and plugins

**One grants model and one journal.** Both are surfaces over the same `Runner`; scopes, `Grants::intersect`,
ceilings, `OpRecord`s, `Cause` and History are shared, and nothing is plugin-only in the executor except the
per-use rule of 3.2. They differ where the two kinds of code differ:

| | Scripts (`scripting.md`) | Plugins | Why |
|---|---|---|---|
| Actor | the caller (person, or the agent's session) | its own `Actor::Plugin` and session | a script runs because someone ran it now; a plugin is someone else's code acting later, unattended, so it needs its own identity, ceiling and kill switch |
| Ceiling | caller's ∩ the scopes the script names | manifest ∩ approval ∩ person | a script's ops are visible in its syntax tree; a component's calls are strings decided at run time, so the manifest declares them |
| Queueing | never: `Denied` instead | queues; the outcome comes back through `approval-resolved` | a script's run has ended when the approval lands; a plugin is still there to hear it |
| Transaction | one per run | one per entry call; none for node activations | the same rule at each kind's unit of intent |
| Limits | Rhai operations (10⁶ console, 10⁷ job), 10 s or 120 s wall clock | wasm fuel, epoch deadlines (§4.3) | each engine's own deterministic counter |
| Consent | per (project, script hash) card | per (hash, publisher key) card | both pin approval to content, never to a name |

**What decides:** ADR 0020's executor; neither has a private door, so only the rows above may differ, and a change to
one of them amends this page.

## 11. Contract

**`omuzaki-plugin-api`** (new, its own task): the WIT files of §1.1, `PluginId` (the slug of §7), `Manifest`
(the fields of §7, `parse`, `check` returning every finding), `Requests`, `PluginInfo`, `PluginState`,
`DisabledReason`, `PluginError` (`Manifest`, `Imports`, `Signature`, `Incompatible`, `Io`, `Faulted`), `CallKind`
with `budget() -> Budget { fuel, deadline_ms }` holding §4.3's table, and:

```rust
pub trait Plugins: Send + Sync {
    fn list(&self) -> Vec<PluginInfo>;
    fn info(&self, id: &PluginId) -> Option<PluginInfo>;
    fn install(&self, package: &Path) -> Result<PluginInfo, PluginError>;
    fn set_enabled(&self, id: &PluginId, on: Enablement) -> Result<PluginInfo, PluginError>;
    fn set_grants(&self, id: &PluginId, approval: Approval) -> Result<PluginInfo, PluginError>;
    fn reload(&self, id: &PluginId) -> Result<PluginInfo, PluginError>;
    fn remove(&self, id: &PluginId) -> Result<(), PluginError>;
    /// Kept data folders, with the publisher and id they were made under and their size.
    fn kept_data(&self) -> Vec<KeptData>;
    fn remove_data(&self, owner: &DataOwner) -> Result<(), PluginError>;
    fn migrate_data(&self, from: &DataOwner, to: &PluginId) -> Result<(), PluginError>;
}
```

`DataOwner { publisher: Publisher, installation: InstallationId }` (6.4) and `KeptData` join the types above.

**`omuzaki-identity-api`** (a cross-area contract change: an ADR 0042 amendment, approved by the Director on
2026-10-15; its own contract task, landing before the install work signs or verifies anything): `Domain::Plugin`, tag
`omuzaki-plugin-v1\0`, added to `Domain::ALL`, with test vectors for the new tag and a test that no two domain tags
are prefixes of each other (ADR 0042's tag tests). The plugin crates only call `verify(Domain::Plugin, …)`.

**`omuzaki-settings-api`** (its own contract task, approved by the Director before #1571): the runtime settings
registry of §7.

**`omuzaki-ops-api`** (approved by the Runtime lead on #1537): `CauseKind::Plugin` and `EndReason::Faulted { reason:
String }`. One task adds them with `CauseKind::Review` (#1535), so the hub file changes once, and marks both enums
`#[non_exhaustive]` (ADR 0054 §4). Nothing else changes: `Actor::Plugin`, `Transport::Plugin` and the session types
exist.

## 12. Tests

- **The conformance suite** (`omuzaki-plugin-mock::conformance`) runs against `MockPlugins` with `MockGuest` and
  against the wasmtime host with WAT components, on `FakeClock`: lifecycle and states, the ceiling intersection,
  every error-code mapping, per-use queueing and `approval-resolved`, a trap ending the session `Faulted`, auto-disable
  at the third fault, reload draining a running call, settings and Fluent resources added and removed, the revocation
  tests of 3.4 and the data-ownership tests of 6.4.
- **Replay tests** (automation D6), in `crates/omuzaki-plugin/tests/` with `NodeHarness` (`omuzaki-live-mock`, a
  dev-dependency) driving the wasmtime node instance through `DynNodeKind`, and a WAT node plugin that reads every
  clock and random import (`monotonic-seconds`, both WASI clocks, `get-random-bytes`, `insecure`, `insecure-seed`)
  and passes what it read to `run-op` as arguments:
  - `node_plugin_replay_identical`: one seed and item script run twice gives byte-identical `log_json`, the
    run-op arguments included; between the runs the host's OS clock and entropy source are advanced and reseeded.
  - `node_plugin_seed_changes_draws`: a different show seed changes the random arguments and nothing else.
  - `node_plugin_stream_is_per_node`: under one seed, two nodes of one plugin at two `<graph>/<node>` paths draw
    different values from `get-random-bytes`, `insecure` and `insecure-seed`, each equal to what D6's
    `Rng::new(seed).fork(stream)` gives for its path, so adding a node never shifts another's draws.
  - `node_instance_never_reaches_os`: the node linker is built with OS clock and entropy sources that panic, and the
    script runs.
  - `node_poll_never_blocks`: a `poll` on a timer later than the step's time traps "would block in a node activation"
    within the activation, and the node faults.
- **Hostile plugins**, each a WAT component in `crates/omuzaki-plugin/tests/hostile/`, each with its expected result:

| Plugin | Expected |
|---|---|
| infinite loop in `init`; a command call that would run 500 ms of fuel | out of fuel at 10⁹, `Faulted { "out of fuel in init" }`, session `Faulted`; the call's fuel is the smaller of its cap and the remaining share, so it is interrupted at 100 ms, not only refused next time |
| loop around a slow host call | epoch deadline at 5 s |
| `memory.grow` to 1 GiB | grow fails at 64 MiB; the guest's abort traps |
| deep recursion | stack overflow trap at 512 KiB |
| host-call flood (10⁶ `log` calls) | 100 lines then "N lines dropped"; the bucket traps at 10,000 a second |
| `call` flood | `limited` with `RetryAfter`; the person's edits are never slowed |
| bad return (invalid UTF-8, a `list` of 10⁶ names); `ops.call` with 2 MiB of arguments, or an op result over 1 MiB | fault "bad return value"; refused with a fault before decoding or before the result is returned |
| trap at start | `Faulted`; restart after 1 s, 10 s; disabled at the third |
| imports `wasi:sockets` | refused at install, `Imports { unexpected: ["wasi:sockets/tcp"] }` |
| `fetch` to a host not approved | `err("host not approved")`, one `warn` line |
| `read`-only agent runs a `read` command that calls `edit:scene` | refused `MissingScope`, nothing changed, one `warn` line |
| 10⁶ zero-length `data.write`s | the 4,097th is "over quota"; `list` returns at most 4,096 names |
| `fetch` that outlives `init`'s 5 s | cancelled at the deadline, fault "deadline in init" |
| `subscribe-duration(60 s)` then `poll` in `init` | the wait ends at the 5 s deadline, fault "deadline in init"; the worker thread is free again |
| `get-random-bytes(2³²)`, and `get-insecure-random-bytes` the same, in a worker and in a node instance | trap "random request too large" before the host allocates (peak host memory unchanged) |
| `.zip` with `../x`, a symlink, or `A.wasm` beside `a.wasm` | refused at install, nothing written outside staging |

- **Bench** `crates/omuzaki-plugin/benches/call.rs`: `call_1kib`, `host_small`, `load_cached`, `install_2mib` and the
  fuel calibration, with the gates of §5.2.
- **Authoring path.** Guests are Rust with `wit-bindgen` on `wasm32-wasip2`. CI needs no such toolchain: tests use
  WAT, and the example plugin is built by `tools/plugins/build-example.sh` and committed (under 200 KB) with its
  source, so a test checks it still loads. The docs page is `docs/plugins/authoring.md` (the SDK task).
- **The web build:** `omuzaki-plugin` is native only: no `plugin.*` op, no plugin, and plugin nodes show inert with
  the finding "plugins run in the desktop studio".

## 13. Worked example

On disk, after install and enable:

```
<local data dir>/plugins/example/9f2c…e1/{plugin.toml, plugin.wasm}   # the manifest of §7; 41 KiB
machine.toml: [plugins] enabled = [{ id = "example", on = "all" }]
```

The plugin, in Rust (`wit-bindgen`):

```rust
impl Guest for Example {
    fn init(_config: String) -> Result<(), String> {
        let project = ops::call("project.describe", "{}").map_err(|e| e.message)?;
        host::log(Level::Info, &format!("opened {} bytes of project", project.len()));
        ops::call("perform.set_expression", r#"{"expression":"happy","weight":1.0}"#).map_err(|e| e.message)?;
        if let Err(e) = ops::call("face.set_profile", r#"{"profile":"oval"}"#) { host::log(Level::Warn, &e.message); }
        if let Err(e) = ops::call("publish.share_link", "{}") { host::log(Level::Info, &e.message); }
        Ok(())
    }
}
```

`perform.set_expression` and `publish.share_link` stand in for the performance and sharing ops, which have no
names yet; `face.set_profile` is the real op (scope `edit:character`, not requested, so it is denied).

The card: **Example 0.1.0, by Example (key ed25519:4b1f…), MIT. Not from Omuzaki.** Wants to: read the project
[✓]; move the avatar (`perform`) [✓]; publish, asking you each time [✓]; reach `example.com` [✓]; keep its own
files, up to 64 MiB [✓]. `[Turn on]` `[Not now]`. Effective session grants: `read`, `perform`, `publish` (per use).

The session's journal records (kind, payload abridged) and log lines, for one `init` and a trap in the next start:

```
audit  {"fact":"session_opened","session":7,"actor":"plugin example#7","for":"person","grants":["read","perform","publish"]}
begin  {"tx":41,"actor":"plugin example#7","label":"Example: init"}
op     {"op":"perform.set_expression","actor":"plugin example#7","cause":{"kind":"plugin","text":"example@0.1.0 init"},"tx":41}
log    warn  plugin example#7  face.set_profile denied: needs edit:character (MissingScope)       # Denied: no journal record
audit  {"fact":"approval_requested","approval":12,"session":7,"op":"publish.share_link","need":"person"}
log    info  plugin example#7  waiting for a person (approval 12)                                  # Waiting
commit {"tx":41}
audit  {"fact":"session_ended","session":7,"reason":"closed"}                                    # disabled; 0.1.1 installed, enabled
audit  {"fact":"session_opened","session":8,"actor":"plugin example#8","for":"person","grants":["read","perform","publish"]}
begin  {"tx":42,"actor":"plugin example#8","label":"Example: init"}
audit  {"fact":"session_ended","session":8,"reason":{"faulted":"out of fuel in init"},"abandoned":[42]}   # 0.1.1 loops in init
```

A refused call has no journal record; the `warn` line is the trace.

## 14. Decided questions

Ruled by the Runtime lead on #1537.

1. **Plugin-defined ops: one static op, `plugin.run {id, command, args}`.** The op registry stays static; there is
   no run-time op table. Each command's schema reaches scripts, agents and MCP through `describe`, and its scope is
   declared in the manifest; the caller needs that scope, and the command runs in the plugin's session, but every
   inner call is limited to the caller's grants as well (3.2). The `command` world itself is PL-03's (ADR 0064:
   `command-plugin`).
2. **Unsigned plugins under development: a per-person dev key** (3.2), signed by the build script; it never
   leaves the machine and an approval against it covers only that machine.
3. **Plugin memory in performance mode: 128 MiB inside `ram.runtime`, provisional.** The first node-plugin task
   measures and states it.
