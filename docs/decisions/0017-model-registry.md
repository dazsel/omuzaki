---
title: "0017: One model registry: self-registered slots, tensor sessions behind a runtime seam, SHA-256 pins, block-counted stream deadlines (2026-10-12)"
summary: "One model registry: self-registered slots, tensor sessions behind a runtime seam, SHA-256 pins, block-counted stream deadlines"
area: decisions
---

# 0017: One model registry: self-registered slots, tensor sessions behind a runtime seam, SHA-256 pins, block-counted stream deadlines (2026-10-12)

**Status:** accepted (2026-10-12), with the review answers folded into decisions 3, 5, 9 and 10. **Context:** ML-01, ML-13 (ADR 0016),
ML-14, the speech host, the sheet fonts, the downloader, licence records, ADR
0006 and 0015 (self-registration), ADR 0018 (the audio engine, which already builds on this record's `BlockSink` /
`BlockSource`). The design, with the types and the slot table, is `docs/design/ai-api.md`.

## Context

The spec (ML-01) wants every model behind a versioned interface, with a manifest, a model card, an eval suite, a licence
record, a fallback when it is missing, on-demand verified downloads with rollback, and inference through ONNX Runtime with
GPU providers and a CPU path. ADR 0016 adds user-supplied models in every slot, run sandboxed and smoke-tested. ML-14
adds a streaming slot fed from the audio thread.

Two crates had already built pieces of this alone:
- the speech host pins Whisper and Silero in its own catalogue and opens `ort` itself;
- the sheet layout pins fonts and has its own `CachedDownloader`.

Neither is reachable from the other, and Whisper isn't ONNX.

## Decision

1. **A slot is a registration, not an enum.**
   - The crate that owns a slot's data registers it with `register_slot!`: a `SlotRef` (`vad@1`), a `Signature`, a mode
     and an `evaluate` hook.
   - A new interface is a new registration (`asr@2`), never an edit.
   - Shipped models register the same way (`register_model!` with a `models/<id>.model.toml` file next to the code), and
     so do runtimes (`register_runtime!`).
   - No central list anywhere (§39 rule 2).
2. **Sessions exchange named tensors.**
   - `Session::run(inputs, outputs)` takes borrowed tensors in the slot signature's order and fills reusable buffers
     (allocation-free after warm-up).
   - Models fit a slot by port name, through the manifest's `io` renames.
   - Typed APIs (`Recognizer`, `VoiceActivity`) stay in the slot owners' crates and wrap a session.
3. **Runtimes are a seam.**
   - `Runtime::probe` / `load` turn files into sessions. ONNX Runtime is one runtime (`onnx`, `omuzaki-ai-ort`); whisper.cpp
     is another (`whisper-ggml`).
   - No `ort` type appears in the contract, so mocks, the sandbox proxy and a future web runtime are all just sessions.
   - **A multi-graph model is not a runtime** (review answer 5). Its manifest lists `[[stages]]`: each a graph file plus
     named wiring (a slot input, or an earlier stage's `<stage>.<port>`), and `io` maps each slot output to a
     `<stage>.<port>`. The `onnx` runtime runs the stages as a pipeline of sessions behind one `Session`, so the slot
     sees one model (the MediaPipe face pipeline). Glue that isn't a graph becomes a small graph of its own.
4. **SHA-256 everywhere a file is pinned,** and a model's identity is `ModelRef { id, version, digest }`, where the
   digest covers every file. Upstreams publish SHA-256, and both existing catalogues use it.
5. **One verified download path.**
   - `PinnedFile`, the app's `Fetcher` and the `FileSource` cache serve model files, runtime libraries, eval fixtures
     and fonts alike.
   - `CachedDownloader` becomes `omuzaki-ai`'s `FileSource`, and its rules stay: size-capped, hash-checked,
     atomic, by hash.
   - **Each file has an ordered list of URLs** (review answer 2): the upstream's pinned URL first, and any failure
     falls through to the next. A Omuzaki mirror is one more URL in a manifest, with no code change; hosting one is a
     maintainer decision for later.
   - **Files of 500 MiB and more resume** (review answer 3): `Fetcher::open(url, from)` makes an HTTP range request
     into the partial file, and the whole file is hashed at the end. A mismatch deletes it and downloads once more from
     the start; a second mismatch fails loudly.
   - A version becomes the default only after it is verified and smoke-tested. The version it replaced stays installed
     as the rollback target.
6. **A slot session follows the slot.**
   - `ModelRegistry::open` returns a session that swaps in a new selection between runs and falls back
     (selection, then shipped default, then `without_model`) on a crash, a stall or missing files.
   - While a replacement loads it answers `Unavailable` and never blocks.
7. **Streams count deadlines in blocks.**
   - A streaming slot is fed through `BlockSink::push` and read through `BlockSource::take`, both non-blocking. A
     bounded queue of `queue` blocks sits between them.
   - Block `k`'s deadline is `take(k)`, made while pushing `k + queue`. A late block is `Missed` (the caller plays its
     fallback), and `stall_blocks` misses in a row stall the model for the stream.
   - The manifest declares the model's rate, block and own latency, and `added_latency(queue)` is what feeds PF-12.
