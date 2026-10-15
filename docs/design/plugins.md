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

The decisions in brief are ADR 0058's; this page gives the reasons and the numbers.

## 1. Crates and layering

| Crate | Kind | Holds | Depends on |
|---|---|---|---|
| `omuzaki-plugin-api` | API | `wit/plugin.wit` as `WIT_PLUGIN: &str` (and the files under `wit/`), `Manifest` with `parse` and `check`, `PluginId`, `PluginInfo`, `PluginState`, `PluginError`, `Requests`, `CallKind` and its `Budget` table, the `Plugins` service trait | `omuzaki-core`, `omuzaki-ops-api`, `omuzaki-store-api` (`ContentHash`), `omuzaki-library-api` (`Version`, `VersionReq`), `omuzaki-l10n-api` |
| `omuzaki-plugin-mock` | Mock | `MockPlugins` (in-memory `Plugins`), `MockGuest` (a plugin written as Rust closures against the same guest interface, no wasm), `conformance` | the API crates |
| `omuzaki-plugin` | Implementation | the component host (wasmtime 49, Cranelift), the WASI stubs (§4.1), the bridge, the compiled-code cache, the watcher, the `plugin.*` ops; `[package.metadata.omuzaki] wasm = false` (a JIT host has no place in a browser) | the API crates, `omuzaki-http-api` |

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
- **One world per extension kind.** The base world is `plugin` (below). Each extension world (`node`, `tracker`,
  `exporter`, `part-generator`, `adapter`) `include`s it and adds its own exports, so every plugin speaks the same base
  interfaces and a host checks one world per kind. The manifest's `worlds` key (§7) says which a component targets.
