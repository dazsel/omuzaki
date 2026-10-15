---
title: "0058: Plugins are wasmtime components that call ops through one generic bridge, as their own sessioned actor, under fuel and memory caps (2026-10-15)"
summary: "Plugins are wasmtime components that call ops through one generic bridge, as their own sessioned actor, under fuel and memory caps"
area: decisions
---

# 0058: Plugins are wasmtime components that call ops through one generic bridge, as their own sessioned actor, under fuel and memory caps (2026-10-15)

**Status:** accepted (2026-10-15). It adds two approved `omuzaki-ops-api` variants. **Context:** PL-01, PL-03, PL-04; spec v3 §6, §9, §10,
§30, §39 rules 1, 2, 6, 10; ADR 0009, 0015, 0016, 0020, 0034, 0042. Design: `docs/design/plugins.md`.

## Context

PL-01 promises a wasmtime host with the op API as WIT interfaces, the same scopes as agents, a sandbox and hot reload.
Already settled: a plugin is an `Actor::Plugin` with a `Transport::Plugin` session (ADR 0020); plugin nodes run on
the automation engine thread with a 1 ms fuel cap (automation D15); plugins ship their own Fluent resources;
wasmtime and extism are cleared in the licence register. Open until now: the crates, the WIT contract and its
versions, how ops are exposed, the ceiling, the sandbox's numbers, which thread runs what, the lifecycle, the
manifest, rights, and how plugins relate to scripts (ADR 0034).

## Prior art

- **Zed's extension host** is the closest match: extensions are WebAssembly components run by wasmtime, its API is a
  WIT package versioned per release, and the host keeps the bindings for older API versions so extensions built
  against them still load. We take the versioned worlds and the host-side adapters for older versions (decision 2).
  We differ in what crosses the boundary: Zed exposes a typed function per capability, where our plugins reach the
  whole op registry through one generic bridge (decision 3), under the same grants as agents (decision 5) and with
  fuel and deadlines per call (decision 6).
- **Extism** wraps Wasm modules in its own byte-buffer ABI; decision 1 says why we don't build on it.
- **Envoy's proxy-wasm** runs Wasm filters inside the host proxy process, called by the host per request event on
  its worker threads, with only the host functions its ABI defines. We take that shape for node activations: the
  host calls the guest per event on its own thread, and every capability is a host function. The per-call fuel and
  deadline caps are ours (design §4.3).
- **VS Code's extension host** is the contrast: extensions run in a separate Node.js process with Node's full access
  to files, network and processes, so the process boundary keeps the editor responsive but doesn't limit what an
  extension can reach. We chose a wasm sandbox because a plugin here acts unattended on a person's project and
  stream: it starts with no ambient access, gets each capability by grant, counts its work deterministically, and
  calls the host in microseconds rather than over a process channel.

## Decision

1. **Three crates:** `omuzaki-plugin-api` (WIT files, `Manifest`, `PluginInfo`, `PluginState`, `PluginError`, the
   `Plugins` trait; no wasmtime), `omuzaki-plugin-mock`, and `omuzaki-plugin` (wasmtime 49, native only, the
   `plugin.*` ops, linked by the desktop App and the CLI through `link()`). wasmtime directly: extism has no
   component model, so no WIT and no versioned imports.
2. **The WIT package `omuzaki:plugin@0.1.0`** has the base world `plugin` (imports `ops`, `host`, `data` and `http`,
   the last two linked to refusals unless requested; exports `lifecycle`: `init`, `approval-resolved`,
   `shutdown`). Each extension world includes it. A 0.x host serves its own minor and the one before through an adapter; from 1.0, semver. A node
   activation gets refusals for `ops`, `data` and `http`; its only effect is an asynchronous `run-op` in the show's
   session, within the plugin's ceiling and the needs that show approved (automation D15).
   A node instance gets its clock and randomness only through `NodeCx`: show time, the step's wall time, and the
   node's stream seeded from the show seed and the node's path. OS entropy and the OS clocks are not linked into node
   instances, and replay tests prove two runs of one show give identical effects (automation D6).
3. **The op bridge is generic:** `call`, `plan` (dry run), `describe` (the JSON Schema) and `list`. A WIT function per
   op would need a central list (§39 rule 2) and would break every plugin whenever any op changed. `op-error` maps
   `OpError` one to one.
4. **One transaction per entry call**, labelled `<plugin>: <entry>`, rolled back on error or trap; node activations
   are one-op transactions, as automation's are. Cause: the new `CauseKind::Plugin`, text `<id>@<version> <entry>`.
5. **Identity and grants:** actor `plugin <id>#<session>`, a session opened at load for the person who enabled it.
   Ceiling = manifest requests ∩ the person's approval ∩ the person's grants. `native`, `fs`, `process`, `input` and
   `script` are never granted; files only in the plugin's own data folder; network only through the host's `fetch`
   to hosts approved one by one; `publish`, `broadcast` and `spend` are queued for a person on every use.
   Approvals attach to the content hash and the publisher key, never the name. **Revocation bites at once:** every
   `http` and `data` call checks the live grant, and revoking the session or removing a grant cancels the
   outstanding `fetch`es (design 3.4, with the tests that prove it mid-request).