8. **User models are imported and sandboxed.**
   - Added files are copied into the cache.
   - Probing, evaluating and running happen in a resource-limited worker process that refuses custom ops and native
     code.
   - Shipped models run in-process.
9. **What only a person may do is in the contract.**
   - The disclaimer text is a constant.
   - `select` takes a `Requester` and refuses an agent's switch to an unacknowledged model.
   - A model that imitates a real person needs a consent record (`NeedsConsent`). Each slot declares which `Likeness`
     its models reproduce, and the registry asks a `ConsentLookup` (review answer 6). The trait is here because the
     registry consumes it; the records live with the licence records in the document, and the app implements
     the lookup over them. Until it does, `NoConsentRecords` keeps every imitating model unusable.
   - No local path appears in what agents see.
10. **Scopes for the `model.*` ops** (review answer 1): `edit:library` to add, install and remove a model, as for
    library items (ADR 0019), and `perform` to choose one. Running models in the browser is out of scope (review answer
    4); the web player (VC-05) takes it up later through the `Runtime` seam.

## Consequences

- New crates: `omuzaki-ai-api` (contract) and `omuzaki-ai-mock` (fake runtime and sessions, an in-memory fetcher, two test
  slots, a shipped test model, the reference stream). There is no mock registry: the `model.*` ops live with the
  registry in `omuzaki-ai` and test against it.
- Implementation tasks (fenced):
  - `omuzaki-ai`: the verified cache, the registry, the slot session, the stream host;
  - `omuzaki-ai-ort`: ONNX Runtime with its providers, and `[[stages]]` pipelines;
  - `omuzaki-http`: the fetcher;
  - the speech host's and the fonts' migrations;
  - the shipped-manifest licence gate;
  - the nightly eval suite;
  - the ML-14 slot, smoke test and consent hook.
  - The ML-13 tasks are rewritten against these types.
- `omuzaki-speech` (ADR 0018's name for the speech host crate) loses its catalogue and its direct `ort` use. `omuzaki-stage` loses
  `CachedDownloader`.
- Every shipped model gets a `model.toml` with a register row and lineage. That is why ML-14 ships no default: no
  candidate's chain is clean.

## Rejected

- **A central slot enum or catalogue file:** every model PR would edit it (§39 rule 2).
- **Typed per-slot traits in this crate:** they couple the registry to audio and vision types, and the sandbox would
  need one IPC protocol per slot.
- **`ort` types in the contract:** no seam for mocks, the sandbox proxy or the web.
- **An ONNX-only registry:** Whisper (ggml) would keep its own downloads and licence handling outside it.
- **BLAKE3 for pins:** upstreams don't publish it, so a pin couldn't be checked against its source.
- **Using user models in place:** files move and change under us, and the sandbox would need access to arbitrary paths.
- **Callers handling fallback themselves:** every subsystem would re-implement it, differently.
- **Wall-clock stream deadlines or a blocking pop with a timeout:** non-deterministic tests, or an audio thread that
  waits.
- **Bundling every ONNX Runtime flavour in the installer:** CUDA and `DirectML` builds alone break the 150 MB budget.
- **A composite runtime per multi-graph model** (such as `mediapipe-face`): code per model, where a manifest's stage
  list and wiring do the same as data.
- **One URL per file:** a dead upstream link would take a shipped model away until an app update.
- **A mock registry:** a second registry to keep in step with the real one.
