---
title: "0064: Plugin extensions are one world and one instance kind each, declared in the manifest, added to run-time catalogues all or nothing (2026-10-15)"
summary: "Plugin extensions are one world and one instance kind each, declared in the manifest, added to run-time catalogues all or nothing"
area: decisions
---

# 0064: Plugin extensions are one world and one instance kind each, declared in the manifest, added to run-time catalogues all or nothing (2026-10-15)

**Status:** proposed (#1083); accepted when merged. Decisions 4, 6, 7 and 9 change contracts other areas own and need
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
   dedicated `plugin-track:<id>` thread; exporters in a fresh instance per job; parts in a fresh instance per build.
   Tracker, exporter and part instances are **pure**: no `ops`, `data` or `http`, seeded clocks and randomness, no
   `lifecycle` exports run (`init` runs once, on the worker), so their input leaves only as the checked return value.
3. **Every kind is declared in the manifest** with its ports, schemas, needs, published kinds and claimed channels.
   The host answers metadata without running guest code, so a disabled plugin's kinds are listed, the card shows
   every need before code runs, and install refuses a bad declaration. Ports are static in 0.1.
4. **Event adapters poll** (every 50 to 1,000 ms; ≤ 256 events a poll, 200 a second, burst 1,000) and publish D9's
   normalised platform kinds, whose `platform` the host sets to the manifest's (never first-party) before the schema
   check, or their own `plugin.<id>.<kind>` through a new `DynEventKind` catalogue in `omuzaki-live-api`; events carry
   `Source::Plugin { id }` and non-droppable ones are retained per D8. BR-10's example is `fakechat`.
5. **Trackers overlay claimed channels.** Input is a downscaled frame (≤ 320 × 240) from a tap on the first-party
   tracker, only with the person's camera tick. Nothing is allocated per frame: a triple buffer in, a `track-out`
   import writing into a preallocated frame, `MailboxWriter` out. On `R`: tap ≤ 0.3 ms inside `track.face`'s 5 ms;
   guest ≤ 2.5 × 10⁶ fuel; tap to frame ≤ 3.2 ms p95. On a fault the claimed channels hold for `lost_after` (200 ms),
   then come from the first-party tracker, then the life layer as today.
6. **Exporters:** an additive run-time exporter catalogue in `omuzaki-io-api` (human lane). The host runs the rights
   preflight unchanged, hands the scene over as GLB (≤ 128 MiB), checks the returned files (≤ 16, ≤ 256 MiB, flat
   names with declared extensions) and `io.export` writes them under its own checks; a plugin never writes. Format ids
   are `plugin-<plugin>-<name>`, since `FormatId` is a slug without dots.
7. **Part generators:** `DynPartKind` and a run-time catalogue in `omuzaki-char-api` (human lane); a `lookup_kind`
   returning
   a `PartKindRef` over static and run-time kinds, which the build path moves to (`find_kind` goes after its
   callers move); ids
   `plugin-<plugin>-<name>`; schema-checked parameters, seeded and pure, so deterministic; part-local flat meshes
   (≤ 8, 65,536 vertices and triangles; rigid in 0.1) the host places and skins to the resolved attachment joint;
   `KindProvenance::External`, with a source row `Generated { tool: "plugin <id>@<version>", model: <hash> }` and the
   manifest's licence, so the licence gate treats the plugin as any source.
8. **No output world in 0.x:** video would copy about 249 MB/s into an instance with no shared memory, and data
   outputs need sockets, which plugins never get; protocol outputs are #1085's.
9. **Registration is all or nothing:** each catalogue reserves under one shared activation gate (fallible, invisible to
   readers); enable reserves every kind and prepares the tracker thread and instances, then opens the gate in one
   store, so every kind appears together and nothing after can fail; disable closes it first.
10. **A document naming a missing kind loses nothing:** parameters stay byte for byte, with a finding naming the plugin;
    nodes are inert, triggers never fire, parts don't build and their character can't be exported until the plugin is
    back, exporters are `not_found`, tracker channels fall back to first party.
11. **Never:** plugin code on the render, audio, capture or tracking thread; the GPU; a raw device; memory shared with
    the host; a file written by the plugin itself.

## Consequences

- Contract work, each its own task landing before its adapter (design §9): `omuzaki-plugin-api` gains the worlds and
  manifest sections (plugins area); `omuzaki-live-api` gains `DynEventKind` (automation, Director); `omuzaki-track-api`
  gains the frame tap (tracking, Director); `omuzaki-io-api` and `omuzaki-char-api` gain run-time catalogues (human
  lane, Director). Only `lookup_kind` is added beside `find_kind` (decision 7; callers move in their own task, then
  `find_kind` goes);
  `omuzaki-store-api` and `omuzaki-core` don't change.
- `automation.md` D9's comment that plugin adapters use `Source::Platform` is corrected by the `DynEventKind` task.
- #1366 keeps its names; its WIT moves to `omuzaki-plugin-api` (T1).
- Rejected: pushing adapters (a looping entry holds the worker); trackers on the tracking thread (CLAP's lesson);
  full-resolution or untapped camera access; a dotted format or part id (a core slug change for no gain); ports
  computed by guest code in 0.1; plugin video outputs; plugin-written files.
