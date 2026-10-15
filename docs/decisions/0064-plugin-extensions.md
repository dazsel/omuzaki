---
title: "0064: Plugin extensions are one world and one instance kind each, declared in the manifest, added to run-time catalogues all or nothing (2026-10-15)"
summary: "Plugin extensions are one world and one instance kind each, declared in the manifest, added to run-time catalogues all or nothing"
area: decisions
---

# 0064: Plugin extensions are one world and one instance kind each, declared in the manifest, added to run-time catalogues all or nothing (2026-10-15)

**Status:** proposed (#1083); accepted when merged. Decisions 4, 6 and 7 change contracts other areas own and need
the Director's sign-off. **Context:** PL-03, BR-10; spec v3 §10, §22, §25, §30, §39 rule 2; ADR 0015, 0054, 0058;
automation D9 and D15. Design, prior art and numbers: `docs/design/plugin-extensions.md`.

## Context

ADR 0058 built the host and the base world and fixed only the node seam. PL-03 also promises event adapters,
trackers, outputs, exporters and part generators. Each plugs into a contract that today registers only at link time
(`register_exporter!`, `register_part_kind!`, `register_event_kind!`) or, for trackers, into a real-time path with a
5 ms budget.

## Decision

1. **One world per extension** (`node-plugin`, `adapter-plugin`, `tracker-plugin`, `exporter-plugin`, `part-plugin`,
   `command-plugin`), each including the base `plugin` world, in `omuzaki:plugin@0.1.0`; the manifest's `worlds` names
   them by key, and a component offering several includes each in its own world.
2. **One instance kind per extension, each with its own `Store`, thread, memory, fuel and deadline** (design §3).
   Nodes stay on the engine thread (D15: 10⁶ fuel, 2 ms); adapters and commands run on the worker; trackers on a
   dedicated `plugin-track:<id>` thread; exporters in a fresh instance per `io.export` job; parts on the character
   builder's thread. Tracker, exporter and part instances are **pure**: no `ops`, `data` or `http`, seeded clocks
   and randomness, so a camera frame, a GLB or a character's parameters can only leave as the checked return value.
3. **Every kind is declared in the manifest** with its ports, schemas, needs, published kinds and claimed channels.
   The host answers metadata without running guest code, so a disabled plugin's kinds are listed, the card shows
   every need before code runs, and install refuses a bad declaration. Ports are static in 0.1.
4. **Event adapters poll** (`poll` every 50 to 1,000 ms; at most 256 events a poll, 200 a second, burst 1,000). Events
   carry `Source::Plugin { id }`; the host overwrites the payload's `platform` with the manifest's, never a
   first-party one; a plugin may publish D9's normalised platform kinds and its own `plugin.<id>.<kind>`, added through
   a new `DynEventKind` catalogue in `omuzaki-live-api` (automation's contract). The host retains non-droppable events
   per D8. BR-10's worked example is `fakechat`, which replays a file in its own data folder.
5. **Trackers overlay claimed channels.** Input is a downscaled frame (≤ 320 × 240) from a tap on the first-party
   tracker, only with the person's camera tick for that plugin. Hand-off is newest-wins with nothing allocated: a
   triple buffer in, `MailboxWriter` out, the stage overlays. Budgets on `R`: tap ≤ 0.3 ms inside `track.face`'s 5 ms;
   guest ≤ 2.5 × 10⁶ fuel; tap to published frame ≤ 3.2 ms p95. On a fault the claimed channels hold for
   `lost_after` (200 ms), then come from the first-party tracker, then the life layer as today.
6. **Exporters:** an additive run-time exporter catalogue in `omuzaki-io-api` (human lane). The host runs the rights
   preflight unchanged, hands the scene over as GLB (≤ 128 MiB), checks the returned files (≤ 16, ≤ 256 MiB, flat
   names with declared extensions) and `io.export` writes them under its own checks; a plugin never writes. Format ids
   are `plugin-<plugin>-<name>`, since `FormatId` is a slug without dots.
7. **Part generators:** `DynPartKind` and a run-time catalogue in `omuzaki-char-api` (human lane); ids
   `plugin-<plugin>-<name>`; parameters checked against the manifest's schema; seeded from `BuildCtx::rng` and pure, so
   deterministic; mesh as flat arrays (≤ 8 meshes, 65,536 vertices, 65,536 triangles; rigid in 0.1);
   `KindProvenance::External`, with a source row `Generated { tool: "plugin <id>@<version>", model: <hash> }` carrying
   the manifest's licence, so the licence gate treats the plugin as it treats any source.
8. **No output world in 0.x:** video would copy about 249 MB/s into an instance with no shared memory, and data
   outputs need sockets, which plugins never get; protocol outputs are #1085's.
9. **Registration is ordered and all or nothing:** on enable, event kinds, node kinds, part kinds, exporters, then the
   tracker and polling; on disable, the reverse. Each catalogue changes by one atomic swap; a failure removes what was
   added and faults the plugin.
10. **A document naming a missing kind loses nothing:** parameters stay byte for byte, with a finding naming the plugin;
    nodes are inert, triggers never fire, parts don't build and their character can't be exported until the plugin is
    back, exporters are `not_found`, tracker channels fall back to first party.
11. **Never:** plugin code on the render, audio, capture or tracking thread; the GPU; a raw device; memory shared with
    the host; a file written by the plugin itself.

## Consequences

- Contract work, each its own task landing before its adapter (design §9): `omuzaki-plugin-api` gains the worlds and
  manifest sections (plugins area); `omuzaki-live-api` gains `DynEventKind` (automation, Director); `omuzaki-track-api`
  gains the frame tap (tracking, Director); `omuzaki-io-api` and `omuzaki-char-api` gain run-time catalogues (human
  lane, Director). None changes an existing type; `omuzaki-store-api` and `omuzaki-core` don't change.
- `automation.md` D9's comment that plugin adapters use `Source::Platform` is corrected by the `DynEventKind` task.
- #1366 keeps its names; its WIT moves to `omuzaki-plugin-api` (T1).
- Rejected: pushing adapters (a looping entry holds the worker); trackers on the tracking thread (CLAP's lesson);
  full-resolution or untapped camera access; a dotted format or part id (a core slug change for no gain); ports
  computed by guest code in 0.1; plugin video outputs; plugin-written files.
