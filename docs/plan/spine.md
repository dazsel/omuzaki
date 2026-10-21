---
title: "The spine map: what each area builds first, end to end"
summary: "The spine of each area (the thinnest end-to-end path the product needs to exist), its exists-when test, the contracts between areas, and the open pull requests each owner inherits"
area: plan
---

# The spine map

How the work runs is in ADR 0067 and `docs/maintainers/operating.md`. This file is **what** gets built first: for
each area, the spine (the thinnest end-to-end path through it that the product needs in order to exist), the test that
says it exists, and what it hands the other areas. The Director keeps this map current; each owner edits its own
section's **Status** line as it lands work.

Everything in the spec still ships (spec §35). The spine comes first because a product that runs end to end can be
used, measured and improved; a set of finished parts can't.

## Where it stands (2026-10-21)

About 400,000 lines in 91 crates. Real and tested: the character kit's generators and 49 character operations
(`omuzaki-char`), the CPU face painter and 14 face operations (`omuzaki-face`), the op runner, undo and transactions
(`omuzaki-ops`), the MCP server, which turns every registered operation into a tool (`omuzaki-mcp`), the CLI
(`omuzaki run|undo|redo|mcp`), the Rhai script host, the webcam face tracker (MediaPipe-class, 52 blendshapes plus head
pose, `omuzaki-track`), the model registry and ONNX/LiteRT runtimes, VRM 0.x/1.0 parsing, the wgpu renderer,
project save and load, the content-addressed store and settings.

**What's missing is the joins.** The studio binary (`omuzaki-ui`) runs a null runner with no document and no
viewport, and the UI doesn't depend on the renderer. Nothing builds a real executor into the app, so an agent over MCP
and a person in the UI can't work on the same character. No rendered frame reaches an output (virtual camera, window,
stream). Journal, history and branches exist as contracts and mocks only. The live, performer, connector and secrets
contracts are in open pull requests.

## The whole: "Omuzaki exists" when

A person opens the studio and, in one session:
1. makes a character with the kit (or loads their own VRM), and sees it in the viewport;
2. edits it in the panels while an agent edits the same character over MCP, each change visible to both at once,
   each undoable;
3. saves, quits, reopens and finds it as they left it;
4. goes live: the webcam drives the face and head, the microphone drives the mouth, and the character reaches OBS
   through a virtual camera or a capture window, at the spec §10 budgets on a mid-range machine;
5. records a clip with transparency.

Every area's spine is a slice of that. When all seven pass, run the whole as one script plus one manual session, and
record it (a video and the numbers) on the coordination issue.

## The spines

Each section: what the owner owns (crates and anything new it creates), the exists-when test, the path in order, the
open pull requests it inherits, and the prior art to beat. **Inherited pull requests are the owner's to decide:** merge
as is, finish, fold into its own branch, or close with a one-line reason. None waits for a review.

### 1. App core and the agent surface

**Owns:** `omuzaki-ops*`, `omuzaki-mcp`, `omuzaki-cli`, `omuzaki-script`, `omuzaki-http*`, `omuzaki-core`,
`omuzaki-doc*`, `omuzaki-history*`, `omuzaki-store*`, `omuzaki-settings*`, `omuzaki-identity*`, `omuzaki-update*`,
`omuzaki-library*`, `omuzaki-l10n-api`, `omuzaki-assets-lock`, `omuzaki-git-api`, `omuzaki-test`, `omuzaki-measure`,
`omuzaki-metrics-api`, `xtask`, the plugin crates.

**Exists when:** one in-process **app core** (executor + open document + event bus + job manager) runs inside the
studio; the MCP server attaches to that same core (stdio and streamable HTTP), so an agent's operation shows up in the
UI at once and a person's panel edit shows up in the agent's log; dry-run, transactions, undo and redo work from both
sides; the journal is written as operations land, so a crash loses nothing and reopening restores the document and its
history; autosave and recovery work.