- **Node activations never reach the base imports** (automation D15). The engine instance (§5.1) links `ops`, `data`
  and `http` to refusals (`denied`, detail "not in a node activation"); a node's only effect is the node world's
  `run-op`, which mirrors `NodeCx::run_op`: asynchronous, in the show's session, with the ceiling show session ∩ the
  plugin's ceiling ∩ the needs that show approved for that node.

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
    /// Called once per instance, with the plugin's settings as JSON (§7). An error faults the plugin with that text.
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
- **One transaction per entry call.** The host `begin`s a transaction labelled `<plugin name>: <entry>` before the
  first edit of an entry call (`init`, `approval-resolved`, and the extension worlds' worker entries), sends each
  `call` in it, and commits when the entry returns `ok`. An `err`, a trap or a breached deadline rolls it back. The
  plugin has no `begin`, `commit` or `undo` of its own: one entry is one step, which is what a person can follow in
  History. **Node activations are the exception:** they follow automation, where every call is its own one-op
  transaction (automation D5), because a show's steps are not undo units.
- **Cause:** `Cause { kind: Plugin, text: "<id>@<version> <entry>" }`, for example `example@0.1.0 init`. It names
  the entry, never arguments, so no plugin data lands in History labels. `CauseKind::Plugin` is a new variant (§11);
  reusing `Script` would make every plugin call refuse to queue (§10), and `Rule` belongs to automation.
- **Queued requests.** A request that waits for a person (`publish`, `broadcast`, `spend`, an op that calls
  `require_person`) is queued and runs, once approved, as its own transaction (ADR 0020 4.4); the entry call has
  long returned, so the host delivers the outcome later through `approval-resolved`.
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

## 4. The sandbox

### 4.1 WASI: a fixed stub subset that grants nothing

The base world imports no WASI. But Rust's standard library on `wasm32-wasip2` imports a few WASI interfaces even
for a plugin that never reads a file, so a host that links none would refuse every `std` plugin. The host links
exactly these, and refuses a component that imports any other (`PluginError::Imports { unexpected }`):

| Interface | What the plugin gets |
|---|---|
| `wasi:cli/environment` | no variables, no arguments, cwd none |
| `wasi:cli/exit` | a trap: the plugin is `Faulted { reason: "exited" }` |
| `wasi:cli/stdout`, `stderr` | written into the plugin's log at `debug`, under the same caps as `log` (§7) |
| `wasi:cli/stdin` | an empty stream |
| `wasi:io/streams`, `error`, `poll` | what the three streams above need |
| `wasi:clocks/monotonic-clock` | the runner's clock, as `monotonic-seconds` |
| `wasi:clocks/wall-clock` | the runner's clock since 1970-01-01 at session start, so tests stay deterministic |
| `wasi:random/random` | OS entropy: it backs the guest's `getrandom`, so keys and nonces must not be predictable |
| `wasi:random/insecure`, `insecure-seed` | a ChaCha stream seeded from the content hash and the session id (deterministic, ADR 0009); tests also seed `random` through a test-only host option |
| `wasi:filesystem/types`, `preopens` | no preopens: every file call fails cleanly; files go through `data` |

`wasi:sockets` and `wasi:http` are never linked.

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
| Export lists | `list()` results and `describe` strings over 1 MiB, an `init` config over 64 KiB | the bridge; a bad or oversized return value faults the plugin |

### 4.3 Fuel and deadlines per call kind

Fuel counts work deterministically (wasmtime's fuel is close to one unit per instruction), so the same plugin on the
same input stops at the same point on every machine. An epoch deadline is the wall-clock backstop; it ticks every
1 ms on a host timer thread. **Fuel counts host work too:** each import charges a fixed 2,000 fuel (about 2 µs on
`R`) plus 1 per byte passed either way, so the caps and the CPU share (§5.2) see host calls deterministically; the
bench fails if a charge falls below that import's measured p95.

An epoch is only seen when control is back in wasm, so **blocking host calls carry the deadline themselves**: the
bridge refuses every import once the entry's deadline has passed; `fetch` runs with timeout min(10 s, time left) and a
`CancelFlag` the timer thread sets at the deadline (`HttpTransport` stops within 50 ms); an `ops.call` can't be
cancelled mid-op, so it is checked on return, and the overrun is at most one op, which its own budget bounds. Node
activations make no blocking call: `run-op` only enqueues (1.1).

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
- **Never on the render, audio or tracking thread.** An extension design may allow one exception only with its own
  number and gate on the §10 table (say, a build-time part generator). Compiling runs on the worker, not the UI thread.

### 5.2 The budget table

Measured on `R` unless the row says otherwise; the plugin bench (§12) runs nightly on `Dev` and scales by 1.43.

| Path | Budget | Statistic | Gate | On breach |
|---|---|---|---|---|
| Install: hash, signature check, manifest check, compile of a 2 MiB component | ≤ 1.5 s | worst of 5 | bench `install_2mib`, nightly | reported; nothing stops |
| Load from the compiled-code cache, to `init` called | ≤ 20 ms | p95 | bench `load_cached`, nightly | reported |
| `init` | 10⁹ fuel, 5 s | per call | §4.3, conformance test | fault |
| Host-call overhead, `call` round trip with 1 KiB of JSON each way, excluding the op | ≤ 20 µs | p95 | bench `call_1kib`, nightly; same-session A/B on bridge PRs | reported |
| `log`, `monotonic-seconds` | ≤ 2 µs, ≤ 0.5 µs | p95 | bench `host_small` | reported |
| A node activation (engine thread) | 10⁶ fuel ≈ 1 ms, 2 ms deadline | per activation | §4.3, calibration test | node fault |
| A plugin's CPU share, all its calls | ≤ 100 ms of work a second (10 % of one core, 1.25 % of `cpu.total`'s 8 cores) | per second, fuel-counted, host-call charges included (§4.3) | conformance test on `FakeClock` | further entry calls `limited` until the next second |
| Memory per plugin | 64 MiB worker + 16 MiB engine instance + ≤ 8 MiB host side (code, store) | max | conformance test (`memory.grow` past the cap fails) | trap: fault |
| Memory, all plugins | ≤ 256 MiB in the editor (of `ram.editor`'s 1.5 GB), ≤ 128 MiB in performance mode (of `ram.runtime`'s 400 MB; provisional, 14.3) | linear memory in use, summed | the host, on every `memory.grow` | that grow fails (the plugin faults); the manager names the largest plugins |
| Reload: drain and swap | ≤ the running call's deadline + 1 s shutdown + load | per reload | conformance test | the old instance is dropped |

The §10 budget scene has no plugins, so these rows add to it and never move its numbers; the Performance HUD shows
plugin CPU and memory as their own line.

## 6. Lifecycle

### 6.1 Where plugins live

- **Installed packages:** `<local data dir>/plugins/<id>/<hash>/` (the settings design's machine directory: code and
  approvals are this machine's, so they never roam). The last two versions are kept for rollback.
- **Data folders:** `<local data dir>/plugin-data/<id>/`, shared by its versions.
- **Compiled code cache:** `<cache dir>/plugins/<hash>-<wasmtime version>-<cpu features>.cwasm`, a cache only: a
  mismatch or a bad file recompiles.
- **Enabled:** the `Machine` setting `plugins.enabled` (`AgentWrite::Never`), each entry "on for every project" or "on
  for project `<real path>`". A project from a friend, a template or a git pull can't turn on code: a project only
  says which plugins its nodes use, and a missing or disabled one leaves those nodes inert with a finding (automation
  D15).

### 6.2 States

`PluginState`: `Installed` (on disk, off), `Starting`, `Running`, `Faulted { reason, at }`, `Disabled { reason }`
(`Person`, `Faults`, `Incompatible`), `Reloading`. **Nothing loads at launch that the person has not enabled**
(SE-04): a disabled plugin's manifest is read for the list, its component is never compiled.

### 6.3 Update, reload and removal

- **Update** installs the new hash beside the old, carries approvals per 3.2, and swaps with a reload.
- **Hot reload** (`plugin.reload`, or the watcher of a plugin installed with `watch`, which re-reads its source folder): the trigger is a changed
  content hash, never a timestamp. New entry calls queue; the running one finishes or meets its deadline; `shutdown`
  runs (1 s); the old session ends `Closed`; the new instance starts with a new session and `init`. Node instances
  swap between activations. **No state is handed over:** a hand-off needs a format both versions agree on, and a
  plugin that wants to keep something writes it to its data folder, which survives.
- **Removal** (`plugin.remove {id, data: keep | delete}`, default `keep`) disables, ends the session, deletes the
  packages and, if asked, the data folder. Its approvals are dropped; History keeps its steps under its actor.

## 7. The manifest

```toml
id = "example"                 # ^[a-z][a-z0-9_]{1,31}$, no reserved word (below)
version = "0.1.0"              # semver
api = "^0.1"                   # must agree with the component's imports (1.1)
licence = "MIT"                # an SPDX expression
name = "Example"               # fallback for the Fluent `plugin-example-name`
description = "Waves when the project opens."
worlds = ["plugin"]            # the base world; PL-03 adds "node", "tracker", ...
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
  content hash is BLAKE3 over the sorted (path, bytes) list without `plugin.sig`; the signature is an ADR 0042
  envelope over that hash with the domain tag `omuzaki/plugin/v1`. **Extraction** is into a fresh staging folder and
  refuses the whole package on: an absolute path, a drive prefix or a `..` component; a symlink, hardlink or any entry
  that is not a regular file or folder; two entries equal after normalising (Unicode NFC, case-folded, `/` only); more
  than 256 entries or 32 MiB uncompressed. The hash is taken over the staged files, so what is hashed is what loads.
- **Settings** (at most 64, as are commands) register at enable under `plugin.<id>.*` with the manifest's scope and
  agent rule (default `never`), and leave at disable; `init` receives them as JSON.
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

| Op | Kind | Scope | Who | What it does |
|---|---|---|---|---|
| `plugin.list {}` | Query | `read` | anyone, agents included | installed plugins: `PluginInfo` (id, version, hash, publisher, licence, state, grants, worlds) |
| `plugin.describe {id}` | Query | `read` | anyone | one plugin: manifest, requests, approvals, last faults, memory and CPU |
| `plugin.install {path, watch?}` | Edit (`Undo::Explicit`) | `edit:settings` | person only | checks and copies a package; shows the card; does not enable |
| `plugin.enable {id, project?}` / `plugin.disable {id, project?}` | Edit | `edit:settings` | person only | writes `plugins.enabled`; starts or stops the instance |
| `plugin.grant {id, scopes?, hosts?, data?}` | Edit | `edit:settings` | person only | sets what the person approves, never beyond the requests |
| `plugin.reload {id}` | Edit | `edit:settings` | person only | 6.3 |
| `plugin.remove {id, data}` | Edit | `edit:settings` | person only | 6.3 |
| `plugin.run {id, command, args}` | Edit | the command's declared scope | anyone with it | runs a plugin command (14.1) inside the caller's grants (3.2) |

The management ops (install, enable, disable, grant, reload, remove) return `OpError::person_only` to any agent or
plugin and are **never queued**: an agent that installs, enables or grants a plugin hands its own work to an actor
whose grants it chose, which is escalating its own privileges with extra steps. `plugin.run` is not one: it runs for
any caller holding the command's declared scope.

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
    fn remove(&self, id: &PluginId, data: DataOnRemove) -> Result<(), PluginError>;
}
```

**`omuzaki-ops-api`** (approved by the Runtime lead on #1537): `CauseKind::Plugin` and `EndReason::Faulted { reason:
String }`. One task adds them with `CauseKind::Review` (#1535), so the hub file changes once, and marks both enums
`#[non_exhaustive]` (ADR 0054 §4). Nothing else changes: `Actor::Plugin`, `Transport::Plugin` and the session types
exist.

## 12. Tests

- **The conformance suite** (`omuzaki-plugin-mock::conformance`) runs against `MockPlugins` with `MockGuest` and
  against the wasmtime host with WAT components, on `FakeClock`: lifecycle and states, the ceiling intersection,
  every error-code mapping, per-use queueing and `approval-resolved`, a trap ending the session `Faulted`, auto-disable
  at the third fault, reload draining a running call, settings and Fluent resources added and removed.
- **Hostile plugins**, each a WAT component in `crates/omuzaki-plugin/tests/hostile/`, each with its expected result:

| Plugin | Expected |
|---|---|
| infinite loop in `init` | out of fuel at 10⁹, `Faulted { "out of fuel in init" }`, session `Faulted` |
| loop around a slow host call | epoch deadline at 5 s |
| `memory.grow` to 1 GiB | grow fails at 64 MiB; the guest's abort traps |
| deep recursion | stack overflow trap at 512 KiB |
| host-call flood (10⁶ `log` calls) | 100 lines then "N lines dropped"; the bucket traps at 10,000 a second |
| `call` flood | `limited` with `RetryAfter`; the person's edits are never slowed |
| bad return (invalid UTF-8, a `list` of 10⁶ names) | fault "bad return value" |
| trap at start | `Faulted`; restart after 1 s, 10 s; disabled at the third |
| imports `wasi:sockets` | refused at install, `Imports { unexpected: ["wasi:sockets/tcp"] }` |
| `fetch` to a host not approved | `err("host not approved")`, one `warn` line |
| `read`-only agent runs a `read` command that calls `edit:scene` | refused `MissingScope`, nothing changed, one `warn` line |
| 10⁶ zero-length `data.write`s | the 4,097th is "over quota"; `list` returns at most 4,096 names |
| `fetch` that outlives `init`'s 5 s | cancelled at the deadline, fault "deadline in init" |
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
   inner call is limited to the caller's grants as well (3.2). The `command` world itself is PL-03's.
2. **Unsigned plugins under development: a per-person dev key** (3.2), signed by the build script; it never
   leaves the machine and an approval against it covers only that machine.
3. **Plugin memory in performance mode: 128 MiB inside `ram.runtime`, provisional.** The first node-plugin task
   measures and states it.
