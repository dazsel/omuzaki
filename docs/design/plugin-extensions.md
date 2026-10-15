---
title: "Plugin extensions: nodes, event adapters, trackers, exporters, part generators and commands (PL-03)"
summary: "the extension worlds that sit on the plugin host (ADR 0064): one world and one instance kind per extension, kinds declared in the manifest, the run-time catalogues in live-api, track-api, io-api and char-api, the tracker hand-off, budgets per kind, the missing-plugin rules, and the tasks this unlocks."
area: design
---

# Plugin extensions: nodes, event adapters, trackers, exporters, part generators and commands (PL-03)

**Status:** proposed. **Feature:** PL-03 (and BR-10's plugin adapters). **Decision record:**
[ADR 0064](../decisions/0064-plugin-extensions.md). **Spec:** v3 §30 (PL-03), §25 (BR-10, BR-26), §22 (PF-01), §10
(`track.face`), §39 rule 2. **Builds on:** `plugins.md` and ADR 0058 (the host, the base world, the sandbox),
`automation.md` D9 and D15, `track-api.md` §3, §8 and §9, `io-api.md` D4, D5 and D10, `omuzaki-char-api`'s `PartKind`,
ADR 0015 and ADR 0054. Not designed here: the plugin store (#1084, ADR 0065) and bridges (#1085, ADR 0066).

## 1. Prior art

Read for ideas only; no code is taken from any of them.

| System | Licence | What it gets right (and we take) | Where ours must do better |
|---|---|---|---|
| Zed extensions | API crate Apache-2.0, editor GPL-3.0 | wasmtime components; a WIT package per release with host adapters for older ones; the manifest says what an extension provides, so the editor lists it without running it | Zed gives each capability its own typed function and runs extensions on its async executor with no real-time contract or per-call fuel. Ours: one instance kind per extension with its own thread, fuel and deadline (§3) |
| Extism | BSD-3-Clause | any guest language, a small bytes ABI, host functions | no component model, so no WIT records or versioned imports (rejected in ADR 0058) |
| OBS plugin modules | GPL-2.0-or-later | one module registers many kinds (sources, outputs, encoders, services) in `obs_module_load`, each as an info struct of callbacks: a kind is data plus entry points | native code in the process: full access, `video_render` runs on the graphics thread, a crash takes OBS down. Ours: no plugin code on the render, audio or capture threads; a trap faults one plugin |
| Bevy plugins | MIT OR Apache-2.0 | `Plugin::build(&self, &mut App)` groups every registration of a feature in one place | compile time only: no unload, no isolation. Ours add and remove at run time, in a fixed order, all or nothing (§5) |
| Godot GDExtension | MIT | registers classes into `ClassDB` per initialisation level and removes them in reverse; a scene that names a missing class loads it as `MissingNode` / `MissingResource`, keeping its properties so saving loses nothing | a native library with no sandbox. Ours keep the same no-loss rule (§6) inside a wasm sandbox |
| CLAP audio plugins | MIT | an explicit thread contract: the audio-thread `process` may not allocate or block, and a thread-check extension proves it | the plugin still runs on the host's real-time thread, so one slow plugin glitches the whole graph. Ours never runs on a host real-time thread: a slow tracker plugin loses only its own frames (§4.3) |

## 2. The worlds

Each extension is a world in `omuzaki:plugin@0.1.0` that `include`s the base `plugin` world (`plugins.md` §1.1) and
exports one interface. The manifest's `worlds` key names them by short key; a component that offers several targets
its own world that includes each (`world dice { include node-plugin; include command-plugin; }`), and the host checks at
install that the component exports every interface the listed keys need (`PluginError::Imports` otherwise). The WIT
lives in `omuzaki-plugin-api/wit/`, one file per world, beside `plugin.wit`.

```wit
interface node-cx {           // NodeCx one to one (automation D4); imported by node instances only
    use types.{op-error};
    variant val { event, flag(bool), number(f64), text(string), items(string), json(string) }   // items, json: JSON text
    now: func() -> f64;   utc: func() -> s64;   random: func() -> u64;     // the node's own D6 stream
    input: func(port: string) -> result<val, string>;
    set: func(port: string, value: val) -> result<_, string>;
    fire: func(port: string) -> result<_, string>;
    fail: func(port: string, why: string) -> result<_, string>;
    wake-after: func(seconds: f64) -> result<u64, string>;   wake-at: func(at: f64) -> result<u64, string>;
    cancel-wake: func(id: u64);
    run-op: func(op: string, args: string) -> result<u64, string>;       // the only effect (automation D7)
    var: func(name: string) -> option<val>;   set-var: func(name: string, value: val) -> result<_, string>;
    group-enabled: func(group: string) -> option<bool>;   switch-group: func(group: string, on: bool) -> result<_, string>;
    emit: func(kind: string, payload: string) -> result<_, string>;
}
interface node {
    use node-cx.{val, op-error};
    variant input { start, stop, event(string), trigger(string), wake(u64), reply(tuple<u64, result<option<string>, op-error>>) }
    resource instance {
        constructor(kind: string, params: string);                       // params already checked by the host
        handle: func(input: input) -> result<_, string>;
        eval: func(port: string) -> result<val, string>;
    }
}
interface adapter {
    record event { kind: string, origin-id: string, utc: option<s64>, payload: string }
    poll: func() -> result<list<event>, string>;
}
interface tracker {
    enum pixels { luma8, rgb8 }
    record image { seq: u64, time: f64, width: u32, height: u32, format: pixels, data: list<u8> }
    record head { yaw: f32, pitch: f32, roll: f32, position: option<tuple<f32, f32, f32>> }   // HeadPose
    record eyes { left-yaw: f32, left-pitch: f32, right-yaw: f32, right-pitch: f32 }          // EyeAngles
    record tracked { found: bool, head: option<head>, eyes: option<eyes> }   // flat: lifting it allocates nothing
    frame: func(input: image) -> tracked;
}
interface track-out {         // imported by tracker instances only; writes into the preallocated TrackFrame
    blend: func(channel: u8, value: f32);                                  // checked when `frame` returns (§4.3)
}
interface exporter {
    record output-file { name: string, bytes: list<u8> }
    export: func(format: string, glb: list<u8>, options: string) -> result<list<output-file>, string>;
}
interface part {
    record mesh { name: string, positions: list<f32>, normals: list<f32>, uvs: list<f32>, indices: list<u32>, colour: u8 }
    record build-input { kind: string, params: string, seed: u64, quality: u8, palette: list<u32> }   // rgba8 per slot
    // part-local space: the host places the mesh at the resolved attachment (§4.5)
    build: func(input: build-input) -> result<list<mesh>, string>;
}
interface command { use types.{op-error}; run: func(command: string, args: string) -> result<string, op-error>; }

world node-plugin { include plugin; import node-cx; export node; }      world adapter-plugin { include plugin; export adapter; }
world tracker-plugin { include plugin; import track-out; export tracker; } world exporter-plugin { include plugin; export exporter; }
world part-plugin { include plugin; export part; }                      world command-plugin { include plugin; export command; }
```

## 3. Instances, threads and budgets

Every extension runs in an instance of the plugin's one compiled component, each with its own `Store`, so no two
kinds share guest memory and data can't pass between them except through the host. "Base only" means the base imports
of `plugins.md` §1.1 under the plugin's ceiling; "pure" means `ops`, `data` and `http` are linked to refusals, `log`
works, and clocks and randomness are the call's own seeded inputs (as node instances, `plugins.md` §4.1). **Pure
instances never run the `lifecycle` exports:** the host instantiates them and calls only their extension export, so
`init`, `approval-resolved` and `shutdown` run once, on the worker, under its imports. A pure call gets everything it
needs in its arguments (the frame, the GLB and options, the parameters); state set up by `init` is never there.

| Key | Instance | Thread | Imports | Memory | Fuel, deadline per call | CPU share |
|---|---|---|---|---|---|---|
| `node` | node instance (exists) | the show's engine thread | pure + `node-cx` | 16 MiB | 10⁶ (≈1 ms on `R`), 2 ms | the plugin's 100 ms/s |
| `adapter` | worker | `plugin:<id>` | base only | 64 MiB | `poll` 10⁸ (≈100 ms), 1 s (`fetch` time counts) | the plugin's 100 ms/s |
| `command` | worker | `plugin:<id>` | base only, inside the caller's grants (ADR 0058 d. 11) | 64 MiB | 10⁹, 30 s (`plugins.md` §4.3) | the plugin's 100 ms/s |
| `tracker` | tracker instance | `plugin-track:<id>` (dedicated) | pure; `time` is the frame's | 16 MiB | `frame` 2.5 × 10⁶ (≈2.5 ms), 5 ms | its own 150 ms/s |
| `exporter` | a fresh export instance per job | the `io.export` job's `Cpu` thread | pure | 512 MiB | 10¹¹ (≈100 s), 120 s and the job's cancel | the job's |
| `part` | a fresh part instance per build (§4.5) | the character builder's thread (authoring plane) | pure; seed from `BuildCtx::rng` | 64 MiB | `build` 10⁸ (≈100 ms), 250 ms | the builder's |

- **Why pure instances for trackers, exporters and parts:** a camera frame, a whole avatar as GLB, or a character's
  parameters go in, and channels, files or a mesh come out; with no `http`, `data` or `ops` linked, the input has no
  way out but the return value, which the host checks. That is a stronger promise than a grant.
- **Tracker share:** 2.5 ms a frame at 60 fps is 150 ms/s, more than a worker's 100 ms; the tracker gets its own share
  so a busy adapter can't starve it. The export instance is outside the 256 MiB resident plugin cap (`plugins.md`
  §5.2): it lives for one job, one plugin export runs at a time, and its 512 MiB is checked against free memory first.
- **Never, for any kind** (ADR 0064 decision 11): the render, audio, capture or tracking thread, the GPU, a device
  (camera, microphone, MIDI, HID, serial), memory shared with the host, or a file the plugin writes itself.

| Path | Budget on `R` | Statistic | Gate | On breach |
|---|---|---|---|---|
| Node activation | 10⁶ fuel, 2 ms | per call | `plugins.md` §4.3 calibration | node fault (D5) |
| Adapter `poll` + publish of 256 events | ≤ 2 ms host side, excluding the guest | p95 | bench `adapter_poll_256`, nightly | reported |
| Tracker tap, downscale to 320 × 240 on the tracker thread | ≤ 0.3 ms, inside `track.face`'s 5 ms | p95 | `track.face` bench with a tap on | reported; the tap is dropped first |
| Tracker tap to published `TrackFrame` | ≤ 3.2 ms (guest ≤ 2.5) | p95 | bench `track_plugin_frame`, nightly | out of fuel or deadline: trap, fault (§4.3) |
| Tracker host-side allocations per frame | 0 | max | counting-allocator test | test fails |
| Export of a 20 MiB GLB, host side (GLB write + file checks) | ≤ 400 ms | worst of 5 | bench `export_plugin_20mib` | reported |
| Part build, host side (instantiate + lift + checks + placement + `MeshData`) for 65,536 vertices | ≤ 5 ms | p95 | bench `part_lift_64k` | reported |

## 4. Each extension

### 4.1 Nodes: D15 confirmed

- **D15 stands:** `DynNodeKind` objects in the engine's catalogue, ids `plugin.<plugin>.<name>`, the engine thread,
  10⁶ fuel and 2 ms per activation, effects only through `run-op` in the show's session ∩ the plugin's ceiling ∩ the
  needs the show approved. The adapter is `PluginNodeKind` in `omuzaki-plugin` (#1366), registered through the engine's
  `NodeCatalogue`.
- **The manifest declares every node** (`[[nodes]]`, §4.7): name, summary, class, ports, `listens`, `needs` and the
  parameter schema. `PluginNodeKind` answers `meta`, `schema`, `defaults`, `ports`, `listens` and `needs` from it
  without running guest code, and `check` validates against the schema. So the editor lists a disabled plugin's
  nodes, the approval card shows every need before code runs, and an install refuses an id, port name or schema that
  is wrong. The cost: ports are static in 0.1 (none computed from parameters); a later minor may add an export.
  A missing plugin leaves the node inert with the finding `plugin.missing` (§6).

### 4.2 Event adapters (BR-10's plugin adapters)

- **Polling, not pushing.** The host calls `poll` on the worker every `poll_ms` (manifest: 50 to 1,000, default 250).
  A guest has no threads or sockets, so a push would be one entry looping on `fetch` that holds the worker forever and
  blocks reload, `shutdown` and commands. A poll is a short transactional entry (`plugins.md` §2.2).
- **What the host does with each event, in order:** the kind must be in the manifest's `publishes`; at most 256 events
  a poll and 16 KiB a payload (more is a bad return value: nothing is published and the plugin faults); for a
  normalised platform kind it sets the payload's `platform` to the manifest's, which may not be a first-party one
  (`twitch`, `youtube`, `bilibili`, `tiktok`), so a plugin can never pass as Twitch; **then** the final payload must
  pass the kind's schema, and it builds `LiveEvent { source: Source::Plugin { id }, acting: None, .. }`. A plugin's
  own kind gets no `platform` (its `Source` names the plugin); install refuses an own-kind schema that declares one.
  One `Source` for plugins:
  D15's `Source::Plugin { id }` wins over D9's comment on `Source::Platform`, which task T2 corrects.
- **Kinds a plugin may publish:** the normalised platform kinds of D9 (`chat.message`, `follow`, `sub`, `sub_gift`,
  `cheer`, `tip`, `redeem`, `raid`, `power_up`, `poll`, `prediction`, `hype_train`), and its own
  `plugin.<plugin>.<kind>` with a schema and `droppable` in the manifest, added at enable through `DynEventKind` (T2).
  Never `hotkey`, `voice.command`, `obs.*`, `show.*`, `var.changed`, `engine.tick` or `custom.*`. The card lists each
  kind: "posts tips into your shows as `kick`".
- **Rate cap:** 200 events a second, burst 1,000, per plugin. Over it, droppable events are dropped and counted; the
  rest go to the adapter's retained buffer of 1,000 (D8's producer rule, which the host implements for every plugin),
  retried on D8's backoff. A full buffer drops the oldest and counts `dropped.producer`.
- **The worked example** (`fakechat`): no account and no service. At `init` it writes `chat.jsonl` (ten lines) into
  its data folder unless one is there; each `poll` returns the next line as a `chat.message` with platform
  `fakechat`, at most one a second. A person edits that file to script a chat. Manifest: `data = true`, no `net`.

### 4.3 Trackers

- **Overlay, not replacement.** A tracker plugin claims channels in its manifest (ARKit names, `head`, `eyes`; all of
  them is allowed) and produces `TrackFrame`s for those only. It rides the first-party tracker's camera session: the
  camera is opened once, by the tracker host.
- **Input:** a downscaled copy of each camera frame (`luma8` or `rgb8`, at most 320 × 240) from a **tap** on the
  first-party tracker (T5), and only if the person ticked "see your camera" on the card for this plugin (SE-04); off
  by default and never full resolution. The tracker instance is pure, so the pixels can't leave it.
- **The hand-off, newest wins, nothing allocated:** the tap is a triple buffer allocated when the plugin starts; the
  tracker thread writes into it and moves on. The `plugin-track:<id>` thread takes the newest frame, lowers it (into
  guest memory through the guest's own `realloc`), calls `frame`, and each `track-out.blend` call writes one channel
  straight into a preallocated `TrackFrame`; `tracked` is a flat record, so lifting it allocates nothing either. When
  `frame` returns the host checks the frame (channel indices < 52, values finite and clamped to each channel's range,
  at most 52 calls, unclaimed channels refused) and publishes it through `MailboxWriter`. The stage polls that mailbox
  beside the first-party one and overlays the claimed channels (T7). A frame that arrives while the guest runs
  replaces the waiting one and is counted as skipped.
- **One frame, on `R`, 30 fps camera, 120 Hz stage** (the bench `track_plugin_frame` checks these):

```
 0.00  frame N reaches the tracker thread
 0.00-0.30  tap: downscale to 320x240 luma8 into the free buffer, publish seq N  (tracker thread, inside track.face)
 0.30-5.00  first-party inference; TrackFrame N published by 5.00 at the latest   (tracker thread)
 0.30-0.35  plugin-track:dice unparks, takes seq N                               (plugin thread from here)
 0.35-0.40  lowers 76,800 bytes (fuel charge 2,000 + 76,800)
 0.40-2.90  guest frame(): at most 2.5 x 10^6 fuel, its track-out.blend calls included (2,000 each)
 2.90-2.95  lift the flat record, check the TrackFrame, MailboxWriter::publish
 8.33  stage tick: polls both mailboxes, takes jawOpen and tongueOut from the plugin's frame N, the rest from N
```

- **A fault:** the plugin's frames stop. The stage keeps the plugin's last values while they are younger than the
  tracker's `lost_after` (200 ms), then uses the first-party values for those channels; when the first-party frame
  is `Lost` too, the life layer takes over as today (`track-api.md` §8). The plugin restarts on `plugins.md` §4.4's
  backoff, and three faults in ten minutes disable it.

### 4.4 Exporters

- **A run-time exporter catalogue in `omuzaki-io-api`** (T8, the export area's contract, human lane): additive, so
  `register_exporter!` and `FormatInfo` don't change. `exporters::register_dyn(desc: FormatDesc, exporter: Arc<dyn
  Exporter>) -> Result<DynRegistration, IoError>`; dropping the registration removes it; `find_format(id)` and
  `formats()` see static and run-time ones. `FormatDesc` is `FormatInfo` with owned strings. Run-time ids must
  start `plugin-` and may not shadow a static one.
- **Format ids are `plugin-<plugin>-<name>`,** not the dotted form: `FormatId` is a slug (`^[a-z0-9][a-z0-9_-]{0,63}$`)
  and plugin ids have no `-`, so the dashed form is unambiguous and needs no core change. `<name>` is at most 24
  characters, so the id fits.
- **The pipeline:** `io.export { format: "plugin-…" }` runs the static checks and the rights `preflight` unchanged
  (D5, D10), writes the scene as GLB with the first-party `glb` exporter (at most 128 MiB), then calls the guest in
  a fresh export instance. The host checks the result: at most 16 files and 256 MiB together, flat names
  `[a-z0-9._-]{1,64}` with an extension the manifest declares, no duplicates. Then `io.export` writes them to the
  destination the person chose under its own scope and path checks; a plugin never writes. Exporting to a publish
  target is a `publish` op under the per-use rule (ADR 0058 decision 5).
- **Options** are JSON checked against the manifest's `options_schema` before the call. The licence record gains the
  plugin's row (`plugins.md` §8).

### 4.5 Part generators

- **A run-time part-kind catalogue in `omuzaki-char-api`** (T10, the character area's contract, human lane):
  `DynPartKind` (the `PartKindRegistration` functions as `&self` methods, with owned `PartMeta`), registered with
  `part_kinds::register_dyn(Arc<dyn DynPartKind>) -> Result<DynRegistration, BuildError>`. Ids are
  `plugin-<plugin>-<name>`, for the same reason as formats.
- **One lookup for both kinds (Director sign-off: an existing signature changes):** `find_kind` and `kinds` return
  `PartKindRef`, a cheap handle over a static `&'static PartKindRegistration` or an `Arc<dyn DynPartKind>` with the
  same methods, so `build_part`, `recolour_part`, planning, `requires_of` and every part op find plugin kinds through
  the path they use today. T10 changes the lookup and its `omuzaki-char-api` callers; T16 moves the callers in
  `omuzaki-char`, `omuzaki-io-ops` and `omuzaki-stage` (about 25 sites) before any plugin part registers.
- **Parameters** are JSON checked against the manifest's schema (`check` normalises defaults); **seeded and
  deterministic:** the seed is a `u64` drawn from `BuildCtx::rng` (character seed, part id), and the part instance is
  pure, so the same character builds the same mesh on every machine. Pure does not mean stateless (a guest may bump
  a global on every call), so **every build gets a fresh instance** from the plugin's `InstancePre` on wasmtime's
  pooling allocator, which resets linear memory and globals to the module's initial image; nothing carries from one
  build to the next. The test builds A, B, then A again, with a WAT guest that counts calls in a global.
- **Mesh data over WIT** as flat arrays: at most 8 meshes and 65,536 vertices and 65,536 triangles in all; three
  floats per position and normal, two per UV, indices in range, every float finite; `colour` is a palette slot.
  The host builds `MeshData` and `PartOutput`, renormalises normals, and refuses anything else with
  `BuildError::Part` naming the plugin. 0.1 parts are rigid, with no joints, weights, textures or spring chains.
- **Placement is the host's:** the guest builds in part-local space; the host resolves `BuildCtx::attach` against
  `published` anchors and `BuildCtx::joint` as first-party accessories do, and transforms positions and normals.
  `attach` is already in the part's build key, so editing it rebuilds; a plugin part with no `attach` is refused
  with `BuildError::Part` ("plugin parts need an attachment").
- **Provenance:** `KindProvenance::External`, always, and `DynPartKind::provenance_record()` gives the record its
  output carries: `Source::Generated { tool: "plugin <id>@<version>", model: "<content hash>", .. }`, the manifest's
  licence, the publisher as author. `licence.derive` uses it for a part with no source row (`io-api.md` D10), as it
  uses `ProjectOwned` today, and a recorded row still wins (T11). So the licence gate treats an unknown or
  non-commercial plugin licence as it treats any such source. No store-api change.

### 4.6 Outputs: no output world in this version

- **Video:** 1080p30 RGBA is about 249 MB/s, and the component model has no shared memory, so every frame would be
  copied into the instance: about 25 ms a second of copying on `R` before the guest works, a quarter of its share.
- **Data outputs** (channels to OSC or VMC) need sockets, which plugins never get (ADR 0058); protocol outputs are
  first-party and #1085's. Reopen when a measured need and a sandboxed transport both exist.

### 4.7 The manifest

```toml
worlds = ["node", "adapter", "tracker", "exporter", "part", "command"]   # the keys of §2; any subset

[[nodes]]                             # id plugin.dice.roll; names are [a-z][a-z0-9_]{0,23}
name = "roll"
summary = "Roll dice from a chat command."
class = "active"                      # trigger, pure or active (automation D4); triggers add `listens`
ports = [{ name = "in", dir = "in", type = "event" }, { name = "text", dir = "in", type = "text" },
         { name = "total", dir = "out", type = "number" }, { name = "out", dir = "out", type = "event" }]
params = { type = "object", properties = { sides = { type = "integer", minimum = 2, maximum = 100, default = 6 } } }
needs = []                            # D11 needs, approved per show

[adapter]                             # own kinds: [[adapter.kinds]] { name, droppable, schema } -> plugin.<id>.<name>
platform = "fakechat"                 # never a first-party platform
poll_ms = 1000
publishes = ["chat.message"]

[tracker]
channels = ["jawOpen", "tongueOut"]   # ARKit names, "head", "eyes"
pixels = "luma8"                      # needs the person's camera tick

exporters = [{ name = "obj", title = "Wavefront OBJ", extensions = ["obj", "mtl"], media_type = "model/obj",
               options_schema = { type = "object" } }]                                  # format id plugin-dice-obj
parts = [{ name = "ring", category = "accessory",                                      # kind id plugin-dice-ring
           params = { type = "object", properties = { radius = { type = "number", minimum = 0.001, maximum = 0.2 } } } }]
commands = [{ name = "reroll", scope = "perform", args = { type = "object" } }]       # plugin.run (ADR 0058 d. 11)
```

At most 32 nodes, 8 event kinds, 4 exporters, 16 part kinds and 64 commands a plugin; every summary and title has a
Fluent id `plugin-<id>-<key>-<name>`.

## 5. Registration lifecycle

Four run-time catalogues take plugin kinds: the engine's (node and event kinds, `omuzaki-live-api`), the stage's
tracker sources (`omuzaki-track-api` taps), the exporters (`omuzaki-io-api`) and the part kinds (`omuzaki-char-api`).
Each catalogue's `register_dyn` has two steps: it **reserves** (fallible: checks ids, collisions and schemas, holds
the ids, publishes nothing) and returns an inactive `DynRegistration` whose `activate()` cannot fail and is one
atomic snapshot swap. Lookups skip reserved entries; collision checks count them.

- **Enable:** compile, start the worker, `init` returns `ok`; then reserve every kind in every catalogue. **All or
  nothing:** if any reservation fails (an id collision, a schema that does not compile), the reservations are
  dropped, which no reader ever saw, the plugin is `Faulted { reason }` and the finding names the kind. Only when
  all are reserved does the host activate, in order, event kinds, node kinds, part kinds, exporters, then start the
  tracker thread and adapter polling; nothing after the first activation can fail. Kinds that others depend on come
  first (a node may listen to the plugin's own event kind).
- **Disable, unload, remove:** the reverse: stop polling (retained events are dropped and counted), stop the
  tracker (the stage falls back as in §4.3), cancel a running plugin export (the job ends `cancelled`, writes
  nothing), then remove exporters, part kinds, node kinds and event kinds. Events of a removed kind still in the
  inbox are dropped and counted. `shutdown` runs last (`plugins.md` §2.2).
- **Reload:** remove and add as above, the new hash's manifest deciding what comes back. A kind with the same id
  rebinds: nodes swap between activations, a running part build finishes on the old instance.

## 6. Documents that name a missing kind

The rule for every kind (Godot's lesson): **nothing stored is lost.** Parameters stay byte for byte in the document,
saving and syncing don't touch them, and a finding names the plugin by id and version.

| Kind | While the plugin is missing, disabled or lacks the kind |
|---|---|
| Node | inert with the finding (D15); the graph keeps its wires |
| Event kind | triggers listening to `plugin.<id>.<kind>` stay and never fire; finding on each |
| Part | not built; the character builds without it and shows the finding; `io.check_export` and `io.export` refuse with `plugin.part_missing`, so a character never ships silently without a part |
| Exporter | not listed; `io.export` with its id is `not_found`, hint "install or enable `<plugin>`" |
| Tracker | the stage uses first-party channels; a finding in the tracking panel |

## 7. Tests

The conformance suite (`omuzaki-plugin-mock::conformance`) gains a `MockGuest` per world, so each case runs on the mock
host and on wasmtime with WAT fixtures. Node: #1366's acceptance, and a node id colliding with a first-party one is
refused. Adapter: 300 events in one poll publish none and fault "bad return value"; a forged `platform: "twitch"` is
replaced before the schema check; an undeclared kind is refused with a `warn` line; a stalled `EventSink` keeps 1,000
non-droppable events and delivers them in order. Tracker: no frame reaches the guest without the camera tick; index 52,
an unclaimed channel or a NaN refuses the frame; a guest burning 10⁷ fuel faults at 2.5 × 10⁶ while first-party frames
keep their timing; after a trap the claimed channels hold 200 ms, then fall back; no host allocation per frame.
Exporter: `../x`, `a/b` and two `x.obj` are refused, as is 257 MiB of output; a cancelled job writes nothing; a failed
preflight stops before the guest runs. Part: two builds with OS entropy reseeded are identical, as are A, B, A from a
guest that counts calls in a global; 65,537 vertices, an index out of range or a NaN are refused; editing the attachment
moves the mesh; a part with no source row derives the plugin's licence. A pure instance never runs `init`. Lifecycle: a
collision in the third catalogue leaves the first two unchanged and no reader ever saw the plugin's kinds; disable
removes in reverse; a document round-trips byte for byte while its plugin is missing.

## 8. Worked example: one plugin node in a three-node show

`dice` 0.1.0, worlds `["node"]`, the `[[nodes]]` entry of §4.7. The show (first-party parameters abridged;
`chat.send` stands in for BR-10's send op, which has no name yet):

```
on.event { kind: "chat.message", filter: "!roll" } ──out──► plugin.dice.roll { sides: 6 } ──out──► op.run {
    text ─────────────────────────────────────────────────► text          total ──────────► op: "chat.send",
                                                                                            args: { text: "rolled {total}" } }
```

The guest's `handle` on `event("in")` reads `text` ("!roll 2d6"), draws two numbers from `random`, sets `total` and
fires `out`. Three messages through `NodeHarness` give this fuel report (`PluginNodeKind`'s per-kind counters; fuel is
deterministic, so the numbers are golden):

```
node              activations  fuel max  fuel p95  cap        share of cap  faults
plugin.dice.roll  3            41,210    41,210    1,000,000  4.1 %         0
```

41,210 fuel is about 0.04 ms on `R` and includes the import charges (`plugins.md` §4.3: 2,000 each, plus bytes):
`input` once, `random` twice, `set` and `fire` once each.

## 9. Unlocked tasks

Filed by the lead after review. Each fence adds its tests; "pr-check" means `tools/ci/pr-check.sh origin/main` green.

| # | Title | Fence | Contract | Acceptance | Blocked by | Model |
|---|---|---|---|---|---|---|
| T1 | `omuzaki-plugin-api`: the extension worlds and manifest sections | `crates/omuzaki-plugin-api/wit/*.wit`, `src/manifest/extensions.rs` | §2's WIT; §4.7's sections with `check` | every world parses with `wit-parser`; 12 bad manifests (bad id, 33 nodes, first-party platform, `hotkey` in `publishes`, 25-char name, …) each give one named finding; pr-check | #1082 | sonnet |
| T2 | `omuzaki-live-api`: `DynEventKind` and the run-time event-kind catalogue (cross-area: Director) | `crates/omuzaki-live-api/src/event/dynamic.rs`; D9's `Source::Platform` comment in `automation.md` | `register_dyn` (reserve), `DynRegistration` (`activate`, drop removes) | §5's reserve and activate; add and remove are atomic; a run-time kind can't shadow a static one; 10⁵ lookups during swaps never see half a set; pr-check | #773 | sonnet |
| T3 | Re-cut comment on #1366 | the issue only | WIT in `omuzaki-plugin-api` (T1), not `omuzaki-plugin`; §4.1 | posted | this design | haiku |
| T4 | `omuzaki-plugin`: the event adapter and `fakechat` | `src/adapter.rs`, `tests/adapter.rs`, `examples/plugins/fakechat/` | §4.2 | §7's adapter rows; `fakechat` replays ten lines in order at 1 a second on `FakeClock`; bench `adapter_poll_256` ≤ 2 ms p95 | T1, T2, T13, #1330 | sonnet |
| T5 | `omuzaki-track-api`: the frame tap (cross-area: Director) | `src/tap.rs` | `TapSpec { width ≤ 320, height ≤ 240, pixels }`, `TapReader` (triple buffer, newest wins) | 0 allocations after creation; the reader never sees a torn frame in 10⁶ swaps; the mock tracker honours it | — | sonnet |
| T6 | `omuzaki-plugin`: the tracker adapter and its bench | `src/tracker.rs`, `benches/track.rs` | §4.3 | §7's tracker rows; `track_plugin_frame` ≤ 3.2 ms p95 on `R` (scaled from `Dev`) | T1, T5, T13 | sonnet |
| T7 | `omuzaki-stage`: overlay plugin tracker frames | `src/track/overlay.rs` | claimed channels, 200 ms hold | a plugin frame overrides only its channels; the hold and fallback of §4.3 on `FakeClock`; no allocation per tick | T5 | sonnet |
| T8 | `omuzaki-io-api`: the run-time exporter catalogue (human lane) | `src/format.rs` (additive), `src/format/dynamic.rs` | §4.4, §5 | `formats()` lists both kinds; reserved ids are invisible until `activate`; a static id can't be shadowed; drop removes; pr-check | — | sonnet |
| T9 | `omuzaki-plugin`: the exporter adapter | `src/exporter.rs`, `tests/exporter.rs` | §4.4 | §7's exporter rows; `export_plugin_20mib` host side ≤ 400 ms | T1, T8, T13 | sonnet |
| T10 | `omuzaki-char-api`: `DynPartKind`, `PartKindRef` and the run-time part-kind catalogue (human lane, Director) | `src/kind/dynamic.rs`, `src/kind.rs`, `src/build.rs` and the crate's other `find_kind` callers | §4.5, §5 | `build_part` and `recolour_part` build a dynamic kind through `find_kind`; reserved ids invisible until `activate`; ids can't shadow; drop removes; pr-check | — | sonnet |
| T11 | `omuzaki-char` and `omuzaki-io-ops`: missing plugin kinds and plugin provenance | `omuzaki-char` `src/build/missing.rs`; `omuzaki-io-ops` the export check and `licence.derive` | §4.5 provenance, §6 part row | a character with a missing kind builds without it, round-trips byte for byte and fails `io.check_export` naming the plugin; a part with no row derives the plugin's MIT record, an unknown licence stays most restrictive | T10, T16 | sonnet |
| T12 | `omuzaki-plugin`: the part adapter | `src/part.rs`, `tests/part.rs` | §4.5 | §7's part rows; `part_lift_64k` ≤ 5 ms p95 | T1, T10, T13, T16 | sonnet |
| T13 | `omuzaki-plugin`: the registration lifecycle | `src/extensions.rs` | §5 | §7's lifecycle rows with fake catalogues | T1, #1567 | sonnet |
| T14 | Example plugins and a golden show | `examples/plugins/{dice,blink,objexport,ring}/` (built and committed, each under 200 KB) and a golden show | one per kind | each loads in CI; the §8 show's trace and fuel report match the golden on two runs | T4, T6, T9, T12, #1366 | haiku |
| T15 | Authoring docs, one page per kind | `docs/plugins/*.md` | — | each page builds its example; `lint-docs-index` passes | T14 | haiku |
| T16 | `omuzaki-char`, `omuzaki-io-ops`, `omuzaki-stage`: callers of `find_kind` take `PartKindRef` | the `find_kind` call sites in those crates | §4.5 | a dynamic kind adds, plans, describes and derives a licence through the existing ops; no behaviour change for static kinds; pr-check | T10 | sonnet |
