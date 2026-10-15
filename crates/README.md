# Rust workspace

The Omuzaki app, as a Cargo workspace (spec v3 §9, §39). Members are globbed (`crates/*`), so adding a crate never edits
a shared file.

## Crate kinds and the dependency rule

| Kind | Names | May depend on |
|---|---|---|
| **API** (contracts) | `omuzaki-core`, `*-api` | `omuzaki-core`, other API crates, external crates |
| **Mock** | `*-mock` | API crates |
| **Lib** (format libraries) | crates with `layer = "lib"` under `[package.metadata.omuzaki]` | API crates and other lib crates (mocks as dev-dependencies) |
| **Implementation** | everything else under `crates/` | API crates and lib crates (mocks as dev-dependencies) |
| **App** | `omuzaki-cli`, `omuzaki-ui`, `omuzaki-test`, `omuzaki-stage`, `xtask` | anything: they wire implementations together |

`cargo xtask lint-deps` enforces the table (§39 rule 10). API crates change rarely and go through maintainer
review; breaking changes need a decision record (§39.2 item 8).

**Lib crates** (ADR 0011) hold code that several implementations need and that is too large to write twice, and that is
a *format* rather than behaviour: `omuzaki-gltf` is glTF 2.0 for both `omuzaki-io-gltf` and `omuzaki-io-vrm`. A lib crate
declares itself in its manifest, because no name says it:

    [package.metadata.omuzaki]
    layer = "lib"

