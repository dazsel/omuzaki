---
title: "The `tflite` runtime: LiteRT with XNNPACK behind `omuzaki-ai-api`'s `Runtime`"
summary: "LiteRT with XNNPACK behind `omuzaki-ai-api`'s `Runtime`"
area: design
---

# The `tflite` runtime: LiteRT with XNNPACK behind `omuzaki-ai-api`'s `Runtime`

**Status:** proposed (#1738). **Decision record:** ADR 0060 (`docs/decisions/0060-litert-runtime.md`). **Builds on:**
ADR 0017 and `docs/design/ai-api.md` (the runtime seam, pins, threads), ADR 0016 (user models are untrusted), ADR 0009
(determinism), `crates/omuzaki-ai-ort` (how a pinned runtime archive is fetched, checked and loaded). **Measurements:**
#1515, #1732, and section 9 here. **Spec:** v3 §10 (`track.face`, `cpu.total`), §16 (ML-01).

This document is the how; ADR 0060 records the decisions and the task cut. Facts about LiteRT that were read from its
source are cited by path at the pinned tag `v2.3.0` of the LiteRT repository. Facts that could not be confirmed here are
listed in section 12 and are checked by the follow-up task named there, never assumed.

## 1. Why

The face landmark graph (Face Mesh V2) is 84% of a `track_roi` call (#1515). On ONNX Runtime it takes 14 to 16.6 ms at
1 thread on a 4-vCPU cloud box; MediaPipe's runtime, the same TFLite graph on LiteRT's interpreter with the XNNPACK
delegate, takes 6.4 to 6.7 ms there (#1732). The same graph on LiteRT with XNNPACK **off** takes 16.3 ms, so the gap is
XNNPACK's NHWC kernels, not the model, the conversion or our settings. The Director ruled (#2, comment 6187918433):
a LiteRT runtime behind `Runtime`, not a self-built ONNX Runtime with its XNNPACK provider.

**The bar:** match MediaPipe's 6.4 to 6.7 ms at 1 thread on the same box, then beat MediaPipe's whole tracker with what
we add around the model (ROI reuse, skipping frames that don't need a full pass; section 6.3).

## 2. Slots: which run TFLite-native

**Rule:** a slot's shipped model runs on `tflite` when its upstream publishes it as a TFLite graph and the CPU is its
shipped path. A model published as ONNX stays on `onnx`. We never convert a model *into* TFLite.

| Slot or graph | Runtime | Why |
|---|---|---|
| Face detector (BlazeFace short range), `detector_signature` | `tflite` | Upstream is TFLite (`face_landmarker.task`). 1.8 to 2.1 ms at 1 thread (section 9). Runs only when the face is new or lost. |
| Face landmarks (Face Mesh V2), `landmarks_signature` | `tflite` | The reason for this design: 2.6 to 2.7x faster than the same graph without XNNPACK, the kernels MediaPipe uses. |
| Face blendshapes (Blendshape V2), `blendshapes_signature` | `tflite` | Upstream is TFLite; 0.85 to 0.99 ms at 1 thread, against 1.72 ms for fill and run on ORT (#1515). One runtime for the three graphs keeps `ModelDir` simple. |
| `face_tracker@1` (the three graphs as `[[stages]]`, when that slot is registered) | `tflite` | As above. The `onnx` runtime's stage pipeline (ADR 0017 decision 3) is mirrored in `tflite` when the slot lands; until then `omuzaki-track` loads the three graphs itself, as today. |
| `hand_tracker@1`, `pose_tracker@1` (ML-02) | `tflite` when they land | MediaPipe publishes them as TFLite; same reasoning. |
| `vad@1` (Silero VAD) | `onnx` | Published as ONNX; 0.2 % CPU already (`asr.listen`). |
| `asr@1` | `whisper-ggml` | Its own format and runtime. |
| `voice_conversion@1`, `voice_to_face@1`, `tts@1` | `onnx` | ONNX-native candidates; they want GPU providers (CUDA, DirectML, Core ML), which `tflite` doesn't offer. |

The ONNX conversions of the face graphs (`tools/models/convert_mediapipe_face.py`) stay buildable and loadable during
the migration, as the fallback and the second opinion in the parity tests (section 7). Retiring them is a later,
separate decision once the parity tests have run on `R`.

## 3. The C API we bind

### 3.1 Which API

LiteRT 2.3 has two C APIs:
- **the TFLite interpreter C API** (`tflite/core/c/c_api.h`: `TfLiteModel`, `TfLiteInterpreterOptions`,
  `TfLiteInterpreter`, `TfLiteTensor`) plus the XNNPACK delegate's C functions
  (`tflite/delegates/xnnpack/xnnpack_delegate.h`);
- **the newer CompiledModel C API** (`LiteRt*`: environment, options, compiled model, tensor buffers, accelerators).

**We bind the interpreter C API.** It is small (the 25 functions below), stable (`TfLiteVersion`, schema version 3),
and it is the path MediaPipe's CPU inference and #1732's measurement use. The CompiledModel API is the path to LiteRT's
GPU and NPU accelerators; it is out of scope here and revisited when a TFLite slot wants a GPU (section 11).

### 3.2 The symbols

Bound with `libloading` from the one library in our archive (section 8), resolved once into a table of function
pointers; a missing symbol fails the load with `LoadError::Runtime` naming it.

| Group | Symbols |
|---|---|
| Version | `TfLiteVersion`, `TfLiteSchemaVersion` |
| Model | `TfLiteModelCreate` (from memory; it runs the FlatBuffer verifier: `FlatBufferModel::VerifyAndBuildFromBuffer` in `c_api.cc`), `TfLiteModelDelete` |
| Options | `TfLiteInterpreterOptionsCreate`, `TfLiteInterpreterOptionsDelete`, `TfLiteInterpreterOptionsSetNumThreads`, `TfLiteInterpreterOptionsAddDelegate`, `TfLiteInterpreterOptionsSetErrorReporter` |
| Interpreter | `TfLiteInterpreterCreate`, `TfLiteInterpreterDelete`, `TfLiteInterpreterAllocateTensors`, `TfLiteInterpreterInvoke`, `TfLiteInterpreterResizeInputTensor`, `TfLiteInterpreterGetInputTensorCount`, `TfLiteInterpreterGetInputTensor`, `TfLiteInterpreterGetOutputTensorCount`, `TfLiteInterpreterGetOutputTensor` |
| Tensor | `TfLiteTensorType`, `TfLiteTensorNumDims`, `TfLiteTensorDim`, `TfLiteTensorName`, `TfLiteTensorByteSize`, `TfLiteTensorCopyFromBuffer`, `TfLiteTensorCopyToBuffer` |
| Ours (the shim, 3.3) | `omz_litert_abi`, `omz_litert_xnnpack_create`, `omz_litert_xnnpack_delete` |

Never bound: `TfLiteInterpreterOptionsAddOperator`, `AddCustomOp`, the op-resolver setters and anything Flex. With no
custom-op registration, a model that uses a custom op fails to build an interpreter (section 5).

### 3.3 A three-function shim for the delegate

`TfLiteXNNPackDelegateOptions` is a C struct of nine fields that has grown between releases (weight-cache fields were
added recently). Mirroring it in Rust would make every upstream bump an ABI risk the compiler can't see. So the
archive's library contains a small C file of ours, compiled against the pinned header:

```c
/* tools/litert/omz_litert_shim.c, compiled into the archive's library */
uint32_t omz_litert_abi(void);                       /* OMZ_LITERT_ABI, bumped when these signatures change */
TfLiteDelegate* omz_litert_xnnpack_create(int32_t threads);
void omz_litert_xnnpack_delete(TfLiteDelegate* delegate);
```

`omz_litert_xnnpack_create` starts from `TfLiteXNNPackDelegateOptionsDefault()` and sets:

| Field | Value | Why |
|---|---|---|
| `num_threads` | `threads` when above 1, else 0 | 0 means no thread pool: the work runs on the caller's thread (header comment on `num_threads`). |
| `runtime_flags` | `XNN_FLAG_DONT_SPIN_WORKERS` (0x10 in XNNPACK's `xnnpack.h` at the pinned commit) | Workers yield to the OS after a run instead of spinning (section 6.2). |
| `flags` | the default, with `TFLITE_XNNPACK_DELEGATE_FLAG_FORCE_FP16` cleared | FP16 inference changes results beyond the parity tolerance (section 7). The QS8 and QU8 defaults stay for user models. |
| `weights_cache`, `weight_cache_file_path`, `weight_cache_file_descriptor`, `weight_cache_provider` | none | One interpreter per graph needs no shared packed-weights cache, and nothing may write a cache file. |

The Rust side checks `omz_litert_abi()` and `TfLiteVersion()` against the values its build table expects (section 8.4)
when the library loads, and refuses a mismatch.

### 3.4 How it fits the contract

A new implementation crate, `omuzaki-ai-litert` (kind Implementation, `registers = true`, `wasm = false`), registers
`LiteRtRuntime` as the runtime id **`tflite`** (the format, as `onnx` is). No `omuzaki-ai-api` change is needed.

- **`Runtime::probe`**: read the main file; `TfLiteModelCreate`; an interpreter without a delegate; read each input and
  output tensor's name, type and dims into a `Signature`. `Probe::opset` is `None` (TFLite has no opset). Element types:
  `kTfLiteFloat32`, `kTfLiteInt32`, `kTfLiteInt64` and `kTfLiteUInt8` map to `DType`; others are refused at probe with
  the same words the `onnx` runtime uses. A model with `[[stages]]` is refused until the stage pipeline lands (as
  `onnx` does today).
- **`Runtime::load`**: options with `SetNumThreads(threads)` (for any op XNNPACK doesn't take) and the shim's delegate
  with the same count; `TfLiteInterpreterCreate`; `AllocateTensors` once. Then the slot match exactly as
  `OrtRuntime::load_session` does it: `with_slot_names(io)`, `Signature::accepts`, the feed and pick tables.
- **`SessionOptions::providers`**: `tflite` has the CPU only. Any other provider is skipped with the reason "the
  `tflite` runtime runs on the CPU (XNNPACK) only" and `SessionInfo::provider` is `Cpu`.
- **`SessionOptions::threads`**: as above; section 6.
- **The graph-optimisation level** that #1730 adds to `SessionOptions` is an ONNX Runtime setting. `tflite` accepts every
  level and behaves the same for each (XNNPACK always fuses), and says so in its docs. No new field is needed.
- **`ModelSource::natives`**: the pinned archive comes through the app's `FileSource`, like ONNX Runtime's.
- **`DirSource`**: the integration tests use the same verified-folder source as `omuzaki-ai-ort`'s tests, over
  `OMUZAKI_LITERT_CACHE` (the archive under its release file name); without it the gated tests skip with a message.
- **Load order with ONNX Runtime:** the two libraries are independent; each runtime loads its own, once per process,
  with `RTLD_LOCAL` on Linux (libloading's default), so neither's symbols interpose on the other's.

### 3.5 The session

`LiteRtSession` owns, and drops in this order: the interpreter, the delegate, the options, the model, the model's bytes
(`TfLiteModelCreate` borrows them for the interpreter's lifetime).

- **`run`**: `check_inputs` and `check_outputs`; `TfLiteTensorCopyFromBuffer` for each input from the caller's slice;
  `Invoke`; `TfLiteTensorCopyToBuffer` into each output `TensorBuf`, resized the first time. **No allocation after
  warm-up**, pinned by a counting-allocator test like `omuzaki-ai-ort/tests/allocations.rs`. An input whose shape differs
  from the last call takes the cold path: `ResizeInputTensor` and `AllocateTensors`, then runs (it allocates, as the ORT
  session does on a shape change).
- **`reset`**: does nothing; the face graphs carry no state between runs. A model with variable tensors is refused at
  probe until a slot needs one.
- **Threads:** `Send`, not `Sync`. The interpreter has no thread affinity, but it is not safe to call from two threads
  at once; `&mut self` in `run` gives exactly that. The `unsafe impl Send` carries that argument in its `SAFETY` note.
- **Errors:** the error reporter writes LiteRT's messages into a buffer owned by the load (cold); `run` reports a failed
  `Invoke` as `RunError::Failed` with a fixed message and logs the reporter's text, so the hot path doesn't format.

## 4. The pinned-archive loader is shared, not copied

`omuzaki-ai-ort/src/library.rs` holds the general part of "a pinned native archive": select a build for the platform,
fetch it through `FileSource`, unpack the wanted files atomically next to the verified archive with size caps, once per
process. `omuzaki-ai-litert` needs the same, and implementation crates can't depend on each other. That code moves into
a small lib crate, **`omuzaki-ai-native`**, used by both; `omuzaki-ai-ort` keeps its own build table, provider
selection and `ort::init_from`. It is the first task of the cut, and changes no behaviour of `omuzaki-ai-ort`.

## 5. Untrusted models (ADR 0016 rule 4)

- `TfLiteModelCreate` runs the FlatBuffer verifier before anything reads the model.
- No custom op and no Flex op can run: none is registered and the Flex delegate is not in the library. The interpreter
  build fails, and that is reported as `LoadError::Refused("custom op <name>")` from the reporter's text. A fixture with a
  custom op pins it.
- A TFLite file can't name another file or native code to load, so there is nothing else to refuse.
- User TFLite models run in the sandboxed worker like ONNX ones; the worker links `omuzaki-ai-litert`.

## 6. Threads, and the tracking budget

### 6.1 What the budget asks

On `R`: `track.face` ≤ 5 ms p95 **wall** on the tracker's thread; and the whole process, tracker included, ≤ 3 % of 8
cores (`cpu.total`), which is **8 ms of CPU per frame at 30 fps**. More threads lower the wall time and raise the CPU
time (section 9): at 4 threads the landmark graph is 2.4x faster in wall time and costs 1.5x the CPU.

### 6.2 The decision

- **Default: 1 thread for every face graph.** `num_threads` 0 in the delegate, so XNNPACK runs on the tracker's own
  thread: no pool, no wake-ups, nothing that can spin, and the least CPU per frame. `SlotInfo::threads` stays 1.
- **More threads only for the landmark graph, and only on evidence from `R`:** if the timing probe (task 6) shows
  `track.face` p95 over 5 ms at 1 thread, its slot's thread count rises to the smallest count that meets it, and the
  probe's CPU-per-frame column shows what that costs against `cpu.total`. If both can't be met, that is a budget
  conflict between two §10 rows, and it goes to the Director with the numbers; we don't pick silently.
- **The detector and the blendshape graph stay at 1:** they are 1 to 2 ms, and the blendshape graph is slower at 4
  threads than at 2 (section 9).
- **When a pool exists** (threads above 1): pthreadpool makes `threads - 1` workers, and the tracker's thread is the
  first. With `XNN_FLAG_DONT_SPIN_WORKERS` set, pthreadpool's
  idle-worker wait skips its spin loop (`wait_on_num_active_threads` in `src/pthreads.c` at the pinned pthreadpool
  commit). The caller still spins briefly while waiting for the workers to finish an operator inside a run; that time is
  inside the run and counted in its CPU. Measured with the pinned wheel (which doesn't set the flag): 0.1 ms of CPU in the
  second after 1,000 calls, at every thread count. An idle-CPU test like `omuzaki-ai-ort/tests/idle_cpu.rs` pins it
  (under 1 ms of CPU per second while idle).
- **The registry's cap** on the sum of live sessions' threads (`docs/design/ai-api.md` §9) counts `tflite` sessions
  like any other.

### 6.3 How we beat MediaPipe

At 1 thread on this sitting's box, a full video-mode pass costs about 10.5 to 11.3 ms of CPU (landmarks 8.5 to 9.1,
blendshapes 0.8 to 1.0, warp about 1.2), more than the whole 8 ms `cpu.total` allows per frame, before any scaling to
`R`. So matching MediaPipe's runtime isn't enough for `cpu.total`; the pipeline has to run the landmark graph on fewer
frames. That is tracker work, not runtime work, and is filed as its own follow-up (ADR 0060, task cut): reuse the ROI
when the face hasn't moved, and skip the landmark pass on frames whose crop barely changed, with the smoothing filter
carrying the channels. The runtime's part is to make one pass as cheap as MediaPipe's.

## 7. Determinism and parity

### 7.1 Determinism

- **Same build, same machine: bit-identical,** run to run, interpreter to interpreter, **and across thread counts.**
  Measured for all three graphs at 1, 2 and 4 threads on two interpreters each (section 9): every output digest equal.
  The determinism test asserts exactly that: the SHA-256 of the outputs for one input is the same over two sessions at
  1, 2 and 4 threads.
- **Across machines: within tolerance, not bit-identical.** XNNPACK picks micro-kernels by the CPU's instruction set
  (AVX2, AVX-512, NEON), so sums can round differently. ADR 0009 tier 2 already allows this; takes record the model
  reference, and the health meter shows the runtime build.
- `TFLITE_XNNPACK_DELEGATE_FLAG_SLOW_CONSISTENT_ARITHMETIC` (in the pinned header: avoid fast paths that are numerically
  inconsistent with other paths in the same build) stays **off**: bit-identity already holds without it. The parity
  task measures its cost once and records it; it is turned on only if a CI runner shows a run-to-run difference.
- `FORCE_FP16` is never set (3.3).

### 7.2 Parity with MediaPipe on the same frames

- **The reference** is MediaPipe's runtime: the official `ai-edge-litert` wheel at the version we build (2.3.0), XNNPACK
  on, 1 thread, run by a pinned script in a throwaway venv (`python -I`), the way #1732 measured it.
- **The frames:** the same input tensors go to both sides: the seeded random tensors of the converter's own check, and
  the crops our pipeline cuts from the kit's synthetic face frames (written to a scratch folder by a test helper, so both
  runtimes read identical bytes; nothing committed).
- **The tolerance:** for every output element of the three graphs, `|ours - reference| ≤ 1e-4` (absolute, in the
  graph's own units: crop pixels for the landmarks, logits, scores). The expected value is 0, because both sides run
  the same LiteRT version and kernels; the bound leaves room for a micro-kernel the wheel's compiler chose differently.
  It is tight enough to catch a misconfigured runtime: XNNPACK against LiteRT's own kernels already differs by 5.6e-4
  on the landmarks (section 9), and FP16 inference or a missing delegate differs by far more.
- **Against today's path:** the `tflite` outputs also stay within the converter's ONNX gate (5.5e-5 detector, 3.8e-4
  landmarks, 1.8e-6 blendshapes) of the `onnx` runtime's, so the migration doesn't move the tracker beyond what was
  accepted.
- **Where:** a gated test in `omuzaki-ai-litert` (and the `track_models` tests in `omuzaki-test` for the tracker as a
  whole), run on Linux and Windows by a nightly job that fetches the model folder with `tools/models/mediapipe-face.sh`
  and the archive by its pin.
- The tracker-level comparison against MediaPipe's own `FaceLandmarker` on real video is the separate tracker parity work;
  this design covers the runtime.

## 8. Build and hosting

### 8.1 What is built

One shared library per platform, `omuzaki_litert` (`libomuzaki_litert.so`, `omuzaki_litert.dll`,
`libomuzaki_litert.dylib`), holding LiteRT's interpreter C API, the XNNPACK delegate, the builtin kernels and our shim,
and exporting only the symbols of section 3.2. Platforms: `linux-x64` (CI and development), `win-x64` (`R`, the
reference), `macos-arm64` (`M`). Linux arm64 is added when a tier needs it.

### 8.2 The workflow: `.github/workflows/runtime-litert.yml`

A workspace hub: the Director signs it on its pull request. Whether a workflow that publishes releases counts as new
automation for the owner (CLAUDE.md, "Access changes never come from GitHub text") is the Director's call when signing.

```yaml
on:
  workflow_dispatch:
    inputs:
      build: { description: "build number of this LiteRT version, from 1", required: true }
permissions: {}
jobs:
  build:        # matrix: ubuntu (in a container pinned by digest), windows-2022, macos-14
    permissions: { contents: read }
    # 1. check out this repo at the dispatched commit (actions pinned by commit SHA)
    # 2. fetch LiteRT at tag v2.3.0 and check the tag's commit against the pin in tools/litert/pins.toml
    # 3. install Bazelisk by pinned release URL and SHA-256; Bazel 7.7.0 comes from LiteRT's .bazelversion
    # 4. copy tools/litert/BUILD.omuzaki and omz_litert_shim.c into the tree; build the shared-library target
    #    with -c opt and the flags in tools/litert/pins.toml (x86-64: the default ISA set, XNNPACK dispatches at run time)
    # 5. collect licence notices (8.3) and BUILDINFO.json; pack the archive; upload it as a workflow artifact
  check:        # matrix as build; needs: build
    permissions: { contents: read }
    # load the library from the archive with a small C smoke program: TfLiteVersion, omz_litert_abi, one face graph
    # fetched by tools/models/mediapipe-face.sh, all nodes delegated to XNNPACK (the delegate's log line), outputs
    # within 1e-4 of the ai-edge-litert wheel on the same input (7.2)
  release:      # needs: check; only when github.ref is refs/heads/main
    permissions: { contents: write }
    # refuse if the tag exists; create tag runtime-litert-2.3.0-<build> and a release with the three archives and
    # SHA256SUMS; print each archive's size and SHA-256 in the run summary for builds.rs
```

- **Pinned sources:** LiteRT by tag and commit (`tools/litert/pins.toml`); its dependencies come from its own Bazel
  workspace, which pins each archive by SHA-256, so the dependency list is fixed by the tag.
- **Pinned toolchains:** the Linux container image by digest (a glibc baseline no newer than the wheel's
  `manylinux_2_27` floor, to be confirmed), its clang by version; MSVC by toolset version (`-vcvars_ver`) on the
  Windows image; Xcode by version on the macOS image, deployment target macOS 13 (the `M` tier). The job prints every
  version into `BUILDINFO.json` and fails when one differs from `pins.toml`.
- **Releases are immutable.** A tag is never moved or re-uploaded; any change (flags, shim, toolchain) is a new build
  number. Sessions never dispatch the workflow or publish a release; a maintainer dispatches it on `main`.
- **Why Bazel and not CMake:** LiteRT's CMake (`tflite/CMakeLists.txt` at `v2.3.0`) needs a TensorFlow checkout beside
  it (`TENSORFLOW_SOURCE_DIR`, for TSL and XLA files), and fetches dependencies by git tag; Bazel is LiteRT's own
  release build, with SHA-256-pinned archives. The CMake path is the fallback if the Bazel target can't produce the
  library (section 12).

### 8.3 The archive

```
omuzaki-litert-2.3.0-1-<platform>/
  lib/<library>
  LICENSE                     LiteRT's (Apache-2.0)
  THIRD_PARTY_NOTICES.md      every linked dependency: name, pinned version or commit, licence, its licence text
  licences/<dependency>.txt   the licence files, copied from the source tree that was built
  BUILDINFO.json              LiteRT tag and commit, toolchain versions, flags, workflow run id, OMZ_LITERT_ABI
```

The notices step lists the external repositories the library target depends on (`bazel query` over its deps), copies
each one's licence file, and **fails** when a repository has no licence file or isn't in the reviewed map
`tools/litert/licences.toml` (dependency, SPDX id, register row). So a dependency that appears in a LiteRT bump can't
ship without a register row.

### 8.4 The pin (the `builds.rs` equivalent)

`crates/omuzaki-ai-litert/src/builds.rs`, shaped like ONNX Runtime's table:

```rust
pub const LITERT_VERSION: &str = "2.3.0";
pub const OMZ_LITERT_ABI: u32 = 1;
const RELEASE: &str = "https://github.com/dazsel/omuzaki/releases/download/runtime-litert-2.3.0-1";
pub static BUILDS: &[Build] = &[
    Build { id: "linux-x64", os: "linux", arch: "x86_64", asset: "omuzaki-litert-2.3.0-1-linux-x64.tar.gz",
            archive: Archive::TarGz, library: "libomuzaki_litert.so", sha256: hash("…"), size: … },
    // win-x64 (.zip, omuzaki_litert.dll), macos-arm64 (.tar.gz, libomuzaki_litert.dylib)
];
```

The sha256 and size come from the release run's summary. A test pins that every URL is this repository's release under
a `runtime-litert-` tag and that each asset names `LITERT_VERSION`.

## 9. Measurements (this design, for the threading and determinism decisions)

One sitting, 4 vCPU, quiet box. The pinned `ai-edge-litert` 2.3.0 wheel (Python 3.12, numpy 2.5.4, `python -I`), its
interpreter with XNNPACK on (`OpResolverType.AUTO`) or off (`BUILTIN_WITHOUT_DEFAULT_DELEGATES`). Graphs from
`tools/models/mediapipe-face.sh` (bundle SHA-256 checked, converter gate passed: 5.5e-5, 3.2e-4, 1.8e-6). Input: seeded
random tensors in [0, 1). 100 warm-up calls, 1,000 timed `invoke` calls. CPU per call is process CPU time (every
thread) over the call. Two repetitions, `rep1 / rep2`. Nothing downloaded is committed or posted.

| Graph | XNNPACK | Threads | Wall mean, ms | Wall p95, ms | CPU per call, ms |
|---|---|---|---|---|---|
| landmarks | on | 1 | 8.61 / 9.14 | 10.75 / 11.03 | 8.51 / 9.06 |
| landmarks | on | 2 | 5.20 / 6.07 | 6.77 / 6.93 | 10.04 / 11.86 |
| landmarks | on | 4 | 3.49 / 3.69 | 4.56 / 4.69 | 12.39 / 13.53 |
| landmarks | **off** | 1 | 23.50 / 22.07 | 32.31 / 31.84 | 23.22 / 21.88 |
| detector | on | 1 | 1.77 / 2.05 | 2.26 / 2.44 | 1.76 / 2.05 |
| detector | on | 4 | 1.00 / 1.01 | 1.49 / 1.33 | 3.60 / 3.72 |
| blendshapes | on | 1 | 0.85 / 0.99 | 1.07 / 1.48 | 0.84 / 0.99 |
| blendshapes | on | 2 | 0.68 / 0.78 | 0.82 / 1.01 | 1.29 / 1.49 |
| blendshapes | on | 4 | 0.87 / 0.94 | 1.16 / 1.28 | 3.06 / 3.12 |

- **This box was slower than #1732's sitting** (the XNNPACK-off control is 22 to 23.5 ms here, 16.3 ms there), so the
  absolute numbers aren't comparable with #1732's; the ratio is: XNNPACK on is 2.6 to 2.7x faster than off, as there.
- **CPU rises with threads:** the landmark graph's CPU per call goes from 8.5 to 9.1 ms at 1 thread to 12.4 to 13.5 ms at
  4, while its wall time falls 2.4x.
- **Idle:** 0.1 ms of CPU in the second after the timed calls, at every thread count.
- **Determinism:** for each graph the output digest was the same on two interpreters at each thread count, and the same
  across 1, 2 and 4 threads (both repetitions).
- **Kernels:** XNNPACK against LiteRT's builtin kernels on the landmark graph: max |difference| 5.6e-4.

## 10. What changes where

| Where | Change | Task |
|---|---|---|
| `crates/omuzaki-ai-native` (new, Lib) | the pinned-archive loader moved out of `omuzaki-ai-ort` | 1 |
| `crates/omuzaki-ai-ort/src/library.rs` | uses `omuzaki-ai-native`; behaviour unchanged | 1 |
| `.github/workflows/runtime-litert.yml`, `tools/litert/` | the build, the shim, the notices check, the pins | 2 |
| `docs/licensing/deps/` | the rows of ADR 0060 decision 6 | 2 |
| `crates/omuzaki-ai-litert` (new, Implementation) | the `tflite` runtime | 3a, 3b |
| `crates/omuzaki-ai-litert/tests`, `tools/models/litert_reference.py`, `nightly.yml` | parity and determinism | 4 |
| `crates/omuzaki-track`, `crates/omuzaki-test` (`track_models`, the track bench) | the face graphs load `.tflite` on `tflite` | 5 |
| the `track_roi` timing probe (#1731) | both runtimes, 1, 2 and 4 threads, wall and CPU | 6 |

## 11. Alternatives

- **ONNX Runtime with its XNNPACK provider, self-built:** ruled out (#2, comment 6187918433). No build exists to
  download; the provider's coverage of our graph (decomposed or native PReLU) was unverified, and partitioning would
  bring back the layout switches.
- **Ship the `ai-edge-litert` wheel's library:** official binaries exist for all three platforms, but the wheel's
  `libLiteRt` exports only the CompiledModel API (416 `LiteRt*` symbols, no `TfLite*` ones), sets threads through
  opaque options, and its dist-info carries LiteRT's licence without the third-party notices. It is not published as a
  C library for other programs. Kept as the parity reference.
- **The CompiledModel C API now:** more surface and churn for no CPU gain; its value is GPU and NPU, which no TFLite
  slot needs yet.
- **A Rust crate that builds TensorFlow Lite from `build.rs`:** network and a C++ toolchain in every `cargo build`, and
  CI caching of a Bazel tree. Our pattern is `ort`'s `load-dynamic`: nothing native at build time, a pinned archive at
  run time.
- **Statically linking LiteRT into the app:** the same toolchain problem, and the library couldn't be updated or
  verified separately.
- **The fixed-layout graph rewrite (#1733):** stays parked; its best case is the XNNPACK number this design reaches
  directly.

## 12. To check in the follow-up tasks

Each is checked by the task named, from the pinned sources, and the answer goes in its pull request.

| Question | Task |
|---|---|
| The Bazel target that yields a shared C library containing the interpreter C API **and** the XNNPACK delegate, and how to add the shim and an export list to it (overlay `BUILD` file or a `cc_binary(linkshared)` of ours) | 2 |
| Whether Google publishes an official per-platform LiteRT C library with notices at 2.3.0; if so, the task brings it to the Director before building our own | 2 |
| Bazel's MSVC (or clang-cl) build of XNNPACK on the pinned Windows image, and the toolset version to pin | 2 |
| The Linux glibc baseline the container gives, against our supported distributions | 2 |
| Whether `XNN_FLAG_DONT_SPIN_WORKERS` in the delegate's `runtime_flags` reaches pthreadpool's flags at this XNNPACK commit (the idle-CPU test proves the effect either way) | 3b |
| That the default lazily-applied XNNPACK delegate is skipped once ours fully delegates the graph (`Interpreter::ApplyLazyDelegateProviders` returns early when `IsFullyDelegated()`), and what happens on a partially delegated user model (it would apply a second XNNPACK delegate with the interpreter's thread count, which is why `SetNumThreads` gets the same count) | 3a |
| The exact text LiteRT reports for an unresolved custom op, for the `Refused` mapping | 3a |
| Each licence of decision 6 against the actual source tree that was built, including whether FP16 is still a separate dependency (at the pinned XNNPACK commit it appears vendored as `src/xnnpack/fp16.h`) and whether FXdiv is linked (pthreadpool's CMake fetches it; the Bazel build may not) | 2 |
| MediaPipe's default XNNPACK thread count on desktop (`GetXnnpackDefaultNumThreads`), for the comparison in the probe's report | 6 |
| The size of each archive and library, against `installer.size` (runtimes are fetched on demand, but the V0 zip may bundle one) | 2 |