6. **Sandbox:** no WASI except a fixed stub subset that grants nothing (so `std` guests link); 64 MiB memory, one
   table of 10,000 elements, 32 instances, a 512 KiB stack; fuel and an epoch deadline per call kind (`init` 10⁹
   fuel and 5 s; node activations 10⁶ fuel, about 1 ms on `R`, and 2 ms); a host-call bucket of 10,000 a second; `ops.call` and `plan` bytes capped at 1 MiB in and out; each call's fuel is the smaller of its cap and the plugin's remaining CPU share. A
   trap faults the plugin and ends its session with the new `EndReason::Faulted { reason }`; three faults in ten
   minutes disable it. Every import charges fuel (2,000 plus 1 per byte), so caps and the 100 ms a second CPU share
   count host work. Blocking host calls carry the deadline: `fetch` is cancelled at it, `wasi:io/poll` waits at most
   until it (and never waits in a node instance), and nothing is called after it. In a worker, `wasi:random/random`
   is OS entropy and only the `insecure` interfaces are seeded; a random request over 4,096 bytes traps before the
   host allocates, in every instance. `.zip` packages refuse absolute
   and `..` paths, links, duplicate normalised names and oversize, and the hash is over the staged files.
7. **Threads:** one worker thread per plugin; node activations only on the engine thread in a second 16 MiB
   instance; never the render, audio or tracking thread unless an extension design adds a number. Every path has a
   budget and a gate (design §5.2).
8. **Lifecycle:** installed per machine, enabled per machine or per (machine, project) in a person-only `Machine`
   setting; nothing loads unless enabled (SE-04). Reload swaps by content hash, drains, and hands over no state.
   **Kept data belongs to an identity:** a data folder is keyed by the verifying publisher key plus a per-install id,
   never the plugin id, so another publisher's plugin with the same id never sees it; moving data between identities
   is a person-only op the person confirms. Removing a plugin is an ordinary edit that keeps the data; deleting the
   data is its own `Undo::None` op, confirmed first.
   Plugin settings register at enable and leave at disable through a **runtime settings registry** in the settings
   contract (design §7 states what it must provide), its own contract task that lands, with conformance tests,
   before the plugin settings work builds on it.
   The management ops (install, enable, disable, grant, reload, remove, remove_data, migrate_data) are `person_only` and never queued;
   `plugin.run` is a driver job (as `script.run`), `read` statically, whose driver checks the command's declared scope with `JobCtx::authorize` before the guest starts and runs every inner call inside
   `caller's grants ∩ declared scope ∩ plugin ceiling`, as `script.run` does; `plugin.list` and `plugin.describe` are `read`
   queries agents may use.
9. **Rights:** the card shows the licence and the "not from Omuzaki" line (ADR 0016's pattern). A non-commercial or
   unknown licence is shown, not refused, for a plugin the person installs; the store (PL-04) refuses it. The licence
   record carries each plugin whose ops touched an exported thing.
10. **Scripts and plugins share one grants model and one journal.** They differ on identity (scripts run as their
    caller), ceiling source (a script's syntax tree, a plugin's manifest), queueing (scripts never queue, plugins
    receive the outcome later) and limit units, each for a stated reason (design §10).
11. **Plugin-defined ops** go through one static op, `plugin.run {id, command, args}`, with each command's schema
    through `describe` and its scope declared in the manifest; there is no run-time op table. A command never acts
    beyond its caller's grants.
12. **Development builds** are signed automatically with a per-person dev key that never leaves the machine; an
    approval against it covers that machine only, and anything published is signed with the publisher key.
13. **Performance-mode plugin memory** is capped at 128 MiB inside `ram.runtime`, provisionally; the first
    node-plugin task measures it and states the number.

## Consequences

- `omuzaki-ops-api` gains `CauseKind::Plugin` and `EndReason::Faulted { reason }` (approved by the Runtime lead), in
  one task with #1535's `CauseKind::Review` that also marks both enums `#[non_exhaustive]` (ADR 0054 §4).
  `Actor::Plugin` and `Transport::Plugin` already exist.
- The executor gains one plugin rule: per-use queueing of `publish`, `broadcast` and `spend` for plugin sessions.
- `omuzaki-settings-api` gains the runtime settings registry (register, unregister, owner-tagged values), as its own
  contract task.
- The tasks of design §12 and the unlocked work (hot reload, the manager panel, the mutating ops and their cards,
  settings and the data folder, log lines in the Theatre, localisation, the SDK example and guide, the web opt-out
  check) are filed when this is accepted.
- The web build has no plugins; their nodes show inert with a finding.
- Rejected:
  - extism, for the reason in decision 1;
  - typed WIT per op;
  - plugins running as the person, like scripts: third-party code that acts later must have its own kill switch;
  - full WASI, or WASI filesystem with a preopen: the data interface is smaller and its quota checkable;
  - handing state over on reload;
  - keying kept data or settings by the plugin id, which its author chooses;
  - a run-time op table, and "developer folders" that carry unsigned approvals forward (design §14).
