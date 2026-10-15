# `omuzaki-ai-api`: the model registry and runtime contract

**Status:** accepted (2026-10-12); the review answers are folded in (section 16). **Crates:** `crates/omuzaki-ai-api`, `crates/omuzaki-ai-mock`. **Decision record:** ADR
0017 (`docs/decisions/0017-model-registry.md`). **Feature:** ML-01; also ML-13 (ADR 0016) and ML-14.
**Spec:** v3 §16 (ML-01 to ML-14), §10 (`installer.size`, `cpu.total`, `startup.live`), §8 (threads), §39.1 rules 1, 2
and 6. **Builds on:** the speech host's catalogue, the font catalogue and `CachedDownloader`, the
shared downloader, licence records, ADR 0018 and `docs/design/audio-api.md` (the `model.stream` node).

## 1. What this contract is for

Every model the app runs (speech to text, voice activity, text to speech, trackers, voice to face, voice conversion)
needs the same things around it: a fixed interface, pinned files that download on demand, a licence record, an
evaluation, a runtime with GPU and CPU paths, a fallback when it is missing or misbehaves, and, since ADR 0016, a way for
a person to put their own model in its place. Today two crates have each built a piece of that: the speech host pins
its models in `catalog.rs`, and the sheet layout pins fonts and downloads them with its own `CachedDownloader`.

This crate is the one agreement, so that:
1. **slot owners** (`omuzaki-speech`, `omuzaki-track`, `omuzaki-voice`, `omuzaki-audio`) declare an interface and get sessions,
   without knowing about downloads, licences, providers or sandboxes;
2. **runtimes** (`omuzaki-ai-ort` for ONNX, whisper.cpp in `omuzaki-speech`) turn files into sessions, without knowing
   about slots' meaning;
3. **the registry** (`omuzaki-ai`) does everything in between, once;
4. **ops, the UI and agents** see models as plain data and can't do what ADR 0016 reserves for a person.

What it deliberately does not do: implement a registry, a runtime, a downloader or a sandbox. Those are tasks (section
15). It holds only the pure helpers that pin a meaning: signature matching, input checks, manifest validation, the digest
text, stream latency arithmetic, metric floors.

## 2. The pieces

```text
 register_slot!     the crate that owns the data       Slot: info (mode, deadlines), signature, evaluate
 register_model!    the crate that uses a shipped model ShippedModel: models/<id>.model.toml
 register_runtime!  omuzaki-ai-ort, omuzaki-speech, ...    Runtime: probe, load
            │
            ▼
 ModelRegistry (omuzaki-ai) ── FileSource (verified cache) ── Fetcher (app's HTTPS client)
   choose → verify / download → probe (sandbox for user models) → evaluate → load
            │                                          │
            ▼                                          ▼
   Box<dyn Session>  (request/response)        Stream { BlockSink, BlockSource }  (real-time blocks)
   handed by the app wiring to the subsystems; ops (model.*) and the Models page read ModelEntry / SlotStatus
```

| Who | Implements | Uses |
|---|---|---|
| slot owner | `Slot` + `register_slot!`; shipped manifests + `register_model!` | `Session` or `BlockSink`/`BlockSource` handed in by the app |
| runtime crate | `Runtime` + `register_runtime!` | `FileSource` for its native libraries |
| `omuzaki-ai` | `ModelRegistry`, `FileSource`, the slot session, the stream host, the sandbox | everything registered |
| app layer | `Fetcher` (HTTPS) | `ModelRegistry::open` / `open_stream`, then wires sessions into subsystems |
| ops, UI | nothing | `ModelRegistry` as an ops service (`ctx.service::<dyn ModelRegistry>()`) |

## 3. Slots

A **slot** is a versioned interface: `SlotRef` = id + interface version (`vad@1`). Its `Signature` lists named input and
output tensors (`TensorSpec`: name, `DType`, shape of `Dim::Fixed(n)` or `Dim::ANY`). A new interface is a new
registration (`asr@2` beside `asr@1`), never an edit: models and recorded takes name the version they were made for.