Implementations, apps and other libs may depend on a lib; a lib may depend on contracts and other libs, never on an
implementation or an app. **Contracts and mocks never depend on a lib, not even as a dev-dependency.** A contract must
not pull real code in, and a mock is the independent oracle the real code is tested against (`omuzaki-io-gltf` is checked
against the mock's own GLB reader and writer); it stops being independent the moment it shares code with what it
checks. A lib crate stays a library: no registration with `inventory`, no global state, nothing that makes it an
implementation of a contract (the format crates that depend on it do that).

## Self-registration and `link()`

Ops, part kinds, templates, formats and the other registries fill themselves: a crate calls `register_<kind>!` next to
what it registers, and no list names it (§39 rule 2). A binary keeps a registration only if it links the crate, and on
wasm only if it calls into the crate's code, so the wiring is explicit (ADR 0015):

- a crate whose library registers anything defines its `link()` with `omuzaki_core::define_link!();` in `src/lib.rs`;
- a crate that depends on one calls `<crate>::link()`: from its own `link()` (`define_link! { omuzaki_char::link(); }`)
  if other crates link it, at startup if it is an App binary;
- each App has a link gate, `tests/registrations.rs`, that compares what the binary registered with what the source
  declares, by name. `cargo xtask registry --app <crate>` prints the expected list.

`cargo xtask lint-links` checks the wiring, and `cargo xtask check` runs it.

## Crates so far

| Crate | Kind | What it is |
|---|---|---|
| `omuzaki-core` | API | slugs and typed ids, deterministic RNG, clock, the sRGB `Colour`, the shared triangle `MeshData`, re-exported `glam` |
| `omuzaki-l10n-api` | API | localised text (SE-03, ADR 0023): `Text` and `Arg` (a message id with typed arguments), `Message`, `Locale` with the shipped list, derived message ids, the `messages!` and `register_locales!` macros, and the pseudo-locale transform |
| `omuzaki-doc-api` | API | the project document as a tree; `Patch` (the only way to change it), exact `inverse()` undo, conflict detection |
| `omuzaki-doc-mock` | Mock | in-memory document for tests |
| `omuzaki-doc` | Implementation | the project store (§7): a folder of TOML files that loads into the document tree and saves back, `Project` implementing the document traits, over `omuzaki-doc-api` |
| `omuzaki-render-api` | API | the renderer contract (§9, §10, §21): `Scene`, toon `Material` with outlines, face `Overlay`s and named shading programs, `Quality` (live, high, offline), the fixed frame graph with `FrameReport` timings, headless `render_image` |
| `omuzaki-render-mock` | Mock | deterministic CPU renderer, test `shapes`, and the `conformance` suite every renderer passes |
| `omuzaki-budget` | Implementation | the frame-budget scheduler (§8): smooths `FrameReport` times (EMA), compares them with the budget and steps `Quality` down the degrade ladder and back up with hysteresis, returning every change as a value; reports the face painter's budget pressure; allocation-free `observe` |
| `omuzaki-render` | Implementation | the wgpu renderer: `WgpuRenderer` implements the render contract headless or on an existing device, with the fixed frame graph one module per pass, per-pass CPU and GPU timings, and `PaintTarget::native()` for GPU painters; it shades toon materials (ramp with tinted shade, rim, skin, hemisphere, directional and point lights) over the surface colour; the post effects (bloom, depth of field, grain, grade) are in; shadows and blending arrive with their own tasks |
| `omuzaki-anim-api` | API | animation and rig (§8, §20, §22): the channel table and fixed-size `ChannelBuf`, the normalised `TrackFrame`, retarget and calibration, blending order and ownership, the life layer, the `Skeleton` (a VRM humanoid with its `HumanoidMap`, or generic for creatures), spring bones, scores |
| `omuzaki-anim-mock` | Mock | deterministic recorded-track replay, stand-in retarget, life layer and animator, fixtures (a humanoid with hair, a quadruped and a blob), and a wired `MockPerformer` |
| `omuzaki-ai` | Implementation | the model registry's implementation, so far the verified file cache `VerifiedCache` (the `FileSource` every pinned file comes from: ordered URLs with fall-through, bounded streamed SHA-256, resume from a kept partial file, atomic store), `sha256_file` and `model_ref`; design in `docs/design/ai-api.md` §8, ADR 0017 |
| `omuzaki-ai-ort` | Implementation | the `onnx` runtime (§16 ML-01): pinned Microsoft ONNX Runtime builds fetched through `FileSource` on first use, providers tried in order with the CPU last, no spin-waiting, custom-op domains and external data refused before ONNX Runtime sees the bytes; native only; design in `docs/design/ai-api.md` §9, ADR 0017 |
| `omuzaki-anim` | Implementation | animation (§20, §22): the ARKit-52 retarget and the life layer, over `omuzaki-anim-api` |
| `omuzaki-face-api` | API | the procedural face (§19, CB-20..25): `FaceMapping` (the ARKit-native bus to `PaintParams`, coefficients in the profile), `FaceProfile` and its eye, brow, mouth, nose and cheek designers, expressions and visemes, and the `FacePainter` contract with its vector `DrawList` and layer compositing |
| `omuzaki-audio-api` | API | the audio engine contract (§8, §9, §10; ADR 0018): devices and roles, the mixer `GraphSpec` with validation and the `egress_pairs` privacy closure, the `Node` / `NodeKind` contract, the exact fader, mute and mono-to-stereo semantics, taps and feeds over one SPSC ring, the engine clock, latency and health reports, the `AudioEngine` service and `OfflineRender`, and the conformance checks |
| `omuzaki-stream-api` | API | the recording and streaming base types (§25, §39.1; `docs/design/record.md`): `Ticks`, exact `Timebase::rescale`, `Packet` with side data, `ByteMeter`, `StreamError`; encoder, muxer and output traits follow |
| `omuzaki-face-mock` | Mock | a deterministic geometric `MockPainter`, a CPU scanline `MockRasterizer` (fills, clips, transforms, gradients, strokes), and an expression contact-sheet example |
| `omuzaki-char-api` | API | the procedural character (§18): ids, the part tree (`CharacterDef`), self-registering `PartKind`s, what parts publish to each other, `BuiltCharacter` with renderer-typed materials, the `PartRuntime` contract, variants as path overrides, the look (LK-06: a preset plus overrides, self-registering presets, the build-time material transform), and the head, hair and body parameters |
| `omuzaki-char-mock` | Mock | a small character built through the real part-kind contract (JSON fixtures, four `mock-*` kinds taking the real parameter types), a frame loop, and the end-to-end stage test against `omuzaki-render-mock` |
| `omuzaki-char` | Implementation | the procedural character (§18): geometry kit, body templates, head, hair, outfit, accessories, cloth, palette and the character ops; module map in the crate docs (`crates/omuzaki-char/src/lib.rs`), over the API crates only |
| `omuzaki-stage-api` | API | the scene contract (ADR 0024): `SceneDef` (settings, lighting, cameras, cast), ids, document paths, keys, limits and the one validator `check` |
| `omuzaki-history-api` | API | the journal and snapshot contract (§6, §7, §29; ADR 0021): `HistoryEntry` and the `JournalSink` the executor appends to, `HistoryExport` (undo and redo stacks as data), snapshot ids, refs and triggers, the `Snapshots` service, and `register_derived_path!` for caches that snapshots leave out; design in `docs/design/journal.md` |
| `omuzaki-history-mock` | Mock | a `RecordingSink` (scriptable `admit`, `wants_mark`, `sync`, `status`) and an in-memory `MemorySnapshots` |
| `omuzaki-store-api` | API | content hashes (BLAKE3), the `Store` trait, provenance records and the licence gate (§7, §40) |
| `omuzaki-library-api` | API | the library contract (§28, ADR 0019): `ItemId`, `Version`, `VersionReq`; the `ItemManifest` with its canonical BLAKE3 hash, `check` and publisher `verify`; `ItemRights`; pure recipe `expand` and `check_ops`; `ApplicationRecord`; `ItemSource` with `register_library_source!`; the `Library` service trait with `Query`, `SearchPage`, `ItemInfo`, `Facets`; append-only manifest `migrations`; design in `docs/design/library.md` |
| `omuzaki-http-api` | API | the one outgoing-HTTP contract (connectors D13): `HttpTransport` (`send`, `open` for streams), `HttpRequest`, `HttpResponse`, `HttpStream`, `HttpError`, and the `Cancel` handle with `NeverCancel` and `CancelFlag`; no network stack; the client is `omuzaki-http`, the fake `omuzaki-http-mock` |
| `omuzaki-http-mock` | Mock | the scripted HTTP transport (connectors D13): `FakeHttp` answers from routes (`Reply`: response, error, hang or stream of `Chunk`s) and hand-written cassettes, logs every call, and never opens a socket or sleeps; the one fake every HTTP caller tests against |
| `omuzaki-http` | Implementation | `HttpFetcher`, the HTTPS `Fetcher` over reqwest with rustls (the OS trust store): https only, at most five redirects (each https), three tries with backoff on connect errors, timeouts, 5xx and 429, `Range` resume with a checked `Content-Range`, streamed bodies with an idle read timeout; native only; `allow_loopback_http` is a test aid |
| `omuzaki-io-api` | API | glTF and VRM import/export (§27): importer and exporter traits that self-register by format, the glTF-shaped `IoScene`, VRM meta mapped to provenance, perfect-sync baking, validation reports; design in `docs/design/io-api.md` |
| `omuzaki-io-mock` | Mock | a small working GLB and VRM 1.0 importer/exporter with fixtures generated in code, and a baker that makes trivial morph targets |
| `omuzaki-gltf` | Lib | the shared glTF 2.0 format library: the document model (raw `extensions` and `extras` on every object), GLB and `.gltf` containers, accessor decode and a deterministic writer, and the document to `IoScene` mapping in both directions, with import and export hooks for formats built on glTF (VRM). Limits are checked before any allocation. Design in `docs/decisions/0011-format-library-layer.md` |
| `omuzaki-embed` | Lib | the build-script helper that embeds a data folder as a sorted table of (relative path, `include_str!`), so seed data is compiled in and wasm needs no file system (ADR 0019 D14); std only |
| `omuzaki-assets-lock` | Lib | the project's `assets.lock` file (spec v3 §7, git design §3.2): one row per content hash with its provenance; `AssetsLock::parse` and the canonical `to_text` (rows sorted by hash), the file's only reader and writer; `omuzaki-doc` re-exports it |
| `omuzaki-pixfmt` | Lib | camera pixel formats to packed RGB8 (PF-01): NV12, YUY2, BGRA and strided RGB in Q14 integer maths (BT.601 or BT.709, limited or full range, nearest-neighbour chroma) and MJPG through `zune-jpeg`, written into a caller buffer; safe Rust, no allocation per raw frame; no `omuzaki-*` dependencies, so the camera, the tracker and the UI preview share it |
| `omuzaki-vts` | Implementation | the VTube Studio plugin API as data (CN-13): the message `Envelope`, `ApiError` and `ErrorKind`, every request, response and event the bridge uses, and the validators and limits (`CustomParamName`, `ParamValue`, `FadeTime`); clean-room from the documented wire protocol, no I/O, native only |
| `omuzaki-io-gltf` | Implementation | plain glTF / GLB as format `glb`: the registration and the plain-glTF policy (VRM data is dropped with a warning, an avatar or `MToon` material on export is reported) over `omuzaki-gltf` |
| `omuzaki-io-vrm` | Implementation | VRM 1.0 and 0.x as formats `vrm1` and `vrm0` (EX-01): the VRM layer (meta and rights, humanoid, expressions, spring bones, node constraints, `MToon`, the 0.x conversion) over `omuzaki-gltf`; messy files import with findings, not failures |
| `omuzaki-io-ops` | Implementation | the op layer for model import and export (§6, §27): the `io.*` ops and the conversions between file types and the runtime character, face and animation types, over the API crates only; and platform export (ADR 0047): limit tables in `data/platforms/`, the fit tools and planner, self-registered platform profiles and the `platform.*` ops |
| `omuzaki-ops-api` | API | the op contract (§6): `Op`, `Plan`, scopes and grants, structured errors, self-registration with `register_op!`, and the `Runner` trait every surface (MCP, CLI, UI, scripts) calls |
| `omuzaki-test` | App | golden-image compare (tolerant, premultiplied), bless, evidence reports and PR comments, `verify` of manifests against the object store, and a branch-safe `gc` of unreferenced objects |
| `omuzaki-store` | Implementation | disk asset store: one atomic directory per object (data + provenance), first write wins, hash-verified reads, whole-store `verify` and a quarantining `repair` |
| `omuzaki-store-mock` | Mock | in-memory store, plus the shared `conformance` suite both stores pass |
| `omuzaki-ai-api` | API | the model registry and runtime (§16 ML-01, ML-13, ML-14): self-registered slots (versioned tensor signatures, call or stream mode, smoke and full evaluation), `model.toml` manifests (multi-graph `[[stages]]`) and model cards, `Session` and the real-time `BlockSink` / `BlockSource`, runtimes, SHA-256 pins with ordered URLs, the resumable `Fetcher` and verified `FileSource`, the SE-05 `ConsentLookup`, and the `ModelRegistry` service; design in `docs/design/ai-api.md`, ADR 0017 |
| `omuzaki-ai-mock` | Mock | a `fake` runtime and deterministic sessions (affine, delay line, scripted, crash on cue), an in-memory fetcher, the `test_echo@1` and `test_stream@1` slots with a shipped test model, and `InlineStream`, the reference stream with scripted late blocks |
| `omuzaki-llm-api` | API | not in the workspace yet (design): the LLM seam (CN-09, AG-01, AP-03): `LlmClient`, `TurnRequest` and the provider-neutral `ContentBlock`s, the pull-based `TurnStream` of `LlmEvent`s, `Usage` with `cost` in integer micro-dollars from a dated `PriceTable`, the offline pre-request `Ledger` bound, `LlmError`, `Cancel`, `register_llm_provider!`; design in `docs/design/llm-api.md`, ADR 0032 |
| `omuzaki-llm-mock` | Mock | not in the workspace yet (design): `FakeLlm` (scripted turns, chunking, usage and error injection on `FakeClock`; deterministic), CN-01's mock connector for the LLM capability, and the `conformance` suite every provider passes |
| `omuzaki-llm` | Implementation | not in the workspace yet (design): the Anthropic Messages and OpenAI-compatible chat providers (HTTP over the workspace `reqwest` and rustls stack shared with `omuzaki-http`, SSE, retries before the first event, cancellation within 50 ms), the price data, model listing and probing, and the MCP tool and result mapping; tested against a loopback fake server, no network in CI |
| `omuzaki-performer-api` | API | not in the workspace yet (designs): the AI performer's contract (`ai-performer.md`, ADR 0031) and its voice and face types: `VoiceAssignment`, `VoiceProvider`, `VoiceCatalog`, `VoiceLead`, `TalkState`, `MoveContext`, `StyleKnobs` and the per-frame `BigMovePlanner` (`ai-performer-voice-face.md`, ADR 0041) |
| `omuzaki-cli` | App | the `omuzaki` command: ops, help, run (dry-run/JSON), undo/redo, all generated from the op registry |
| `omuzaki-ops` | Implementation | the op executor (`Runner`): read scope on every run, transactions with owner-only commit/rollback, per-actor undo that never wedges, op log, events |
| `omuzaki-mcp` | Implementation | the MCP server (§11): every registered op is a tool, served over stdio or a local Streamable HTTP port; native only, over `omuzaki-ops-api`, `omuzaki-doc-api` and `omuzaki-store-api` |
| `omuzaki-rig` | Implementation | the rig (§8, §20): `HumanoidRig` solves VRM normalised humanoid poses onto any skeleton, whatever its bone axes (and reads them back), `RigSkeleton` holds the tables derived from the rest pose, `Skinner` fills `Scene::joints` with skinning matrices; no allocation per frame |
| `omuzaki-audio` | Implementation | the speech host (ML-03): `SpeechHost` takes 16 kHz mono samples through a lock-free ring (`AudioInput::push` never allocates), runs Silero VAD (`ort`, `load-dynamic`) and a streaming segmenter on a VAD thread, and transcribes utterances with Whisper (`whisper-rs`) on an ASR thread; bounded drop-oldest queues, events (speech start/end, partial and final transcripts, drops, errors), a configurable language, and a model catalogue (URL, size, SHA-256; never committed). Models via `OMUZAKI_SPEECH_MODELS` in tests |
| `omuzaki-track` | Implementation | the webcam face tracker (PF-01), ported from the `spikes/webcam-track` spike: MediaPipe's BlazeFace detector, Face Mesh V2 and Blendshape V2 graphs as `omuzaki-ai-api` sessions (`FaceGraphs`, three slot signatures), video-mode ROI tracking, One-Euro landmark smoothing and the Procrustes head-pose fit; `FaceTracker::track` allocates nothing after warm-up and returns a fixed-size `FaceSample`, which `to_track_frame` turns into an `anim-api` `TrackFrame`. Tested over `omuzaki-ai-mock` scripted sessions; the real models, camera thread and `Tracker` seam are separate work |
| `omuzaki-stage` | App | not in the workspace yet: the character stage, which uploads a `BuiltCharacter` to the renderer and runs the rig, the runtimes and the face painter each frame. It is wiring, not an API+impl pair, because the UI, the CLI, the web player and the golden runner all link the same wiring |
| `omuzaki-ui` | App | the desktop UI: `ds`, the design system (tokens and the three themes with a contrast test, fonts and Phosphor icons, the components with AccessKit labels and their own words in a `Strings` table with a pseudo-locale, a gallery binary that renders screenshots, runs in the pseudo-locale and measures frame time); panels and the shell arrive in their own modules |
| `omuzaki-agent-api` | API | not in the workspace yet: the in-app assistant's panel contract (`Assistant`, `UserMessage`, `ContextChip`, `AssistantEvent`, `HostError`) and `RationaleSource` for "Explain this"; design in `docs/design/agent-runtime.md`, ADR 0040 |
| `omuzaki-agent-mock` | Mock | not in the workspace yet: `ScriptedAssistant`, which plays an event script on `FakeClock` for panel tests and the gallery |
| `omuzaki-agent` | Implementation | not in the workspace yet: `AssistantHost`, the assistant's turn loop over an in-process MCP client and `dyn LlmClient`, the context builder and compaction, the host-local tools and the conversation store; links API crates and rmcp's client only, never `omuzaki-mcp` or `omuzaki-ops` |

The other API crates (§39.1) arrive in later phases, each with its mock.

**Planned: connectors** (ADR 0030, `docs/design/connectors.md` D1 and D13). `omuzaki-connect-api` (API: manifests,
requests, the `Connectors` service, the tray), `omuzaki-connect-mock` (Mock), `omuzaki-connect` (Implementation: the host
and the `connect.*` ops) and one `omuzaki-connect-<family>` Implementation per adapter family. HTTP has one contract,
`omuzaki-http-api` (API, built), with `FakeHttp` in `omuzaki-http-mock` and the one client in `omuzaki-http`, so
connectors, `net.http` and the LLM providers never link a second HTTP stack. `omuzaki-stage` wires the host into each
App's `Services`.

## Checking your change

    cargo xtask check --affected       # what CI runs, on the crates you touched (the default mode)
    cargo xtask check --affected --list  # just print those crates
    cargo xtask check --all            # the whole workspace
    cargo xtask lint-deps              # only the layering rule above
    cargo xtask lint-links             # only the link() wiring (ADR 0015)

`check` maps the files that differ from `--base` (default `origin/main`, via the merge-base; uncommitted and untracked
files count) to crates, adds every crate that depends on them (dev-dependencies too), then runs in order: `fmt --check`,
`clippy -D warnings`, `test`, `cargo deny check licenses bans sources`, `lint-deps`, `lint-links`. A change to `Cargo.toml`,
`Cargo.lock`, `deny.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml` or `.cargo/` checks everything; a change
outside every crate (docs, tools) runs nothing. It always runs every step and ends with a one-screen summary; the exit
code is non-zero if any step failed.

**Link gate only.** An App such as `omuzaki-cli` links every op crate only to carry their registrations (ADR 0015), so a
change to an op crate can break its link but not its behaviour. A crate lists those dependencies in its manifest:

    [package.metadata.omuzaki]
    link_only = ["omuzaki-io-ops", "omuzaki-stage"]

When the crate is in scope only through such dependencies (directly or through crates that depend on them), `check` runs
`fmt` and `clippy` on it and one extra step, `link-gate` (`cargo test -p <crate> --test registrations`), instead of all its
tests; the banner (stderr) prints it as `omuzaki-cli (link gate only)`, while `--list` stdout stays plain package names for scripts. A change to the crate itself, or to any
dependency not listed, or a dev-dependency on a listed one, still runs everything. Only list a dependency whose API the
crate's own source never calls (a bare `use x as _;` or the `define_link!` list does not count).

`lint-deps` classifies crates by name (table above), except that `layer = "lib"` in a manifest makes an implementation-named
crate a lib (an unknown layer, or `lib` on a contract, mock or app, is a violation), and checks normal and build
dependencies against it. Dev-dependencies follow the same table plus one widening: mocks. Output is one line per violation, `crate → dependency: rule`.

## Building

    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cargo build --target x86_64-pc-windows-gnu      # Windows, cross-compiled from Linux (needs mingw-w64)
    cargo build --target wasm32-unknown-unknown -p <crate>

## Fixtures

Golden files and parity fixtures are checked in. Several were made once by generator scripts from the earlier JavaScript prototype, which is not published; the tests compare against the checked-in files and never run those scripts.
