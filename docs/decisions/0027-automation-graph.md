---
title: "0027: Automations are deterministic graphs of nodes that act only through ops, in a session the person approved (2026-10-12)"
summary: "Automations are deterministic graphs of nodes that act only through ops, in a session the person approved"
area: decisions
---

# 0027: Automations are deterministic graphs of nodes that act only through ops, in a session the person approved (2026-10-12)

**Status:** proposed. **Context:** BR-11, BR-26, BR-10, AP-10,
AP-11, PL-03; spec v3 §3, §6, §7, §8, §9, §25; ADR 0009 (determinism),
ADR 0013 (file roots), ADR 0015 (registration linking), ADR 0018 (no microphone for automations), ADR 0019 (library
items, the reserved `show` target), ADR 0020 (agent sessions). Design:
`docs/design/automation.md`.

## Context

The automation graph is the main way Omuzaki serves the long tail: engagement
tools, OBS and Streamer.bot glue, voice commands, the chat reader and plugin nodes all come through it. Fourteen BR-26
tasks were filed without a contract. Five facts shape the answer:
- Every state change in Omuzaki is an op, checked, logged and permission-scoped by one executor (§6, ADR 0020), and
  agents already reach Omuzaki with sessions, grants and approvals that only a person resolves.
- Shows are shared: library templates, friends' files, a collaborator's push, an agent's edit. Their graphs could call
  any URL, write files, run programs or switch a stream.
- Chat arrives in bursts (a raid is hundreds of messages a second) and the same graph handles many events at once,
  some of them waiting.
- VNyan's own docs say its graph's execution order "is not reliable"; Omuzaki promises the same inputs give the same
  results (§4 principle 4, ADR 0009).
- The project store makes an object with a plain value a file and an object of objects a folder, and §7 asks for one
  entity per file.

## Decision

1. **Crates** (within the `omuzaki-live*` decision): `omuzaki-live-api` is the contract, `omuzaki-live-mock` the
   harness, `omuzaki-live` the engine and the `show.*` and `automation.*` ops, and `omuzaki-live-nodes` the first-party
   nodes together with the ops they call. Nodes self-register (`register_node!`) and see only the contract.
2. **The document** is `shows/<show>/`: `show.toml` (name, seed, variable declarations, groups),
   `automations/<graph>/graph.toml`, `automations/<graph>/nodes/<node>.toml` (kind, position, params, wires into its
   inputs, constants) and `applied/<app>.toml`. `project.default_show` names the default. Wires are stored at their
   destination. Runtime state (variable values, group switches, suspensions, approvals) is never in the document.
3. **Event ports carry control and value ports carry data.** Each external event starts a token that carries the
   values set along its path, so overlapping events never read each other's data. Pure nodes are evaluated when read.
4. **Execution** is one engine thread per running show, off the frame thread. Items (events, wakes, op replies) are
   processed in time order, each as a step run to completion depth-first, with fan-out in ascending node id. Given the
   same show, seed and items, results are byte-identical; each node draws from its own seeded stream.
5. **Nodes act only through ops.** A node changes the engine's own state (wakes, variables, group switches, emitted
   events) directly, and anything outside the engine only through an op run by the `Runner` in the show's automation
   session: `Transport::Automation`, a new `Actor::Automation`, on behalf of the person, with causes that name the rule
   and the event but never the chat text. One generic `op.run` node makes every allowed op an action, except ops that take a path, URL or program (jobs, `fs`,
   `net` or `process` scopes, the effect ops by name): those have dedicated nodes with literal, approved parameters. Live actions
   (empty patch, `Undo::None`) never enter undo, and automation steps are not part of the person's Ctrl+Z. A test
   event's effects run as whoever fired it, and that holds through `event.emit`: the token's acting actor is copied
   onto every derived event, so a caller can't reach, through a chain of graphs, what its own grants refuse.
6. **A loop guard and rate limits** run on the engine clock: 2,000 activations and depth 128 per step, 8 emit hops,
   500 new tokens a second per graph, 64 calls in flight, a bounded inbox: chat may be dropped at 10,000 queued, every other kind is held up to 100,000 and
   then refused with `Full`, and the producer retains and retries it. A graph that
   trips the loop guard is suspended, never the show.
7. **The event envelope** is `LiveEvent` (kind, source, platform id for dedup, time, payload, hops, and the actor a manual event acts for), published through
   `EventSink` by adapters, hotkeys, voice commands, OBS and the engine. Kinds self-register with a payload schema. The
   normalised kinds and `ChatMessage` (platform, channel, message id, user with stable id, login, display name,
   normalised roles and raw badges, text, emote spans) are fixed here; BR-10 adds fields and owns its adapters.