The crate that owns the slot's data registers it (`register_slot!`), so there is no central slot list (§39 rule 2). A
slot also says, in `SlotInfo`:
- **mode:** `Call { stall_ms }` (one `Session::run` per request, on the caller's thread) or `Stream { queue,
  stall_blocks }` (real-time blocks, section 10); a streaming slot may also limit a model's `StreamSpec`
  (`Slot::check_stream`: voice conversion refuses blocks over 100 ms);
- **`without_model`:** what the app does when nothing can run, in words for the health meter;
- **`threads`:** the default for its sessions.

**Matching a model to a slot** (`Signature::accepts`) is by port **name**, not order, after the manifest's `io` table
renames the model's ports (`Signature::with_slot_names`). Element types and ranks must be equal. For an input, a model
dimension that is `any` takes the slot's fixed size, but a fixed model dimension can't take a slot's `any`. For an
output the model chooses, so its `any` is checked on every run. Extra model outputs are ignored; extra model inputs are
refused (nothing would feed them). Every mismatch is reported at once, in words.

**The first slots.** Signatures are drafts here; each is fixed by its owner's registration task and then never changes.

| Slot | Mode | Owner | Signature (draft) | Shipped default | Without a model |
|---|---|---|---|---|---|
| `vad@1` | call | `omuzaki-speech` | `input f32[1,576]`, `state f32[2,1,128]`, `sr i64[]` → `output f32[1,1]`, `stateN f32[2,1,128]` (Silero's graph; state travels in tensors) | `silero-vad` 6.2.3 | speech isn't detected: captions and AI listening are off |
| `asr@1` | call | `omuzaki-speech` | `pcm f32[any]` (16 kHz mono, ≤ 30 s), `language u8[any]` (BCP 47, empty = detect) → `text u8[any]` (UTF-8), `language u8[any]` | `whisper-base-en` (runtime `whisper-ggml`) | live captions are off |
| `voice_conversion@1` | stream (queue 2) | `omuzaki-audio` | `audio f32[any]` (one block at the model's rate), `pitch f32[]` (semitones) → `audio f32[any]` (same length) | **none** (research `2026-10-voice-changers.md`) | the voice stays dry (or the BR-24 chain) |
| `voice_to_face@1` | stream | `omuzaki-voice` | `audio f32[any]` (16 kHz block) → `channels f32[52]` (ARKit order, `omuzaki-anim-api`) | Audio2Face-3D after legal review, an ort-only fallback beside it | lip sync from visemes (FE-08) |
| `tts@1` | call | ML-04's crate | `text u8[any]`, `voice u8[any]` → `pcm f32[any]` (24 kHz) | Kokoro or Chatterbox | hosted TTS, or text only |
| `face_tracker@1` | call | `omuzaki-track` | `image u8[any,any,3]` (RGB) → `blendshapes f32[52]`, `head f32[4,4]`, `score f32[]` | MediaPipe face (runtime `onnx`, its graphs as `[[stages]]`) | phone tracking, or the life layer alone |
| `hand_tracker@1`, `pose_tracker@1` | call | `omuzaki-track` | image → landmarks (shapes with ML-02) | once their licences are recorded (ML-02) | the pose library and procedural motion |

Streaming speech to text (GPU first) arrives as its own slot (`asr_stream@1`) with the same stream shape.

**Multi-graph models.** A model made of several graphs (MediaPipe face: detector, landmarks, blendshapes) lists them as
`[[stages]]` in its manifest. Each stage has a name, a graph file and its input wiring: a slot input by name, or an
earlier stage's `<stage>.<port>`. The `io` table maps each slot output to a `<stage>.<port>`. The `onnx` runtime runs
the stages in order, as a pipeline of sessions behind one `Session`, so the slot sees one model and the manifest
carries the wiring as data, with no runtime per model. Glue that isn't a graph (MediaPipe's crop between detector and
landmarks) is converted into a small graph of its own, so it runs as a stage. A probe of a staged model reports the
slot inputs its stages read and every stage output as `<stage>.<port>`; `validate` checks the stage names, files,
wiring order and `io` targets.

## 4. Manifest and model card

One format, `model.toml`, for shipped models and for user models. `ModelManifest::from_toml` / `to_toml` read and write
it; the app writes one for a user model that came without one (licence unknown).

```toml
id = "silero-vad"                  # user models: user-<name>; they can never shadow a shipped id
version = "6.2.3"                  # major.minor.patch, no suffixes
name = "Silero VAD"
slot = "vad@1"
runtime = "onnx"
min_app_version = "0.1.0"          # older apps list it as disabled, with the reason
providers = ["cpu"]                # best first; a shipped model always keeps cpu