**Path:** the app-core crate and its handle (contract below) → executor sessions, approvals and budgets (#2229) →
MCP attached to the running app → the journal writer and reader (#2149) → real snapshots and the history sink, undo
and redo across restarts → autosave and crash recovery → branches (#2200) → long jobs with progress and cancel
(#1973) → the assistant host (#2125) → plugins (#1999).

**Inherits:** #2229, #2149, #2200, #1973, #2125, #1999, #2079, #2152, #1956, #2190, #2156.

**Beat:** the MCP reference servers and Blender's operator/undo model (every action an operation, every operation
undoable and scriptable).

### 2. The studio UI

**Owns:** `omuzaki-ui*`, `docs/design/ux/`.

**Exists when:** `omuzaki-ui` launches into a studio a first-time user understands without a manual: the viewport
shows the character (rendered by `omuzaki-render` into an egui texture) with orbit, pan and frame; the outliner shows
its parts; the inspector edits any part through its operation's schema; the command palette finds every registered
operation; the history panel shows who did what (person or agent) with undo and redo; the agent panel shows an agent's
activity and approval requests; the Live workspace starts tracking and output. Every panel edit is an operation, never
a direct write.

**Path:** the app core in place of the null runner → shell reads the panel registry (#2245) → viewport host (#217) →
outliner (#218) and schema inspector with the edit panels (#2164) → command palette (#153) → history with undo (#446)
→ agent panel and approval cards (#390, #420) → Live workspace and output window (#2144) → first run.

**Inherits:** #2245, #2164, #2144.

**Beat:** the editing feel of Blender and VRoid Studio, the clarity of Figma's panels; the taste bar is restraint:
one obvious way to do each thing, nothing on screen that doesn't earn its place.

### 3. Character: body, rig and motion

**Owns:** `omuzaki-char*`, `omuzaki-rig`, `omuzaki-anim*`, `omuzaki-test-char`.

**Exists when:** the kit builds a complete default character with no stand-in parts (body and head kinds, hands,
hair, an outfit), rigged on the VRM humanoid skeleton with spring bones that hold at 60 Hz; it reaches the renderer as
an avatar (`omuzaki-avatar-api`); poses, the ARKit-52 retarget and the life layer move it from a tracking frame.

**Path:** an avatar from a kit character (the char → avatar join) → real body, head and hand kinds (#1944, #158) →
hair generators (#2227), caps and physics → garments (#2095), cloth at runtime (#2121) → the rig runtime (#2113) →
retarget and life tuning (#2167) → export to VRM with spring chains.

**Inherits:** #1944, #2227, #2095, #2121, #2113, #2167, #2157.

**Beat:** VRoid Studio for the kit's range and finish; three-vrm (MIT) for spring-bone behaviour.

### 4. Face

**Owns:** `omuzaki-face*`, `omuzaki-test-face`.

**Exists when:** the painted face rasterises on the GPU inside the spec §10 budget (0.5 ms), every ARKit-52 channel and
the anime extras visibly move it, lids, lashes and closed eyes read right, and the expression library holds at least
18 expressions; the renderer samples it as a texture on the head.

**Path:** the GPU rasteriser (#1433, A1 to A6, B1) → the face-to-render texture join (#151) → lash line, lids and
closed eyes (#167) → the channel sweep and coverage tests (#319, #171) → the expression range test and library (#170,
#320) → the known mouth jumps (#836, #880).

**Inherits:** none of its own; works with #2211 (GPU expressions) through the Render owner.

**Beat:** Live2D Cubism's expressiveness for a 2D-read face; Vello for GPU vector rasterising.

### 5. Render, stage and import

**Owns:** `omuzaki-render*`, `omuzaki-stage*`, `omuzaki-io*`, `omuzaki-gltf`, `omuzaki-avatar*`, `omuzaki-take-api`,
`omuzaki-pixfmt`, `omuzaki-budget`, `omuzaki-link-*`, `omuzaki-camera-mf`, `omuzaki-test-stage`, the release zip.

**Exists when:** the renderer draws the kit character and an imported VRM (bring your own model) with toon and MToon
shading, outlines, GPU morphs and expressions, lights and framing; the stage runs the perform loop at a fixed 60 Hz
from a tracking frame to a posed, rendered frame inside the §10 budgets; the frame is available as a GPU texture for
the UI viewport and for outputs.

**Path:** render a kit avatar → import a VRM as an avatar (#2214) → GPU morphs and VRM expressions (#377, #2211) →
the perform loop drives the scene from a tracker (#314, #306) → light rigs and framing (#130, #131) → scene and render
ops (#275, #336) → the native-texture handle for outputs (#629) → FBX import (#2202).

**Inherits:** #2214, #2211, #2202, #1941, #2235.

**Beat:** three-vrm and UniVRM for VRM fidelity; VSeeFace for the live look and cost.

### 6. Tracking, voice and models

**Owns:** `omuzaki-track*`, `omuzaki-test-track`, `omuzaki-ai*`, `omuzaki-embed`, `omuzaki-llm*`, the performer
crates, voice and speech.

**Exists when:** the webcam drives the character's face and head live (52 blendshapes plus head pose) within 5 ms p95
for tracking; calibration takes one guided step; iPhone ARKit and VMC input work as alternatives; the microphone
drives the mouth (voice activity plus visemes); models install through the registry with their licences checked.

**Path:** tracker frames into the stage (with the Render owner) → the 5 ms budget (#903) → calibration and the rest
pose (#542, #536) → curves, smoothing and life (#474, #475, #477) → iPhone ARKit and VMC ingest → voice activity and
visemes for lip sync (#464, with Face) → model ops (#687, #857, #2126) → the AI performer contract (#2176), after the
spine.

**Inherits:** #2126, #2176, #2199, #1513, #1982, #1987, #1989, #2215, #1981, #1984.

**Beat:** VSeeFace and VTube Studio for tracking feel; MediaPipe's own demos for accuracy.

### 7. Broadcast and live

**Owns:** `omuzaki-stream*`, `omuzaki-audio*`, `omuzaki-obs`, `omuzaki-vts`, and the live, secrets and connector
crates.

**Exists when:** the rendered frame reaches OBS through a virtual camera and a capture window with transparency, at
the §10 latency; recording with alpha works; the audio path carries the microphone; OBS can be controlled over its
WebSocket; Twitch or YouTube events trigger an automation (one platform is enough for the spine).

**Path:** the frame from the stage (#629, with Render) → the stream service and A/V timeline → the window output (the
plan exists) and the virtual camera → encode and record with alpha (decide the encoder's licence route, #1424) → the
live supervisor and watchdog → OBS WebSocket control (#600) → platform events and the automation graph (#2147,
#2132, #2098).

**Inherits:** #2147, #2132, #2098, #1946.

**Beat:** OBS's own outputs for reliability; VTube Studio's and VSeeFace's virtual cameras for latency.

## Contracts between areas

These joins cross areas. The owner named first builds the contract; the others consume it. Additive changes are the
building owner's call; a change that breaks another area's code is agreed between the two owners and recorded on the
coordination issue, and the Director settles a disagreement.

| Contract | Built by | Used by | What it carries |
|---|---|---|---|
| App core handle | App core | UI, MCP, CLI, Broadcast | the executor, the open document, the event stream of applied operations, jobs |
| Avatar | Character | Render, UI | a rigged, renderable character from the kit or an import (`omuzaki-avatar-api`) |
| Face texture | Face | Render | the painted face as a GPU texture plus its channel inputs |
| Tracking frame | Tracking | Render and stage, Character | blendshapes, head pose and timestamps per frame (`omuzaki-track-api`) |
| Rendered frame | Render | UI, Broadcast | the stage's output as a GPU texture with its timing (`omuzaki-render-api`) |
| Audio and visemes | Broadcast (audio) | Tracking, Face | microphone frames, voice activity and visemes (`omuzaki-audio-api`) |

`omuzaki-core` and the `*-api` crates stay contracts (AGENTS.md, layering); `cargo xtask lint-deps` still holds.

## Order

All seven start together. The joins that unblock the most come first in each path: the app core handle, the avatar
from a kit character, the rendered frame as a texture, and the tracking frame into the stage. Shared root files
(the workspace `Cargo.toml`, `Cargo.lock`) take line additions from anyone; merge `main` before you push.

## After the spine

When an area passes its exists-when test, its owner says so on the coordination issue with the evidence (a video, the
numbers). The area then gets one deep audit (correctness, security, performance), the findings are fixed, and the
leaves start: the rest of that area's spec, in the order of spec §35.
