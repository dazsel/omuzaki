# 0058: Plugins are wasmtime components that call ops through one generic bridge, as their own sessioned actor, under fuel and memory caps (2026-10-15)

**Status:** proposed. It adds two `omuzaki-ops-api` variants. **Context:** PL-01, PL-03, PL-04; spec v3 §6, §9, §10,
§30, §39 rules 1, 2, 6, 10; ADR 0009, 0015, 0016, 0020, 0034, 0042. Design: `docs/design/plugins.md`.

## Context

PL-01 promises a wasmtime host with the op API as WIT interfaces, the same scopes as agents, a sandbox and hot reload.
Already settled: a plugin is an `Actor::Plugin` with a `Transport::Plugin` session (ADR 0020); plugin nodes run on
the automation engine thread with a 1 ms fuel cap (automation D15); plugins ship their own Fluent resources;
wasmtime and extism are cleared in the licence register. Open until now: the crates, the WIT contract and its
versions, how ops are exposed, the ceiling, the sandbox's numbers, which thread runs what, the lifecycle, the
manifest, rights, and how plugins relate to scripts (ADR 0034).

## Decision

1. **Three crates:** `omuzaki-plugin-api` (WIT files, `Manifest`, `PluginInfo`, `PluginState`, `PluginError`, the
   `Plugins` trait; no wasmtime), `omuzaki-plugin-mock`, and `omuzaki-plugin` (wasmtime 49, native only, the
   `plugin.*` ops, linked by the desktop App and the CLI through `link()`). wasmtime directly: extism has no
   component model, so no WIT and no versioned imports.
2. **The WIT package `omuzaki:plugin@0.1.0`** has the base world `plugin` (imports `ops`, `host`, and `data` and
   `http` when requested; exports `lifecycle`: `init`, `approval-resolved`, `shutdown`). Each extension world
   includes it. A 0.x host serves its own minor and the one before through an adapter; from 1.0, semver.
3. **The op bridge is generic:** `call`, `plan` (dry run), `describe` (the JSON Schema) and `list`. A WIT function per
   op would need a central list (§39 rule 2) and would break every plugin whenever any op changed. `op-error` maps
   `OpError` one to one.
4. **One transaction per entry call**, labelled `<plugin>: <entry>`, rolled back on error or trap; node activations
   are one-op transactions, as automation's are. Cause: the new `CauseKind::Plugin`, text `<id>@<version> <entry>`.
5. **Identity and grants:** actor `plugin <id>#<session>`, a session opened at load for the person who enabled it.
   Ceiling = manifest requests ∩ the person's approval ∩ the person's grants. `native`, `fs`, `process`, `input` and
   `script` are never granted; files only in the plugin's own data folder; network only through the host's `fetch`
   to hosts approved one by one; `publish`, `broadcast` and `spend` are queued for a person on every use.
   Approvals attach to the content hash and the publisher key, never the name.
6. **Sandbox:** no WASI except a fixed stub subset that grants nothing (so `std` guests link); 64 MiB memory, one
   table of 10,000 elements, 32 instances, a 512 KiB stack; fuel and an epoch deadline per call kind (`init` 10⁹
   fuel and 5 s; node activations 10⁶ fuel, about 1 ms on `R`, and 2 ms); a host-call bucket of 10,000 a second. A
   trap faults the plugin and ends its session with the new `EndReason::Faulted { reason }`; three faults in ten
   minutes disable it.
7. **Threads:** one worker thread per plugin; node activations only on the engine thread in a second 16 MiB
   instance; never the render, audio or tracking thread unless an extension design adds a number. Every path has a
   budget and a gate (design §5.2).
8. **Lifecycle:** installed per machine, enabled per machine or per (machine, project) in a person-only `Machine`
   setting; nothing loads unless enabled (SE-04). Reload swaps by content hash, drains, and hands over no state.
   Every mutating `plugin.*` op is `person_only` and never queued; `plugin.list` and `plugin.describe` are `read`
   queries agents may use.
9. **Rights:** the card shows the licence and the "not from Omuzaki" line (ADR 0016's pattern). A non-commercial or
   unknown licence is shown, not refused, for a plugin the person installs; the store (PL-04) refuses it. The licence
   record carries each plugin whose ops touched an exported thing.
10. **Scripts and plugins share one grants model and one journal.** They differ on identity (scripts run as their
    caller), ceiling source (a script's syntax tree, a plugin's manifest), queueing (scripts never queue, plugins
    receive the outcome later) and limit units, each for a stated reason (design §10).

## Consequences

- `omuzaki-ops-api` gains `CauseKind::Plugin` and `EndReason::Faulted { reason }`, additive, each needing the
  Director's approval on its task first. `Actor::Plugin` and `Transport::Plugin` already exist.
- The executor gains one plugin rule: per-use queueing of `publish`, `broadcast` and `spend` for plugin sessions.
- The tasks of design §12 and the unlocked work (hot reload, the manager panel, the mutating ops and their cards,
  settings and the data folder, log lines in the Theatre, localisation, the SDK example and guide, the web opt-out
  check) are filed when this is accepted.
- The web build has no plugins; their nodes show inert with a finding.
- Rejected:
  - extism, for the reason in decision 1;
  - typed WIT per op;
  - plugins running as the person, like scripts: third-party code that acts later must have its own kill switch;
  - full WASI, or WASI filesystem with a preopen: the data interface is smaller and its quota checkable;
  - handing state over on reload.
- Open questions for the Director and the owner are in design §14: plugin-defined ops, unsigned plugins under
  development, and the performance-mode memory cap.