[[files]]                          # first file is the main one
name = "silero_vad.onnx"           # letters, digits and ._+- only; what runtimes look files up by
sha256 = "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
size = 2327524
urls = ["https://raw.githubusercontent.com/snakers4/silero-vad/5cd79456.../silero_vad.onnx"]  # shipped only; tried in order

# [[stages]]  name, file, inputs = { graph_input = "slot_port" | "<earlier stage>.<port>" }: multi-graph models
# [io]   slot port name = model port name where they differ (with stages: slot output = "<stage>.<port>")
# [stream]  sample_rate, block, latency: streaming slots only (section 10)

[card]
register = "Silero VAD"            # the Item of its row in docs/licensing/register.md (shipped only)
authors = "Silero Team"
opset = 16
# imitates = "<person>"            # a real person's voice or likeness: needs their consent record (SE-05)

[card.licence]
spdx = "MIT"                       # or text = "...", url = "..."; all empty = unknown

[[card.lineage]]                   # training_data | base_model | code | conversion
kind = "code"
name = "silero-vad v6.2.3"
licence = "MIT"

[[card.metrics]]
name = "golden_max_abs_diff"
value = 6e-8
better = "lower"
set = { id = "aesop-52-frames-1", kind = "real" }
```

- **Origin is never written in the manifest.** The registry decides it from where the manifest came from: a
  `register_model!` registration is `Shipped`; anything a person adds is `User`.
- **`validate(origin)`** checks what needs neither the slot nor the bytes. Shipped models need at least one `https://`
  URL per file (all of them `https://`), an SPDX licence, a register row and a CPU path. User models need a `user-` id and no URLs: we never download
  them (ADR 0016 rule 2). Both need safe, unique file names, non-zero sizes, sane stream numbers and a non-empty `io`
  table. The registry adds the checks that do need the slot or the bytes: the signature, mode against `stream`,
  `min_app_version`, sizes and hashes.
- **Identity.** `ModelRef { id, version, digest }`. The digest is the SHA-256 of `digest_text()`: one line
  `<sha256> <size> <name>` per file, sorted by name. It covers every file and nothing else, so editing the card keeps a
  model's identity and changing one byte of one file changes it. Selections, sessions, takes and licence records all
  store a `ModelRef`.
- **SHA-256, not the store's BLAKE3**, because pins must be checkable against what upstreams publish (Hugging Face LFS,
  release checksums), and both existing catalogues already pin SHA-256 (ADR 0017).
- **Shipped manifests are files**: `crates/<owner>/models/<id>.model.toml`, registered with
  `register_model!("<id>", include_str!("../models/<id>.model.toml"))`. The registry checks the literal id against the
  manifest. Several versions of one model may be registered; the newest the app can run is the default and the others
  are rollback targets. Being files, they are readable by the repo licence gate without a build.

## 5. Licence records

- **Shipped models are registered.** Each manifest names its `docs/licensing/register.md` row in `card.register`, and
  its lineage lists weights, training data and code. A gate (a task in section 15) fails CI when a shipped
  manifest's row is missing or not `yes` / `conditions`. Nothing ships whose chain isn't written down: this is why
  `voice_conversion@1` ships empty.
- **User models carry what their author declared** (`Licence`: SPDX, words or a URL; all empty is "unknown") plus the
  person's **acknowledgement**: one per digest, recording who accepted which version of `DISCLAIMER` (the exact ADR
  0016 text is a constant in the crate, `DISCLAIMER_VERSION` counts its revisions).
- **What a model makes carries it.** `ModelEntry::made_with()` gives `MadeWith { model, origin, licence }`, which
  licence records store. Export warns when a user model's licence is non-commercial or unknown; classifying
  SPDX ids is the licence-records work, not this contract's.
