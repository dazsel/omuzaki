---
title: "0061: A `tflite` runtime (LiteRT with XNNPACK) behind `Runtime` for the slots whose models are TFLite (2026-10-15)"
summary: "A `tflite` runtime (LiteRT with XNNPACK) behind `Runtime` for the slots whose models are TFLite"
area: decisions
---

# 0061: A `tflite` runtime (LiteRT with XNNPACK) behind `Runtime` for the slots whose models are TFLite (2026-10-15)

**Status:** proposed (#1738); accepted by the Director when merged. **Context:** #1515 and #1732 (the measurements),
the Director's ruling on #2 (comment 6187918433), ADR 0017 (the runtime seam), ADR 0016 (user models), ADR 0009
(determinism), spec v3 §10 (`track.face`, `cpu.total`). The design, with the API surface, the workflow and the
measurements, is `docs/design/litert-runtime.md`.

## Context

The face landmark graph is 84% of a tracker call. On ONNX Runtime it runs in 14 to 16.6 ms at 1 thread on a 4-vCPU
box; MediaPipe's runtime, LiteRT's interpreter with the XNNPACK delegate, runs the same TFLite graph in 6.4 to 6.7 ms
there, and in 16.3 ms with XNNPACK off (#1732). The gap is XNNPACK's NHWC kernels. No ONNX Runtime build with the
XNNPACK provider exists to download, and the Director ruled for a LiteRT runtime behind `omuzaki-ai-api`'s `Runtime`.

The bar: match MediaPipe's runtime at 1 thread on the same box, then beat MediaPipe's tracker with what we add around
the model.

## Decision

1. **Slots.** A slot's shipped model runs on the new runtime, id **`tflite`**, when its upstream publishes it as TFLite
   and its shipped path is the CPU; ONNX-native models stay on `onnx`, and nothing is converted into TFLite.
   - **On `tflite`:** the three face graphs (detector, landmarks, blendshapes), so `face_tracker@1`, and later
     `hand_tracker@1` and `pose_tracker@1`.
   - **On `onnx`:** `vad@1`, `voice_conversion@1`, `voice_to_face@1` and `tts@1` (ONNX-native, and they want GPU
     providers). `asr@1` stays on `whisper-ggml`.
   - The ONNX conversions of the face graphs stay loadable as the fallback and the second opinion in parity tests until
     a later decision retires them.

2. **The C API we bind.** LiteRT's TFLite interpreter C API (`tflite/core/c/c_api.h`), 25 functions loaded with
   `libloading` from a pinned library at run time, plus a three-function C shim of ours compiled into that library:
   `omz_litert_abi`, `omz_litert_xnnpack_create(threads)` and `omz_litert_xnnpack_delete`. The shim keeps
   `TfLiteXNNPackDelegateOptions`, a struct that grows between releases, out of Rust.
   - **Delegate options:** `TfLiteXNNPackDelegateOptionsDefault()`, then `num_threads` (0 when `threads` is 1, so no
     pool), `runtime_flags = XNN_FLAG_DONT_SPIN_WORKERS`, `FORCE_FP16` cleared, and no weight cache or cache file.
   - **Never bound:** custom-op registration, op-resolver setters, Flex. That alone doesn't refuse a custom op: LiteRT
     2.3.0 keeps an unresolved custom op in the graph for a delegate to claim, so interpreter creation can succeed.
   - **A model scan before LiteRT sees the bytes.** This is one bounded pass, in Rust, in `omuzaki-ai-litert/src/scan.rs`,
     over the model's FlatBuffer. It reads the TFLite schema at the pinned tag with a minimal, hand-written reader: every
     offset is checked, counts are capped and nothing recurses. In that pass it:
     1. reads each graph input and output's name, type and `shape_signature`, with `-1` becoming `Dim::ANY`;
     2. refuses a custom op, or any builtin not on the runtime's allow-list (the builtins the build registers, minus
        `CUSTOM`, `DELEGATE` and the variable ops);
     3. refuses any tensor with `is_variable`, so a no-op `reset` is sound;
     4. refuses data in an `external_buffer`.

     It runs at every probe and load, both cold, and is expected well under a millisecond for the face graphs (task 3a
     measures it). Its tests build synthetic FlatBuffers in the test; no model files are committed.
   - **The newer CompiledModel C API** (`LiteRt*`) is the later path to LiteRT's GPU and NPU accelerators; no TFLite
     slot needs those yet.
   - **The crate:** a new implementation crate, `omuzaki-ai-litert`, `register_runtime!(LiteRtRuntime)`. It fits
     `Runtime`, `Session` and `SessionOptions` as they are, with no contract change:
     - `probe` is the scan alone, so it runs no native code on the bytes (`opset: None`);
     - `load` scans, builds the interpreter, resizes any `any` input to the slot's shape, checks the interpreter's ports
       against the scan, and matches the slot through `io` as `onnx` does. It has the CPU provider only and skips the
       others with a reason;
     - `run` copies in and out of the interpreter's tensors and allocates nothing after warm-up;
     - the tests' `DirSource` works as for ONNX Runtime, against the run's own SHA-256 (decision 5).
   - **The shared loader:** the general pinned-archive loader moves out of `omuzaki-ai-ort` into a lib crate,
     `omuzaki-ai-native`, which both runtimes use, so it isn't copied.

3. **Threads.** At 1 thread XNNPACK runs on the tracker's own thread, with no pool and nothing that can spin, at the
   least CPU per frame. More threads cut the wall time and raise the CPU time: the landmark graph is 2.4x faster at 4
   threads and costs 1.5x the CPU.
   - **The default is 1 thread for every face graph.** The landmark slot alone may rise to the smallest count that
     meets `track.face` (5 ms p95) on `R`, and only on the timing probe's evidence, which shows the CPU it costs against
     `cpu.total`. If both rows can't be met, the Director decides with the numbers.
   - **When a pool exists,** idle workers yield to the OS instead of spinning. An idle-CPU test pins it at under 1 ms
     of CPU per second.
   - **Beating MediaPipe is the tracker's job.** A full pass every frame doesn't fit `cpu.total`'s 8 ms per frame even
     at MediaPipe's speed, so ROI reuse and frame skipping are needed (follow-up below).

4. **Determinism and parity.**
   - **Bit-identical** run to run, session to session and across 1, 2 and 4 threads, for one build on one machine.
     This was measured for all three graphs, and a test asserts it on output digests.
   - **Across machines,** results are within tolerance (ADR 0009 tier 2), because XNNPACK picks kernels by instruction
     set. `SLOW_CONSISTENT_ARITHMETIC` stays off unless a CI runner shows a run-to-run difference.
   - **Parity with MediaPipe on the same frames:** the reference is the official `ai-edge-litert` wheel at the version
     we build, XNNPACK on, 1 thread, fed the same tensors. The inputs are the converter check's seeded tensors and the
     crops our pipeline cuts from the kit's synthetic faces.
     - **Gate:** `|ours - reference| ≤ 1e-4` absolute on every output element of each graph. The expected value is 0.
       XNNPACK against LiteRT's own kernels already differs by 5.6e-4, so a misconfigured runtime fails the gate.
     - **Against the ONNX path:** `tflite` also stays inside the converter's ONNX gate (5.5e-5, 3.8e-4, 1.8e-6).

5. **Build and hosting.** The build is decided; publishing to users is not.
   - **The workflow:** `.github/workflows/runtime-litert.yml`, a **reusable workflow** (`workflow_call`, plus
     `workflow_dispatch` for maintainers), with read-only permissions (`contents: read`, nothing more). Sessions never
     dispatch it.
     - It builds one shared library per platform (`linux-x64`, `win-x64`, `macos-arm64`) from LiteRT at tag `v2.3.0`,
       its commit pinned, with LiteRT's own Bazel (7.7.0 from its `.bazelversion`, Bazelisk pinned by SHA-256). LiteRT's
       Bazel workspace pins every dependency archive by SHA-256.
     - Toolchains are pinned: the Linux container by digest, the MSVC toolset version, the Xcode version with macOS 13
       as the deployment target. A job fails when a version drifts from `tools/litert/pins.toml`.
     - A check job loads each library, runs a face graph fully delegated to XNNPACK, and compares it with the wheel
       (decision 4).
   - **Hosting now: a checksummed artifact of the same run.** Each platform's archive is uploaded with
     `actions/upload-artifact`, and its SHA-256 is a job output of the build job. No new permission is needed.
     - **Consumers call the workflow in their own run.** A cross-run download would need `actions: read`, a new
       permission only the owner can grant (CLAUDE.md), so nothing depends on one. The nightly parity job and the `R`
       probe's job (#1731) each call the workflow as a job, then take the archive with `actions/download-artifact` from
       that same run.
     - **A cache skips the rebuild:** `actions/cache`, keyed on the hash of `tools/litert/**` (version, commit, toolchain
       pins), the platform and the runner image's version. Only `cache-warm.yml` saves it, as the repository's rule says:
       it calls the workflow with `save-cache: true` on pushes to `main` that change `tools/litert/` or the workflow.
       Every other caller only restores, and on a miss builds without saving.
     - **What a consumer trusts:** a rebuild after a cache miss isn't promised to be byte-identical, so CI checks the
       archive against the build job's SHA-256 output, not a hash committed in the repository. It then checks
       `BUILDINFO.json` against `pins.toml` and, at load, the library's version and ABI number. The `R` probe's
       artifact carries that SHA-256 with the archive.
   - **Publishing to users is pending the owner's decision.** A GitHub Release (it needs `contents: write`) is how the
     app would fetch the archive through `FileSource`, but whether to publish one is asked of the owner by the Director
     (#1748).
     - Until the owner answers, nothing is published, the build table has no fixed pin or URL, and the app's default
       for the face graphs stays `onnx`.
     - If a release is approved, it is one added job that publishes the already-checked artifacts unchanged under an
       immutable tag, and a fixed `PinnedFile` (URL, SHA-256, size) goes into the table.

     That step is not decided by this record.
   - **The archive** holds `lib/`, LiteRT's `LICENSE`, `THIRD_PARTY_NOTICES.md` and `licences/` copied from the source
     tree that was built, and `BUILDINFO.json`. The notices step fails on a linked dependency that has no licence file
     or no entry in `tools/litert/licences.toml`.
   - **The build table:** `crates/omuzaki-ai-litert/src/builds.rs`, shaped like ONNX Runtime's. For now it holds the
     version, the ABI number and each platform's asset and library name; the SHA-256 comes from the run
     (`OMUZAKI_LITERT_SHA256`). On load, the crate checks `TfLiteVersion()` and the shim's ABI number.
   - **Why Bazel, not CMake:** LiteRT's CMake build at `v2.3.0` needs a TensorFlow checkout beside it and fetches by git
     tag. CMake is the fallback.
   - **The sign-off:** the workflow is a hub and needs the Director's sign-off.

6. **Licence rows** (`docs/licensing/deps/`, added with the build task, each `conditions: ship the notice in the
   archive`). Each is checked against the source tree that was actually built; "seen" means its licence file was read at
   the pinned commit for this design.
   - **LiteRT:** Apache-2.0 (seen at `v2.3.0`).
   - **XNNPACK:** BSD-3-Clause (BSD text seen at commit `d89ef666`).
   - **pthreadpool:** BSD-2-Clause (licence file seen at `15a6644b`, clause count to confirm).
   - **cpuinfo:** BSD-2-Clause (licence file seen at `8ce83db8`, clause count to confirm).
   - **FP16:** MIT. At this XNNPACK commit it appears vendored rather than downloaded; to confirm.
   - **FXdiv:** MIT. pthreadpool's CMake fetches it; whether the Bazel build links it is to confirm.
   - **Not in the issue's list, but linked by LiteRT's build** (from its CMake modules; the build task confirms each
     against Bazel's dependency query):
     - FlatBuffers, Abseil, ruy, gemmlowp, ml_dtypes and KleidiAI (arm64): Apache-2.0;
     - farmhash: MIT;
     - Eigen: MPL-2.0, allowed unmodified (policy);
     - OouraFFT (fft2d): its own permissive notice;
     - NEON_2_SSE: BSD-2-Clause.
   - **`libloading`:** ISC, if the crate takes it as a new direct dependency.
   - **The face model rows** change their condition from "ONNX conversions ship" to "the bundle's TFLite files ship
     unmodified".

7. **Prior art.**
   - **MediaPipe's task runner** runs these graphs on this same interpreter and delegate in its CPU inference
     calculator, and runs the detector only when tracking is lost. We take its runtime and its model files byte for
     byte; our pipeline already ports its tracking. We don't take its calculator-graph framework.
   - **The `ai-edge-litert` wheel** ships official binaries for all three platforms. Its library exports only the
     CompiledModel API, and its dist-info lacks the third-party notices, so we use it as the parity reference and don't
     ship it.
   - **Rust crates that build TensorFlow Lite in `build.rs`** put network access and a C++ toolchain in every cargo
     build. We follow `ort`'s `load-dynamic` pattern instead: nothing native at build time, a pinned archive at run
     time.
   - **Mobile apps** ship Google's prebuilt libraries; desktop embedders build `tensorflowlite_c` themselves, as we will.

## Consequences

- Two new crates (`omuzaki-ai-native`, a Lib; `omuzaki-ai-litert`, an Implementation), one reusable workflow and a job
  in `cache-warm.yml`, `tools/litert/`,
  and about fifteen licence rows. No contract crate changes.
- Until the owner decides on publishing, the `tflite` runtime runs in CI and in the `R` probe only; the app can't fetch
  it for people, so the shipped default for the face graphs moves to `tflite` only once a download URL exists (task 5
  keeps `onnx` as the app's default until then).
- The V0 face path no longer depends on the ONNX conversion. The `.tflite` files come straight from the verified
  bundle, which `tools/models/mediapipe-face.sh` already writes and lists in `manifest.json`.
- We own a native build per platform and its upkeep on LiteRT bumps; the notices gate makes a bump's new dependencies
  visible.
- Open questions are listed, with the task that answers each, in the design's section 12.

## Task cut

Sizes are changed lines. Each task's acceptance includes `timeout 3600 cargo xtask check --affected`.

1. **`omuzaki-ai-native`: the shared pinned-archive loader** (model: sonnet, about 300 lines).
   - **Fence:** `crates/omuzaki-ai-native/**` (new), `crates/omuzaki-ai-ort/src/library.rs`,
     `crates/omuzaki-ai-ort/Cargo.toml`.
   - **Contract:** none changes.
   - **Acceptance:**
     - the unpack, size-cap, atomic-rename and once-per-process code moves with its unit tests;
     - `omuzaki-ai-ort`'s tests pass unchanged, including the gated ones with `OMUZAKI_ORT_CACHE`.
2. **The LiteRT build workflow** (hub, Director; model: opus, about 400 lines).
   - **Fence:** `.github/workflows/runtime-litert.yml` (reusable, `workflow_call`), one job in
     `.github/workflows/cache-warm.yml` (the cache save), `tools/litert/**` (the shim, the Bazel overlay, `pins.toml`,
     `licences.toml`, the notices script, the C smoke program), and `docs/licensing/deps/` for decision 6's rows.
   - **Acceptance:**
     - the PR links a run on its branch: three archives as checksummed same-run artifacts, each archive's size and
       SHA-256 as a job output, its notices list, the delegated-node count for the three face graphs, and the smoke
       parity against the wheel;
     - a second run shows a cache hit skipping the Bazel build, with the `check` job still run;
     - no permission beyond `contents: read`, and no cross-run artifact download;
     - answers design section 12's task-2 rows;
     - no release job and no `contents: write`: publishing waits for the owner's decision (decision 5).
3. **`omuzaki-ai-litert`: the `tflite` runtime,** in two PRs (model: opus).
   - **3a, the library and the probe** (about 400 lines):
     - **Fence:** `crates/omuzaki-ai-litert/**` (new): `builds.rs` (version, ABI, asset and library names; no SHA-256
       or URL until #1748), the model scan (`src/scan.rs`), the FFI table, library loading through
       `omuzaki-ai-native`, `probe`, refusals;
     - **Acceptance:**
       - the scan's tests use **synthetic FlatBuffers built in the test**, with no downloaded or generated model files:
         a dynamic input probes as `Dim::ANY`, and a custom op, an off-list builtin, a variable tensor, an external
         buffer and truncated or out-of-range files are each refused with the right `LoadError`;
       - the allow-list is checked against the build's registered builtins;
       - the scan's cost on the landmark graph is in the PR body;
       - the version and ABI checks refuse a mismatched library;
       - the tests' source checks the archive against `OMUZAKI_LITERT_SHA256`.
   - **3b, the session** (about 350 lines):
     - **Fence:** the same crate;
     - **Acceptance:** `load` and `run` against slots on a synthetic `ADD` graph built in the test; resizing an `any`
       input to the slot's shape; the shape-change path; no allocation after warm-up (a counting-allocator test); idle CPU
       under 1 ms per second.
   - Gated tests use `OMUZAKI_LITERT_CACHE` and skip without it.
4. **Parity and determinism tests** (model: sonnet, about 300 lines).
   - **Fence:** `crates/omuzaki-ai-litert/tests/**`, `tools/models/litert_reference.py` (the pinned wheel in a throwaway
     venv, `python -I`), and in `.github/workflows/nightly.yml` (Director) one job that calls the LiteRT workflow and one
     that takes its archive from the same run.
   - **Acceptance:** decision 4's checks on Linux and Windows:
     - digests equal over two sessions at 1, 2 and 4 threads;
     - `≤ 1e-4` against the wheel on the seeded and synthetic-face inputs;
     - inside the ONNX gate;
     - the measured cost of `SLOW_CONSISTENT_ARITHMETIC` in the PR body.
5. **Slot migration: the face graphs on `tflite`** (model: sonnet, about 250 lines).
   - **Fence:** `crates/omuzaki-track/**` (`ModelDir` picks `.tflite` or `.onnx` by the runtime's id),
     `crates/omuzaki-test/tests/track_models.rs`, `crates/omuzaki-test/benches/track.rs`, `crates/omuzaki-test/Cargo.toml`,
     and the face model rows in `docs/licensing/deps/`.
   - **Acceptance:**
     - the gated `track_models` tests pass on both runtimes;
     - `tflite` is the face graphs' runtime in CI and the probe; the app's default stays `onnx` until a published pin
       exists (decision 5);
     - the PR body shows `track_roi` before and after on the same box (mean, p50 and p95 of 1,000 calls at 1 thread);
     - the landmark run is within 10% of the wheel's time on that box in the same sitting.
6. **The timing probe on `R`** (extends #1731 and #1734; model: sonnet, about 200 lines).
   - **Fence:** the probe in `crates/omuzaki-track/`, and #1731's nightly job, which calls the LiteRT workflow in the
     same run and bundles its archive with the build job's SHA-256; the probe checks it before loading.
   - **Acceptance:** the probe prints wall mean, p50 and p95, and CPU per frame, per stage, for `onnx` and `tflite` at
     1, 2 and 4 threads. It prints no CPU or machine name. The owner's run on `R` (#1734) then settles decision 3's
     thread count.

**Order:** 1 and 2 in parallel; 3a after both; 3b; then 4 and 5 in parallel; 6 after 5.

**Follow-up to file, not designed here:** the tracker's frame skipping and ROI reuse, the "beat MediaPipe" part of the
bar (`crates/omuzaki-track`), measured with task 6's probe against `cpu.total`.

## Rejected

- **ONNX Runtime with a self-built XNNPACK provider:** the Director's ruling. Its coverage of our graph was unproven, and
  partitions would bring the layout switches back.
- **Shipping the wheel's library:** see decision 7.
- **Mirroring `TfLiteXNNPackDelegateOptions` in Rust:** an ABI break the compiler can't see on every bump.
- **Building LiteRT in `build.rs`, or linking it statically:** a native toolchain and network access in every build, and
  a runtime that can't be verified or replaced on its own.
- **More threads by default:** they cost CPU that `cpu.total` counts, for a wall-time gain only `R` can show is needed.
- **The fixed-layout graph rewrite (#1733):** stays parked; at best it reaches the number this design reaches directly.