8. **Variables** are declared in the document and have three scopes: `session` (not persisted), `show` (the project's
   `.omuzaki/` folder) and `global` (the user's data folder). Writes are debounced to one per second per file; changes
   are announced once per step. A show carries only its declarations and defaults, never values. Secrets are never
   variables.
9. **Risky nodes need the person's approval for this show on this machine.** Each node declares static needs (scope,
   host, file, program, secret, process); an unapproved node is inert. Hosts, file paths and programs must be literal.
   An unapproved show runs with `read` and `perform` only. Approvals live in the user's data folder, keyed by project
   folder and show, are given only in Omuzaki's window, and never travel with a project. A program is an absolute path
   pinned by its content hash (scripts too), run without a shell with whole-argument placeholders; a changed file needs
   approval again. Secrets are keychain references resolved inside the op and sent only to the host approved for them;
   CN-05's secret store is pulled into Phase 2 for this, and a person enters a secret only in Omuzaki's window.
10. **Templates** are library `member` items in the `automations` section on the `show` target: one
    `automation.add_graph` step with the whole graph. Risky nodes in a template are inert until approved.
11. **Plugins** add node kinds and event kinds at run time through the same contract, with namespaced ids, fuel-capped
    activations and the same approvals.
12. **Testing:** a `NodeHarness` runs one node with scripted inputs, fake time and a fake `Runner`, so each node is one
    small PR; a `ShowHarness` runs whole shows for the engine and the templates.

## Consequences

- Four new crates; two additive `omuzaki-ops-api` changes (`Actor::Automation`, the `edit:show` scope), an executor rule
  for live actions, the l10n crate check learning `node-` and `event-`, and the library's `show` target. The task cut
  is `docs/design/automation.md` §7. The BR-26 tasks are re-cut onto these names; the engine, document ops, editor,
  Rules panel, templates and show target get tasks of their own.
- The HTTP, file and program ops are jobs, so they wait for the job path; their nodes are tested without it.
- Every effect a show has is in the op log with its rule, so History, the Theatre and agents can explain it. A raid
  that drives many live actions writes many records; the session's limits bound it.
- AP-10's commands arrive as `voice.command` events without transcripts, and its phrase lists are a `User` setting in
  the person's profile; shows refer to command ids. AP-11's reader speaks through the `tts.say` op, whose arguments and
  queue its own design shapes.
- **Answers to the open questions (2026-10-12):**
  1. an unapproved show gets `read` + `perform` only;
  2. variable values stay on this machine;
  3. AP-10's phrase lists live in the person's profile;
  4. AP-11's speech is an op;
  5. CN-05's secret store lands no later than the first node that needs a secret, so it moves into Phase 2.
  The design's §8 records each answer with its reason.
- **Revised 2026-10-15 for ADR 0054** (design D21): `omuzaki-live-api` exports by path; node and op folders get
  generated module lists; the event, payload, need and input types and their struct-like variants are
  `#[non_exhaustive]` with constructors (`PortType`, `Val` and `NodeClass` stay closed on purpose); keys other
  features add to a show register with `register_show_key!`, are named after their owner crate and are kept by every
  binary, checked only where the owner is linked; a node's wildcard input arm returns `Unhandled`, never `Ok`; mocked graph runs
  go in `omuzaki-test-live` and template apply-and-undo checks in `omuzaki-cli`'s tests; the crates set `registers = true` and opt out of wasm.
- Spec §6 lists `edit:show` (with `input` and `process`), §7 the show folder, §9 the four crates.
- **Rejected:**
  - **Latched outputs only (the Blueprint model):** overlapping events overwrite each other's data after any wait.
  - **Message passing only (Node-RED):** no data wires to see or type-check; every node would re-parse one object.
  - **Breadth-first or unordered execution:** "do this, then that" stops meaning what people expect; VNyan's
    unreliable order is a top complaint.
  - **Nodes doing their own I/O through services:** a second permission path beside the executor, which ADR 0020
    rejected for sessions, and effects that agents and scripts couldn't reuse.
  - **Variables through the `Runner`:** a counter tick per chat message in the op log, the journal and undo.
  - **Variable values, group switches or approvals in the shared document:** they belong to one machine and must not
    travel or become undo steps.
  - **One file per graph:** every drag rewrites a long file and edits to different nodes collide.
  - **Approving programs by name or path alone, or machine-wide for every show:** a changed file or an imported show
    would run without the person looking at it.
  - **A node per op:** hundreds of nodes to keep in step with the ops; `op.run` reads the op's own schema instead.
  - **Approval through an op, the CLI or MCP:** the agent controls those channels (ADR 0020 §7.2).