- **A real person's voice or likeness** (`card.imitates`) needs that person's consent record (SE-05) before the model can
  be selected: its status is `NeedsConsent` until then.
  - Each slot declares in `SlotInfo::likeness` what its models reproduce: `VoiceClone` (text to speech),
    `VoiceConversion` (ML-14) or `Face`. `None` means models there can't imitate anyone, and one that claims to is
    refused.
  - The registry asks a `ConsentLookup` (`find(person, likeness) -> Option<Consent>`, the `omuzaki-store-api` record
    with `Likeness::scope()` as its scope).
  - The trait is in this crate because the registry consumes it. The records live with the licence records in the
    project document, and the app implements the lookup over them.
  - Until then the registry runs with `NoConsentRecords`, so every imitating model stays unusable, the safe default.
  - What an imitating model makes carries the consent record in its licence record.

## 6. Evaluation: smoke tests and the eval suite

`Slot::evaluate(session, env, level)` is the one hook:
- **`EvalLevel::Smoke`** runs in the app in seconds, on a few pinned public-domain or synthetic items, with floors. A user
  model must pass it before it is enabled (ADR 0016 rule 4), and so must a shipped model after it is installed or
  upgraded.
- **`EvalLevel::Full`** is the suite behind the model card and the gates (QG-09), run by `omuzaki-test` nightly, never in
  the app.

An `EvalReport` is a list of `Check`s: a `Metric` (name, value, which way is better, `TestSet { id, kind }`) and an
optional floor. It passes only if it has at least one check and all of them pass; a NaN never does.
- **Real and synthetic results are never averaged together.** Every metric names its test set and that set's
  `TestSetKind` (`Real` or `Synthetic`). Gates that decide defaults read real sets only (spec ML-01: "real-illustration
  and real-webcam test sets are kept apart from synthetic ones").
- **`EvalEnv`** gives an evaluation verified fixtures (`files()`, the same pinned download path as models) and the
  slot's shipped default of another slot (`shipped(slot)`). Text to speech is checked by transcribing its output with
  the shipped speech recogniser, and voice conversion the same way.

The first smoke tests, owned by the slot's crate:

| Slot | Smoke test | Floor |
|---|---|---|
| `vad@1` | a labelled synthetic set (tones, noise, silence, spliced public-domain speech) | frame accuracy (the slot owner sets it) |
| `asr@1` | a few short LibriVox clips with their texts | WER ≤ the slot owner's floor |
| `voice_conversion@1` | convert a public-domain clip, transcribe the output with `asr@1` | WER of the output against the text ≤ floor (intelligible) |
| `tts@1` | speak fixed sentences, transcribe with `asr@1` | WER ≤ floor |
| trackers | synthetic faces (our renders) | blendshape error ≤ floor |

## 7. Choosing and falling back

For each slot the registry runs the first usable model in this chain: the explicit model a caller asked for (a
character's own voice), else the person's selection, else the slot's shipped default (the newest installed shipped
model the app can run). If none works, the slot's `without_model` applies and `open` returns `NoModel`.

`ModelStatus`, first match wins:
1. `Disabled { reason }`: failed checks, a failed smoke test, a crash or stall, files that changed, an app too old;
2. `NotInstalled`: shipped, not downloaded yet;
3. `NeedsAcknowledgement`: a user model the person hasn't acknowledged;
4. `NeedsConsent`: it imitates a real person with no consent record;
5. `Ready`.

**The slot session.** `ModelRegistry::open` returns a session that *follows the slot*:
- a new selection is loaded off-thread and swapped in between runs;
- a crash or a stall past `stall_ms` disables the model for this session, reports it on the health meter, and falls
  back down the chain;
- while a replacement loads, `run` returns `RunError::Unavailable` (skip, like a dropped frame), never blocks for the
  load.

Callers hold one session for as long as they need the slot and never re-implement fallback.

**People choose and agents ask** (ADR 0016 rule 5). `select(slot, model, by)` refuses an agent's switch to an
unacknowledged user model (`NeedsPerson`). `acknowledge` is called only by surfaces where a person acts, and its op
refuses agents. `select(slot, None, _)` goes back to the shipped default, so it is always one call away.

## 8. Download on demand, verification and rollback

One path for every pinned file: model weights, runtime libraries (ONNX Runtime per platform and provider), eval
fixtures and the sheet's fallback fonts. Nothing is in the installer that can come later, so it stays under 150 MB
(`installer.size`, QG-06).

- **`PinnedFile { urls, sha256, size }`**: each URL names an immutable revision, and they are tried in order. The
  upstream's pinned URL comes first; a dead link, a wrong length or a wrong hash falls through to the next, and
  `FetchError::AllFailed` lists every attempt. A Omuzaki mirror, if the maintainers decide to host one, is one more URL in the
  manifests: no code change.
- **`Fetcher`** (the app's HTTPS client) opens a URL from a byte offset (`open(url, from)`, an HTTP range
  request) and says where the body really starts (`Body::start`: 0 when the server ignored the range). Nothing below
  the app links a network stack.
- **Resume for big files:** a file of `RESUME_MIN_SIZE` (500 MiB) or more keeps its partial download across failures
  and restarts and continues it with a range request. The whole file is hashed at the end; a mismatch deletes it and
  downloads once more from the start, and a second mismatch fails loudly (`FetchError::Hash`). Smaller files restart
  from zero.
- **`FileSource::get(pin, progress)`**: `omuzaki-ai`'s verified cache. It is the existing `CachedDownloader`, generalised:
  - reads at most `size + 1` bytes, refuses another length before hashing, refuses a hash mismatch;
  - stores by hash with an atomic rename, so concurrent first uses never see a partial file, and returns the path;
  - on later calls, a cached file whose size or modification time changed since it was stored is hashed again, and
    fetched again if it fails. Whole-file re-hashing on every start would cost `startup.live` up to half a second for a
    large model.
- **`FileSource::lookup(sha256)` and `forget(sha256)`**: the registry rebuilds a lost state file from `lookup`
  and deletes a removed model's blobs through `forget`, so no caller knows the cache's folder layout. `lookup` never
  downloads and treats a failing blob as absent (deleting it); its I/O errors are errors. `forget` is idempotent.
- **Install** (`ModelRegistry::install`): fetch every file, verify, run the smoke test, and only then make the version
  the default. A failed or cancelled install replaces nothing.
- **Rollback.** The version an upgrade replaces stays installed until the next upgrade. The registry rolls back on its
  own when the new version fails to load or fails its smoke test, and a person (or `model.rollback`) can roll back at any
  time. The registry keeps every installed manifest in its own state file, so it can roll back to a version the
  current app no longer registers.
- **User models are imported:** their files are copied into the cache when added (hashed once), so a moved or edited
  original changes nothing, and the sandbox reads only the cache.
- **Where things live** (registry task): `<app data>/cache/sha256/<hex>` for blobs, shared with fonts;
  `<app data>/models/state.toml` for installed versions, user manifests, acknowledgements, selections, evaluation
  results and health. These are person-level, not project, data.

## 9. Runtimes: ONNX Runtime, providers, threads, the sandbox

**Runtimes** implement `probe` (read the model's own signature and opset without running it) and `load` (a session that
presents exactly the slot's signature, through the manifest's `io` renames). The first ones:
- **`onnx`** in `omuzaki-ai-ort`: `ort` (`load-dynamic`), ONNX Runtime fetched through `FileSource` on first use. The
  flavour per platform: the CPU build; the `DirectML` build on Windows; the CUDA build when the person opts in; Core ML in
  the macOS build; the WebGPU build where none of these exist. Each is a pinned file with a register row.
- **`whisper-ggml`** in `omuzaki-speech`: whisper.cpp through `whisper-rs`, presenting `asr@1`'s tensors.

**Providers.** `SessionOptions.providers` is the manifest's list filtered by the person's settings. The runtime tries
them in order, skips what the machine lacks, always ends on the CPU, and reports the one it got in `SessionInfo`. The
CPU path is mandatory for shipped models.

**Threads and sessions.**
- **The caller owns the thread that calls `run`:** a tracker thread, a speech worker, a stream worker. It is never the
  audio callback or the render thread, since a run takes milliseconds and the runtime may allocate inside it.
- **Sessions are `Send`, not `Sync`.** `run(&mut self)` on one thread at a time; `reset` clears carried state.
- **Runtimes own their maths threads**, `SessionOptions::threads` per session, inter-op 1, and **never spin-wait**:
  `cpu.total` counts every cycle. ONNX Runtime's environment is created once per process.
- **The registry owns** loader threads (swaps), stream workers (section 10) and sandbox processes, and caps the sum of
  live sessions' threads.

**The sandbox for user models** (ADR 0016 rule 4).
- **Shipped models run in-process.** Every user model, from its first probe, runs in a separate worker process
  (`omuzaki-model-worker`, an App crate linking the runtime crates) with a memory cap, lower CPU priority and no network
  stack.
- **The worker refuses** ONNX custom-op domains, custom-op libraries, external data outside the model's own files, and
  any native code a model asks to load (`LoadError::Refused`).
- **Tensors cross the boundary** (pipes first, shared memory if measurements ask for it), so the session proxy is
  just another `Session`.
- **A run past the slot's deadline is a stall:** the worker is killed, and the slot session falls back and says so.
- **On the web player** (no processes), user models are not offered.

## 10. Streaming slots (ML-14, streaming speech to text, ML-05)

Some models are fed from a real-time thread one block at a time, and that thread must never wait for them:

```text
 real-time thread            worker thread (omuzaki-ai)                  real-time thread
 BlockSink::push(k) ──▶ bounded queue (queue blocks) ──▶ Session::run ──▶ BlockSource::take(k) → Ready | Missed
```

- **`StreamSpec`** in the manifest declares the model's `sample_rate`, `block` (samples) and own `latency` (samples of
  look-ahead: input sample `n` comes out as output sample `n + latency`). The caller resamples and cuts blocks; input 0
  is the audio, shape `[block]`; other inputs are per-block controls.
- **The deadline of block `k` is `take(k)`**, made in the call that pushes block `k + queue`. The model has `queue`
  block periods per block, and the output trails the input by `added_latency(queue) = queue × block + latency` samples.
  This is what feeds `voice.vc` and the PF-12 A/V delay; the model node adds its own accumulation and resampling
  (`audio-api.md` section 4.5). Deadlines count blocks, not clock time, so a stream is deterministic under test.
- **Bounded queue:** when `queue` blocks already wait, `push` drops the oldest (it would be late anyway). The worker
  skips any block whose deadline passed before it started. `push` and `take` copy into preallocated slots and never
  block, lock or allocate.
- **`Block::Missed`** means "play your fallback for this block": the time-aligned dry voice for voice conversion,
  silence or hold for voice to face. A late answer is thrown away.
- **Stall:** `stall_blocks` missed in a row and the slot gives up on the model for the stream (`StreamStats::stalled`).
  It falls back to the next model in the chain if there is one; `voice_conversion@1` has none, so it stays dry, and the
  health meter says so.
- **`StreamStats`** (pushed, ready, missed, dropped, `run_p95_us`, stalled) is `Copy`: meters read it per block.
- **`BlockSink::restart`** starts a new stream (a device change) and resets the model's state.

`omuzaki-ai-mock::InlineStream` is the reference behaviour (it runs the model inside `push`, with late blocks scripted),
so the model node and other consumers test their fallback without threads.

## 11. Hot and cold, and what an op or agent can see

| Hot (per frame or block; no allocation once buffers have grown, no blocking) | Cold (may allocate, lock, touch disk, take seconds) |
|---|---|
| `Session::run`, `BlockSink::push`, `BlockSource::take`, `stats` | `ModelRegistry` (every method), `Runtime::probe` / `load`, `FileSource::get` |
| `TensorRef` (`Copy`), `TensorBuf` (reused), `RunError` (heap only in `Failed`), `TensorProblem`, `Block`, `StreamStats` (`Copy`) | `Signature`, `ModelManifest`, `ModelCard`, `ModelEntry`, `SlotStatus`, `EvalReport`, `SessionInfo` |
| `Signature::check_inputs` / `check_outputs` (every session calls them first) | `Signature::accepts`, `ModelManifest::validate`, `Slot::evaluate` |

`omuzaki-ai-mock/tests/hot_loop.rs` proves the session and stream paths allocate nothing over 1000 runs and blocks, with a
counting allocator. The real slot session and stream host reuse the test.

**Agents** see, through `model.list`, everything in `SlotStatus` and `ModelEntry`, all of it data with JSON schemas:
- ids, versions and digests;
- slots and their modes;
- licences, sizes and statuses with reasons;
- evaluation results.

**No local path** appears in either: where a person keeps a model is theirs (ADR 0013).

Agents can:
- list models;
- request `select` (refused for an unacknowledged user model);
- `install` with the `net` scope;
- `add` a model from a root the person delegated (it stays `NeedsAcknowledgement`).

Agents can't acknowledge, can't satisfy a consent requirement, and never hold a session or a stream.

Scopes for the `model.*` ops, decided on review and aligned with `library.install` (ADR 0019):

| Op | Scope |
|---|---|
| `model.list` | `read` |
| `model.select`, `model.rollback` | `perform` |
| `model.add` | `edit:library` + `fs` (with the root) |
| `model.install` | `edit:library` + `net` |
| `model.remove` | `edit:library` |
| `model.acknowledge` | person only |

## 12. What existing code becomes

| Today | Becomes |
|---|---|
| `omuzaki-speech/src/catalog.rs` | `models/*.model.toml` + `register_model!`; `ModelSpec` and `find` go |
| the speech host's own `ort` session for Silero | `vad@1` sessions from the registry; `SileroVad` wraps a `Session` |
| `Whisper::load(path)` | runtime `whisper-ggml` registered by `omuzaki-speech`; `Whisper` wraps an `asr@1` session |
| `SpeechError::ModelMissing { catalog_id }` | `RegistryError::NoModel` / `NotInstalled` status, at wiring time |
| `omuzaki-stage` `fallback/source.rs` `CachedDownloader`, `Fetcher`, `sha256_hex` (merged) | `omuzaki-ai`'s `FileSource`; `omuzaki_ai_api::Fetcher`; `FontSpec` holds a `PinnedFile` |
| app-layer fetcher for fonts | one `Fetcher` (HTTPS, streaming, size-capped) for every pinned file, in `omuzaki-http` |
| real-model tests with `OMUZAKI_SPEECH_MODELS` | `omuzaki-test` integration tests (real runtime + real slot), still opt-in by env var |

## 13. Interfaces with the other contracts

- **`omuzaki-ops-api`:** `ModelRegistry` is a `Service`; the `model.*` ops live with the registry in `omuzaki-ai` (a
  vertical slice; no mock registry is needed). `Requester` is what an op derives from `Actor`.
- **`omuzaki-store-api`:** unchanged. Model blobs live in the registry's cache, not the project store; licence records
  carry `MadeWith`. Projects reference models by `ModelRef`, never embed weights.
- **`omuzaki-audio-api` (ADR 0018):** the `model.stream` node uses `BlockSink` / `BlockSource`, `StreamSpec`,
  `StreamStats` and `Block` as defined here; tap consumers (streaming ASR, voice to face) push from their own threads.
- **`omuzaki-anim-api`:** `voice_to_face@1` and the face tracker output ARKit-52 in the channel table's order.
- **Licence records:** `MadeWith`; `card.imitates` for SE-05.

## 14. Decisions and alternatives

The main choices are recorded in ADR 0017; in short:

| Decision | Rejected |
|---|---|
| Slots self-register from the crate that owns their data | a central slot enum in this crate |
| Sessions exchange named tensors, positional by signature | typed per-slot traits here (couples the registry to audio and vision types; the sandbox would need one protocol per slot); `ort` types in the API (no sandbox, mock or web seam) |
| One `Runtime` trait for ONNX and other formats | an ONNX-only registry (whisper.cpp would stay outside, with its own downloads and licences) |
| Multi-graph models as manifest `[[stages]]`, run by the `onnx` runtime | a composite runtime per model (code where data does) |
| An ordered list of URLs per file; ranges resume files of 500 MiB and more | one URL per file (a dead link loses the model until an update) |
| `ConsentLookup` in this crate, records in the document | the registry storing consent itself (consent belongs with licence records) |
| SHA-256 pins and digests | BLAKE3 (upstreams don't publish it) |
| Shipped manifests as TOML files, registered by the using crate | a central catalogue file (§39 rule 2); Rust literals (the licence gate would need a build) |
| User models copied into the cache | used in place (files move and change; the sandbox would need arbitrary paths) |
| A slot session that follows selection and fallback | callers re-open on error (every caller re-implements fallback) |
| Stream deadlines counted in blocks, `take` is the deadline | wall-clock deadlines (non-deterministic tests); a blocking pop with a timeout (the audio thread waits) |
| One verified download path for models, runtimes, fixtures and fonts | per-subsystem downloaders (two exist already) |
| No mock registry | a second, in-memory registry to keep in step with the real one |
| A three-number `Version` | the `semver` crate (a dependency for pre-release rules we forbid) |

## 15. Tasks

Each task names its fence, contracts and acceptance.

| Task |
|---|
| `omuzaki-ai`: the verified file cache (`FileSource`), absorbing `CachedDownloader` |
| `omuzaki-ai`: the registry (shipped catalogue, install with smoke test, rollback, selection, state file) |
| `omuzaki-ai`: slot sessions that swap and fall back without blocking |
| `omuzaki-ai`: the stream host behind `BlockSink` / `BlockSource` (ML-14, ML-05, streaming ASR) |
| `omuzaki-ai-ort`: the `onnx` runtime, pinned ONNX Runtime builds, providers, no spinning |
| `omuzaki-ai-ort`: multi-graph models (`[[stages]]`) as one pipeline session |
| `omuzaki-http`: the HTTPS `Fetcher` and the shared cache folder (rewritten) |
| `omuzaki-stage`: sheet fonts on `PinnedFile` and the shared `FileSource` |
| `omuzaki-speech`: `vad@1`, `asr@1`, shipped manifests, the `whisper-ggml` runtime |
| `omuzaki-speech`: the host on registry sessions; real-model tests to `omuzaki-test` |
| `xtask`: shipped-model licence gate |
| `omuzaki-test`: nightly full evaluations, real and synthetic apart |
| user models in the registry: add, written manifest, matching, acknowledgement (rewritten) |
| the sandboxed worker and `SandboxedSession` (rewritten) |
| the `model.*` ops (comment with the API names and scopes) |
| `omuzaki-audio`: the `voice_conversion@1` slot and its manifest fields (ML-14) |
| `voice_conversion@1` smoke test: WER of a converted public-domain clip (ML-14) |
| consent hook: `NeedsConsent` for models that imitate a real person (ML-14, SE-05) |
| `omuzaki-audio`: the `model.stream` node (filed with ADR 0018) |

## 16. Decisions taken on review (2026-10-12)

| # | Question | Decision | Where it went |
|---|---|---|---|
| 1 | Scopes for `model.*` | `edit:library` to add, install and remove; `perform` to choose | section 11 |
| 2 | Upstream URLs or a mirror | upstream pinned URLs, verified by SHA-256, as an **ordered list per file** so a dead link falls through; a Omuzaki mirror (whether to host one is a maintainer decision) is one more URL, no code change | `ModelFile::urls`, `PinnedFile::urls`, `FetchError::AllFailed`; section 8 |
| 3 | Resume for big files | yes, from 500 MiB: an HTTP range request into the partial file, the whole hash checked at the end; a mismatch deletes it and restarts once, then fails loudly | `Fetcher::open(url, from)`, `Body`, `RESUME_MIN_SIZE`; section 8 |
| 4 | Browser inference | out of scope for now; the web player (VC-05) takes it up later through the `Runtime` seam | ADR 0017 decision 10 |
| 5 | Multi-graph models (MediaPipe face) | no runtime per model: a manifest `[[stages]]` list (graph plus named wiring), run as a pipeline of sessions behind one `Session` | `ModelManifest::stages`, `Stage`, `ModelSource::stages`; section 3 |
| 6 | The SE-05 consent lookup | a `ConsentLookup` trait here (the consumer); records with the licence records in the document, implemented app-side | `consent` module, `SlotInfo::likeness`; section 5 |
