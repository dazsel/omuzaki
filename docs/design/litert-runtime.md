---
title: "The `tflite` runtime: LiteRT with XNNPACK behind `omuzaki-ai-api`'s `Runtime`"
summary: "LiteRT with XNNPACK behind `omuzaki-ai-api`'s `Runtime`"
area: design
---

# The `tflite` runtime: LiteRT with XNNPACK behind `omuzaki-ai-api`'s `Runtime`

**Status:** proposed (#1738). **Decision record:** ADR 0061 (`docs/decisions/0061-litert-runtime.md`). **Builds on:**
ADR 0017 and `docs/design/ai-api.md` (the runtime seam, pins, threads), ADR 0016 (user models are untrusted), ADR 0009
(determinism), `crates/omuzaki-ai-ort` (how a pinned runtime archive is fetched, checked and loaded). **Measurements:**
#1515, #1732, and section 9 here. **Spec:** v3 §10 (`track.face`, `cpu.total`), §16 (ML-01).

This document is the how; ADR 0061 records the decisions and the task cut. Facts about LiteRT that were read from its
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

**We bind the interpreter C API.** It is small (the 24 functions below), stable (`TfLiteVersion`, schema version 3),
and it is the path MediaPipe's CPU inference and #1732's measurement use. The CompiledModel API is the path to LiteRT's
GPU and NPU accelerators; it is out of scope here and revisited when a TFLite slot wants a GPU (section 11).

### 3.2 The symbols

Bound with `libloading` from the one library in our archive (section 8), resolved once into a table of function
pointers; a missing symbol fails the load with `LoadError::Runtime` naming it.

| Group | Symbols |
|---|---|
| Version | `TfLiteVersion`, `TfLiteSchemaVersion` |
| Model | `TfLiteModelCreate` (from memory; it runs the FlatBuffer verifier: `FlatBufferModel::VerifyAndBuildFromBuffer` in `c_api.cc`), `TfLiteModelDelete` |
| Options | `TfLiteInterpreterOptionsCreate`, `TfLiteInterpreterOptionsDelete`, `TfLiteInterpreterOptionsSetNumThreads`, `TfLiteInterpreterOptionsAddDelegate` |
| Interpreter | `TfLiteInterpreterCreate`, `TfLiteInterpreterDelete`, `TfLiteInterpreterAllocateTensors`, `TfLiteInterpreterInvoke`, `TfLiteInterpreterResizeInputTensor`, `TfLiteInterpreterGetInputTensorCount`, `TfLiteInterpreterGetInputTensor`, `TfLiteInterpreterGetOutputTensorCount`, `TfLiteInterpreterGetOutputTensor` |
| Tensor | `TfLiteTensorType`, `TfLiteTensorNumDims`, `TfLiteTensorDim`, `TfLiteTensorName`, `TfLiteTensorByteSize`, `TfLiteTensorCopyFromBuffer`, `TfLiteTensorCopyToBuffer` |
| Ours (the shim, 3.3) | `omz_litert_abi`, `omz_litert_xnnpack_create`, `omz_litert_xnnpack_delete`, `omz_litert_capture_errors` |

Never bound: `TfLiteInterpreterOptionsAddOperator`, `AddCustomOp`, the op-resolver setters and anything Flex. Not
registering custom ops is **not** enough to refuse them: LiteRT 2.3.0 keeps an unresolved custom op in the graph for a
delegate to claim later (`tflite/core/interpreter_builder.cc`), so interpreter creation can succeed. The refusal is the
model scan's job, before any LiteRT code sees the bytes (section 5).

### 3.3 A four-function shim: the delegate and the error reporter

`TfLiteXNNPackDelegateOptions` is a C struct of nine fields that has grown between releases (weight-cache fields were
added recently). Mirroring it in Rust would make every upstream bump an ABI risk the compiler can't see. So the
archive's library contains a small C file of ours, compiled against the pinned header:

```c
/* tools/litert/omz_litert_shim.c, compiled into the archive's library */
uint32_t omz_litert_abi(void);                       /* OMZ_LITERT_ABI, bumped when these signatures change */
TfLiteDelegate* omz_litert_xnnpack_create(int32_t threads);
void omz_litert_xnnpack_delete(TfLiteDelegate* delegate);
/* installs a reporter that vsnprintf's each message into buf (truncated, always NUL-terminated within cap) */
void omz_litert_capture_errors(TfLiteInterpreterOptions* options, char* buf, size_t cap);
```

The error reporter is in the shim because LiteRT's reporter callback takes a C `va_list`, which stable Rust can't
receive. So `TfLiteInterpreterOptionsSetErrorReporter` is called from C, not bound in Rust.

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

- **`Runtime::probe`**: read the main file and run the model scan (section 5). The scan alone gives the `Signature`:
  each input and output tensor's name, element type and shape, the shape taken from `shape_signature` when the model
  has one (its `-1` entries become `Dim::ANY`) and from `shape` otherwise. So `probe` runs no native code on the bytes.
  `Probe::opset` is `None` (TFLite has no opset). Element types `FLOAT32`, `INT32`, `INT64` and `UINT8` map to `DType`;
  others are refused with the same words the `onnx` runtime uses. A model with `[[stages]]` is refused until the stage
  pipeline lands (as `onnx` does today).
- **`Runtime::load`**: the same scan first. Until a published pin exists (8.4) there is nothing to fetch, so `load`
  uses the library `load_archive` loaded in this process and refuses with `LoadError::Runtime` when there is none. Then options with `SetNumThreads(threads)` (for any op XNNPACK doesn't
  take) and the shim's delegate with the same count; `TfLiteInterpreterCreate`. Where the scan says a model input is
  `any` and the slot fixes it, `ResizeInputTensor` to the slot's shape; then `AllocateTensors` once. The interpreter's
  input and output tensors are checked against the scan (count, names, types), and a mismatch is `LoadError::Format`.
  Then the slot match exactly as `OrtRuntime::load_session` does it: `with_slot_names(io)`, `Signature::accepts`, the
  feed and pick tables.
- **`SessionOptions::providers`**: `tflite` has the CPU only. Any other provider is skipped with the reason "the
  `tflite` runtime runs on the CPU (XNNPACK) only" and `SessionInfo::provider` is `Cpu`.
- **`SessionOptions::threads`**: as above; section 6.
- **`ModelSource::natives`**: once a published pin exists (8.5), the archive comes through the app's `FileSource`, like
  ONNX Runtime's.
- **`DirSource`**: the integration tests use the same verified-folder source as `omuzaki-ai-ort`'s tests, over
  `OMUZAKI_LITERT_CACHE` (the archive under its asset file name); without it the gated tests skip with a message.
  There is no pin in the repository yet (8.4), so this crate's test source finds the archive by its asset name from
  the build table and checks it against `OMUZAKI_LITERT_SHA256`, not against `pin.urls[0]` and a committed hash as
  `omuzaki-ai-ort`'s does.
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
- **`reset`**: does nothing, and that is sound by construction: the scan refuses any model with a variable tensor or
  a variable op (section 5), so no state can survive a run inside the interpreter.
- **Threads:** `Send`, not `Sync`. The interpreter has no thread affinity, but it is not safe to call from two threads
  at once; `&mut self` in `run` gives exactly that. The `unsafe impl Send` carries that argument in its `SAFETY` note.
- **Tests:** the session's logic is unit-tested on an in-process fake of the C function table (`src/fake.rs`, test
  builds only: an `ADD` interpreter whose faults the model bytes choose), with a counting allocator; the same checks run
  on the real library in the gated tests (`tests/session.rs`, `allocations.rs`, `idle_cpu.rs`).
- **Errors:** the shim's reporter writes LiteRT's messages into a fixed buffer the session owns (`ERROR_BYTES`, 1 KiB,
  allocated at load). `run` reports a failed `Invoke` as `RunError::Failed` with a fixed message and logs the buffer's
  text, read up to its first NUL and within `ERROR_BYTES`, as lossy UTF-8. So the hot path doesn't format.
- **Outputs are checked before any caller buffer is written.** After `Invoke`, for every output, in one pass:
  1. its rank (`TfLiteTensorNumDims`) and dims (`TfLiteTensorDim`) are read into a stack array of `MAX_RANK`;
  2. they are checked against the slot's spec, as `omuzaki-ai-ort`'s `check_shape` does: same rank, and every fixed dim
     equal (an `any` dim takes what came back);
  3. `TfLiteTensorByteSize` must equal the element count times the type's size (checked multiplication).

  Only when every output passes are the caller's `TensorBuf`s resized and filled. A failure is
  `RunError::Mismatch { port: Output, .. }`, and no buffer has changed. This is what keeps a model with a dynamic output
  from writing a shape the slot doesn't allow.

### 3.6 What comes back from LiteRT, and its check

Every value that crosses back from native code is checked before Rust relies on it. A failed check is an error, never
a panic, and never a silent fallback.

| Value | Where | Check |
|---|---|---|
| A symbol's address | library load | non-null for every symbol of 3.2, else `LoadError::Runtime` naming it |
| `TfLiteVersion()` | library load | non-null, NUL within 64 bytes, valid UTF-8, equal to `TFLITE_VERSION`: what the pinned build reports (`tflite_version` in `tools/litert/pins.toml`, TensorFlow's version string `2.22.0-dev0+selfbuilt`, not LiteRT's `2.3.0`) |
| `omz_litert_abi()` | library load | equal to `OMZ_LITERT_ABI` |
| `TfLiteSchemaVersion()` | library load | equal to the schema version the scan reads (3) |
| `TfLiteModelCreate`, `TfLiteInterpreterOptionsCreate`, `omz_litert_xnnpack_create`, `TfLiteInterpreterCreate` | load | non-null, else `LoadError::Format` (model) or `LoadError::Runtime` (the rest), with the captured error text |
| every `TfLiteStatus` (`AllocateTensors`, `ResizeInputTensor`, `Invoke`, `CopyFromBuffer`, `CopyToBuffer`) | load and run | `kTfLiteOk` (0) or an error; an unknown value counts as an error |
| input and output tensor counts | load | non-negative, and equal to the scan's counts |
| `TfLiteInterpreterGetInputTensor` / `GetOutputTensor` | load, and again after any `AllocateTensors` | non-null for every index; the pointers are re-read after each reallocation, never kept across one |
| `TfLiteTensorName` | load | non-null, NUL within `MAX_NAME_BYTES` (1 KiB), valid UTF-8, equal to the scan's name for that port |
| `TfLiteTensorType` | load and run | a known value, mapping to the scan's `DType` for that port |
| `TfLiteTensorNumDims`, `TfLiteTensorDim` | load (inputs), run (outputs) | rank in `0..=MAX_RANK`; each dim non-negative and fits `usize`; then the slot check above |
| `TfLiteTensorByteSize` | before every copy | equal to element count × element size, computed with checked arithmetic; an input's byte length must also equal the caller's slice, so `CopyFromBuffer` can't overrun |
| the reporter's text | on any error | read only within `ERROR_BYTES`, up to the first NUL, as lossy UTF-8 |

The values are checked at load on the cold path. On the hot path only the output dims, byte sizes and statuses are
re-checked, without allocating.

## 4. The pinned-archive loader is shared, not copied

`omuzaki-ai-ort/src/library.rs` holds the general part of "a pinned native archive": select a build for the platform,
fetch it through `FileSource`, unpack the wanted files atomically next to the verified archive with size caps, once per
process. `omuzaki-ai-litert` needs the same, and implementation crates can't depend on each other. That code moves into
a small lib crate, **`omuzaki-ai-native`**, used by both; `omuzaki-ai-ort` keeps its own build table, provider
selection and `ort::init_from`. It is the first task of the cut, and changes no behaviour of `omuzaki-ai-ort`.

## 5. The model scan, and untrusted models (ADR 0016 rule 4)

One pass over the model's FlatBuffer, in Rust, before any LiteRT code sees the bytes. It is the counterpart of
`omuzaki-ai-ort`'s `scan.rs` (which reads ONNX's protobuf the same way) and lives in **`omuzaki-ai-litert`, in
`src/scan.rs`**: only this runtime reads TFLite, so it isn't shared.

**How it reads.** The official `flatbuffers` crate (Apache-2.0): its verifier checks every table, vector and string the
scan reads, then its accessors read them. What is written in `scan.rs` is the field list of the tables the allow-list
reads, by slot, from the TFLite schema at the pinned tag (`tflite/converter/schema/schema.fbs`, Apache-2.0); it is not
generated by `flatc`, so no unused code is checked in (#1749 records why). It is safe on hostile bytes, and fuzzed
nightly (`fuzz/fuzz_targets/litert_scan.rs`, #1653):
- the file is at most `MAX_MODEL_BYTES` (256 MiB, above any slot's model; the face landmark graph is 2.5 MB);
- every offset, vtable and vector is checked against the buffer before it is read, and an unaligned or out-of-range one
  is `LoadError::Format`;
- counts are capped (`MAX_SUBGRAPHS`, `MAX_TENSORS`, `MAX_OPERATORS`, `MAX_DIMS` = `omuzaki-ai-api`'s `MAX_RANK`), and
  the error names the constant;
- it never recurses and never follows a buffer's data, only table fields, so the work is linear in the tables, not the
  weights.

**The scan is an allow-list: it accepts exactly what is listed here and refuses everything else** with a named
`LoadError` (`Refused` for a feature we don't run, `Format` for malformed bytes). A schema feature nobody listed fails
closed. Each row was checked against the three shipped face graphs (section 9 survey), which it must accept.

| Table | Field | Accepted |
|---|---|---|
| `Model` | `version` | 3 |
| | `subgraphs` | exactly 1 (no control flow across subgraphs) |
| | `operator_codes` | each as `OperatorCode` below; at most `MAX_OPERATORS` |
| | `buffers` | at most `MAX_BUFFERS`; each as `Buffer` below |
| | `metadata` | at most `MAX_METADATA` entries, each named `min_runtime_version`, `CONVERSION_METADATA` or `TFLITE_METADATA`, with a valid buffer index (descriptive only; any other name is refused, because a metadata entry such as `reduced_precision_support` changes how the interpreter computes) |
| | `metadata_buffer` | valid buffer indices |
| | `signature_defs` | at most 1, with valid tensor indices; not used for binding (ports are the subgraph's inputs and outputs) |
| | `description` | any string within `MAX_NAME_BYTES` |
| | `external_buffer_groups`, `external_buffers` | absent or empty |
| `SubGraph` | `inputs`, `outputs` | 1 to `MAX_PORTS` each; valid, unique tensor indices; no tensor in both lists |
| | `tensors` | at most `MAX_TENSORS`; each as `Tensor` below |
| | `operators` | at most `MAX_OPERATORS`; each as `Operator` below |
| | `name`, `debug_metadata_index` | any (not used) |
| `Tensor` (every tensor) | `type` | `FLOAT32`, `FLOAT16`, `INT32`, `INT64`, `UINT8`, `INT8`, `INT16` or `BOOL` |
| | `shape` | at most `MAX_RANK` entries, each ≥ 0 |
| | `shape_signature` | absent, or the same length as `shape`, each entry -1 or equal to `shape`'s |
| | `buffer` | a valid buffer index |
| | `quantization` | absent, or affine only (`scale`, `zero_point`, `quantized_dimension`, with `min`/`max` allowed); `details` absent |
| | `is_variable` | false |
| | `sparsity` | absent |
| | `variant_tensors` | empty |
| | `external_buffer` | 0 |
| | `has_rank` | any for internal tensors (the interpreter computes their shapes) |
| `Tensor` (graph inputs and outputs) | `type` | `FLOAT32`, `INT32`, `INT64` or `UINT8` (the `DType`s) |
| | rank | known: `has_rank` true, or `shape` non-empty. An empty `shape` with `has_rank` false is unknown rank and refused |
| | `name` | non-empty, valid UTF-8, within `MAX_NAME_BYTES`, unique among the inputs and unique among the outputs |
| | `quantization` | absent (no quantized ports) |
| | `buffer` | no data (a port is not a constant) |
| `Operator` | `opcode_index` | a valid index |
| | `inputs`, `outputs` | valid tensor indices; -1 (an omitted optional input) only in `inputs` |
| | `builtin_options` | any (the op's own options; LiteRT's verifier and the kernel check them) |
| | `builtin_options_2` | absent (not listed, so refused: only StableHLO ops carry it, and no slot uses them) |
| | `mutating_variable_inputs` | empty, or every entry false |
| | `intermediates` | empty |
| | `custom_options`, `custom_options_format`, `large_custom_options_offset`, `large_custom_options_size` | empty, default, 0, 0 |
| | `debug_metadata_index` | any (not used) |
| `OperatorCode` | `custom_code` | absent (a custom op, Flex included, is refused by name) |
| | `builtin_code` | `max(deprecated_builtin_code, builtin_code)` (the schema's rule) on the operator allow-list below |
| | `version` | ≥ 1 (LiteRT checks it against the kernel it registers) |
| `Buffer` | `data` | inline, at most `MAX_MODEL_BYTES` in total |
| | `offset`, `size` | 0 and 0 (no data outside the FlatBuffer) |

**The operator allow-list** is the builtin operators the archive's build registers, minus `CUSTOM`, `DELEGATE`,
`CALL`, `IF`, `WHILE`, `CALL_ONCE`, `STABLEHLO_CASE`, `STABLEHLO_SCATTER`, `STABLEHLO_REDUCE_WINDOW` and
`STABLEHLO_COMPOSITE` (they run other subgraphs), `VAR_HANDLE`, `READ_VARIABLE`, `ASSIGN_VARIABLE` (state), and every
kernel that keeps state between invocations at the pinned source: `RANDOM_STANDARD_NORMAL`, `RANDOM_UNIFORM`,
`MULTINOMIAL` (`tflite/kernels/random_ops.cc`) and `HASHTABLE`, `HASHTABLE_FIND`, `HASHTABLE_IMPORT`, `HASHTABLE_SIZE`
(`tflite/kernels/hashtable*.cc`). It is a constant in `scan.rs`, checked against the build by task 3a: the build's
resolver is `register.cc`'s 172 builtins, so 155 are allowed. It must contain the 22 builtins the face
graphs use: `ADD`, `CONCATENATION`, `CONV_2D`, `DEPTHWISE_CONV_2D`, `DEQUANTIZE`, `DIV`, `LOGISTIC`, `MAX_POOL_2D`,
`MEAN`, `MUL`, `NEG`, `PAD`, `PRELU`, `RELU`, `RESHAPE`, `RSQRT`, `SQRT`, `SQUARED_DIFFERENCE`, `STRIDED_SLICE`, `SUB`,
`SUM`, `TRANSPOSE`.

**Fields the reader doesn't know.** A table whose vtable is longer than the pinned schema's version of that table, with
any of the extra slots set, is refused ("a field this runtime does not know"). So a model written with a newer schema
fails closed instead of having a feature silently ignored.

**What the allow-list gives:**
- the probe's signature, with `-1` in `shape_signature` as `Dim::ANY` (section 3.4);
- a no-op `reset` that is sound (no variable tensors, ops or mutating inputs);
- no custom code, no data outside the model's bytes, and no second subgraph.

**The allow-list is the contract.** A change to it is a design change recorded here, not a detail of the code.
Corner cases found later in the implementation are covered by task 3a's tests against this table.

**What it costs.** Once per probe and once per load, both cold. It reads only table fields, a few thousand for the face
graphs, so it is expected well under a millisecond; task 3a measures it on the landmark graph and puts the number in its
pull request. About 250 lines with its unit tests.

**Then LiteRT.** `TfLiteModelCreate` runs LiteRT's own FlatBuffer verifier as a second check, before the interpreter
is built. A TFLite file can't name native code to load. User TFLite models run in the sandboxed worker like ONNX ones;
the worker links `omuzaki-ai-litert`.

**Tests use synthetic FlatBuffers built in the test** (a small builder in the test helpers): a fixed-shape `ADD` graph
that is accepted, then one rejected case per allow-list row (task 3a lists them), and truncated or out-of-range files.
No downloaded or generated model files are committed. The gated tests also scan the three real face graphs, which must
be accepted.

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
frames. That is tracker work, not runtime work, and is filed as its own follow-up (ADR 0061, task cut): reuse the ROI
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
  on, 1 thread, run by `tools/models/litert_reference.py` in a throwaway venv (`python -I`), the way #1732 measured it.
  The same script runs `onnxruntime` on the converted graphs for the ONNX gate below, and writes every input and output
  as raw bytes with an index (`reference.txt`) that `tests/parity.rs` reads (`OMUZAKI_LITERT_REFERENCE`).
- **The frames:** the same input tensors go to both sides: the seeded random tensors of the converter's own check, and
  synthetic faces the script draws from ellipses (skin, eyes, brows, nose, mouth) at three positions and sizes, for the
  detector and the landmark graph. Both runtimes read the identical bytes the script wrote; nothing is committed. (The
  crops our pipeline cuts from the kit's frames are the tracker-level test of task 5, which may use `omuzaki-track`.)
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
  and takes the archive from the LiteRT build it calls in the same run (8.2).
- The tracker-level comparison against MediaPipe's own `FaceLandmarker` on real video is the separate tracker parity work;
  this design covers the runtime.

## 8. Build and hosting

### 8.1 What is built

One shared library per platform, `omuzaki_litert` (`libomuzaki_litert.so`, `omuzaki_litert.dll`,
`libomuzaki_litert.dylib`), holding LiteRT's interpreter C API, the XNNPACK delegate, the builtin kernels and our shim,
and exporting only the symbols of section 3.2. Platforms: `linux-x64` (CI and development), `win-x64` (`R`, the
reference), `macos-arm64` (`M`). Linux arm64 is added when a tier needs it.

### 8.2 The workflow: `.github/workflows/runtime-litert.yml`

A workspace hub: the Director signs it on its pull request. It is a CI build with read-only permissions: its output is
a checksummed workflow artifact (`actions/upload-artifact`), which needs no new permission. Publishing to users (a
GitHub Release, which needs `contents: write`) is **pending the owner's decision**, which the Director is asking for;
section 8.5 says what would change if it is approved.

**A reusable workflow, consumed in the same run.** Downloading an artifact from *another* run needs a token with
`actions: read`, and a new workflow permission is an access change only the owner makes (CLAUDE.md). So no consumer
downloads across runs. The build is a reusable workflow (`on: workflow_call`). Each consumer calls it as a job of its own
run and takes the archive from that run with `actions/download-artifact`, which needs no extra permission. The consumers
are the nightly parity job (task 4), the `R` timing probe's job (#1731, task 6) and the build task's own evidence run.

**Rebuilds are skipped with a cache.** The built archive is cached with `actions/cache`. The key hashes every build
input:
- `tools/litert/**`, which holds the LiteRT tag and commit, the Bazelisk and toolchain pins, the shim, the Bazel overlay
  and every script the workflow runs (the workflow runs no script and no composite action from anywhere else);
- the workflow file itself;
- the platform and the runner image, by name and version (`ImageOS` and `ImageVersion`, which GitHub-hosted runners
  set). The image carries the toolchains: MSVC on Windows, Xcode on macOS, and the container runtime on Linux.

Bazel's version is fixed by LiteRT's `.bazelversion` at the pinned commit, and the Linux container by its digest in
`pins.toml`, so both are inside the hash of `tools/litert/**`. The `v1` prefix is bumped by hand to drop every entry.
With a warm cache a consumer's call restores the archive in seconds, then runs the `check` job anyway.

```yaml
      - id: image   # runner env vars aren't in the `env` context, so read them in a step
        shell: bash
        run: echo "id=${ImageOS}-${ImageVersion}" >> "$GITHUB_OUTPUT"
      - uses: actions/cache/restore@<commit SHA>   # actions/cache@<commit SHA> only when inputs.save-cache
        with:
          path: out/omuzaki-litert-2.3.0-${{ matrix.platform }}.${{ matrix.ext }}
          key: litert-v1-${{ matrix.platform }}-${{ steps.image.outputs.id }}-${{ hashFiles('tools/litert/**', '.github/workflows/runtime-litert.yml') }}
``` The repository's rule that only `cache-warm.yml` saves caches (CONTRIBUTING.md, "Continuous integration")
holds:
- `cache-warm.yml` calls the workflow with `save-cache: true` on pushes to `main` that change `tools/litert/` or the
  workflow;
- every other caller only restores (`actions/cache/restore`), and on a miss builds without saving.

```yaml
on:
  workflow_call:
    inputs:
      save-cache: { type: boolean, default: false }   # true only from cache-warm.yml
    outputs:   # one named output per platform, and the map; see collect
      sha256_linux_x64:   { value: ${{ jobs.collect.outputs.sha256_linux_x64 }} }
      sha256_win_x64:     { value: ${{ jobs.collect.outputs.sha256_win_x64 }} }
      sha256_macos_arm64: { value: ${{ jobs.collect.outputs.sha256_macos_arm64 }} }
      sha256:             { value: ${{ jobs.collect.outputs.sha256 }} }   # {"linux-x64": "…", "win-x64": "…", "macos-arm64": "…"}
  workflow_dispatch: {}                                # maintainers, on main, once the file is there
permissions: {}
jobs:
  build:        # matrix.platform: linux-x64 (ubuntu, in a container pinned by digest), win-x64 (windows-2022), macos-arm64 (macos-14)
    permissions: { contents: read }
    # 1. check out this repo (actions pinned by commit SHA)
    # 2. restore the archive from the cache (the key above); on a hit, skip to 6
    # 3. fetch LiteRT at tag v2.3.0 and check the tag's commit against tools/litert/pins.toml
    # 4. install Bazelisk by pinned release URL and SHA-256; Bazel 7.7.0 comes from LiteRT's .bazelversion
    # 5. copy tools/litert/BUILD.omuzaki and omz_litert_shim.c into the tree; build the shared-library target with
    #    -c opt and the flags in pins.toml (x86-64: the default ISA set, XNNPACK dispatches at run time); collect
    #    the notices (8.3) and BUILDINFO.json; pack the archive; save it to the cache only when save-cache is true
    # 6. hash the archive; upload it as artifact litert-2.3.0-<platform> and its digest, a one-line file
    #    <platform>.sha256, as artifact litert-digest-<platform> (retention 7 days: consumers read them in the same run)
  collect:      # not a matrix: a matrix job's output is a single value, so the three digests are merged here
    needs: build
    permissions: { contents: read }
    outputs:
      sha256_linux_x64:   ${{ steps.merge.outputs.linux_x64 }}
      sha256_win_x64:     ${{ steps.merge.outputs.win_x64 }}
      sha256_macos_arm64: ${{ steps.merge.outputs.macos_arm64 }}
      sha256:             ${{ steps.merge.outputs.map }}
    # actions/download-artifact with pattern litert-digest-*, merge-multiple; step `merge` checks that there are
    # exactly three files, each a 64-character lowercase hex digest, and writes the four outputs
  check:        # matrix as build; needs: [build, collect]
    permissions: { contents: read }
    # download the same run's artifact; check its SHA-256 against fromJSON(needs.collect.outputs.sha256)[matrix.platform]; load the library with a
    # small C smoke program: TfLiteVersion, omz_litert_abi, one face graph fetched by tools/models/mediapipe-face.sh,
    # all nodes delegated to XNNPACK (the delegate's log line), outputs within 1e-4 of the ai-edge-litert wheel (7.2)
  # no release job: publishing waits for the owner's decision (8.5)
```

A consumer job, for example nightly's parity job:

```yaml
  litert:
    uses: ./.github/workflows/runtime-litert.yml
    permissions: { contents: read }
  parity:       # matrix.platform: linux-x64, win-x64
    needs: litert
    permissions: { contents: read }
    env:
      OMUZAKI_LITERT_SHA256: ${{ fromJSON(needs.litert.outputs.sha256)[matrix.platform] }}
    # actions/download-artifact name litert-2.3.0-${{ matrix.platform }} (same run) into $OMUZAKI_LITERT_CACHE;
    # fail if OMUZAKI_LITERT_SHA256 is empty; then the gated tests. A job for one platform reads its named output
    # instead, e.g. needs.litert.outputs.sha256_win_x64 in the R probe's Windows job.
```

- **Pinned sources:** LiteRT by tag and commit (`tools/litert/pins.toml`). Its dependencies come from its own Bazel
  workspace, which pins each archive by SHA-256, so the dependency list is fixed by the tag.
- **Pinned toolchains:**
  - Linux: the container image by digest, with a glibc baseline no newer than the wheel's `manylinux_2_27` floor (to be
    confirmed), and its clang by version;
  - Windows: the MSVC toolset version (`-vcvars_ver`) on the Windows image;
  - macOS: the Xcode version on the macOS image, with macOS 13 as the deployment target (the `M` tier).

  The job prints every version into `BUILDINFO.json` and fails when one differs from `pins.toml`.
- **What a consumer trusts.** A rebuild after a cache miss isn't promised to be byte-identical, so CI can't check a
  fixed SHA-256 committed in the repository. Inside a run, the trust root is the build job's output:
  - the consumer checks the downloaded archive against its platform's SHA-256 from `collect`, computed on the runner
    that built it. Within a run, artifacts and outputs share one trust domain, so this guards against a truncated,
    mixed-up or wrong-platform file, not against someone who controls the run;
  - it checks `BUILDINFO.json` against `pins.toml`;
  - at load, it checks `TfLiteVersion()` and `omz_litert_abi()`.

  The `R` probe's artifact bundles the archive with that SHA-256, and the probe checks it before loading. A fixed pin
  for people's machines comes only with publishing (8.4, 8.5).
- **Sessions never dispatch the workflow.** Maintainers do, and the build task's evidence run is triggered as #1747
  says.
- **Why Bazel and not CMake:** LiteRT's CMake (`tflite/CMakeLists.txt` at `v2.3.0`) needs a TensorFlow checkout beside
  it (`TENSORFLOW_SOURCE_DIR`, for TSL and XLA files), and fetches dependencies by git tag; Bazel is LiteRT's own
  release build, with SHA-256-pinned archives. The CMake path is the fallback if the Bazel target can't produce the
  library (section 12).

### 8.3 The archive

```
omuzaki-litert-2.3.0-<platform>/
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

### 8.4 The build table (the `builds.rs` equivalent)

`crates/omuzaki-ai-litert/src/builds.rs`, shaped like ONNX Runtime's table:

```rust
pub const LITERT_VERSION: &str = "2.3.0";
pub const OMZ_LITERT_ABI: u32 = 1;
pub static BUILDS: &[Build] = &[
    Build { id: "linux-x64", os: "linux", arch: "x86_64", asset: "omuzaki-litert-2.3.0-linux-x64.tar.gz",
            archive: Archive::TarGz, library: "libomuzaki_litert.so" },
    // win-x64 (.zip, omuzaki_litert.dll), macos-arm64 (.tar.gz, libomuzaki_litert.dylib)
];
```

**For now there is no SHA-256, size or URL in the table**, because CI's archive is rebuilt on a cache miss and its
trust root is the run's own build output (8.2). The tests' source reads the expected SHA-256 from
`OMUZAKI_LITERT_SHA256`, which the calling workflow sets from the build job's output, and refuses an archive without
one. Size and SHA-256 are still checked before anything is unpacked, as `omuzaki-ai-ort`'s source does.

If publishing is approved (8.5), the published bytes get a fixed `PinnedFile` (URL, SHA-256, size) in this table, and the
app fetches through `FileSource` like any pinned file. Until then the app has no way to get the runtime, and the face
graphs' app default stays `onnx` (ADR 0061, task 5).

### 8.5 Publishing to users: pending the owner's decision

Not decided here. If the owner approves a GitHub Release, the change would be one added job that publishes the
already-checked artifacts **unchanged** (the same bytes and SHA-256) under an immutable tag such as
`runtime-litert-2.3.0-<build>`, holding `contents: write` in that job alone; then a fixed pin (URL, SHA-256, size) is
added to each row of the build table
and the app's default for the face graphs can move to `tflite`. If the owner decides otherwise, the runtime stays a CI
and probe runtime until another hosting is chosen.

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
- **Schema survey of the three face graphs** (the wheel's schema bindings, for the scan's allow-list):
  - one subgraph each, schema version 3, no custom ops;
  - tensor types FLOAT32, FLOAT16 (the weights, before `DEQUANTIZE`) and INT32; ports all FLOAT32;
  - no variable tensors, sparsity, quantization, mutating variable inputs, intermediates, custom options, external
    buffers or out-of-FlatBuffer buffer data;
  - metadata named only `min_runtime_version`, `CONVERSION_METADATA` and `TFLITE_METADATA`, and one signature def in
    the blendshape graph;
  - two internal blendshape tensors have `has_rank` false and an empty shape (shapes computed at run time), so the
    known-rank rule applies to ports only.

## 10. What changes where

| Where | Change | Task |
|---|---|---|
| `crates/omuzaki-ai-native` (new, Lib) | the pinned-archive loader moved out of `omuzaki-ai-ort` | 1 |
| `crates/omuzaki-ai-ort/src/library.rs` | uses `omuzaki-ai-native`; behaviour unchanged | 1 |
| `.github/workflows/runtime-litert.yml` (reusable), one job in `cache-warm.yml`, `tools/litert/` | the build (a checksummed same-run artifact, cached), the shim, the notices check, the pins | 2 |
| `docs/licensing/deps/` | the rows of ADR 0061 decision 6 | 2 |
| `crates/omuzaki-ai-litert` (new, Implementation) | the `tflite` runtime, with its model scan (`src/scan.rs`) | 3a, 3b |
| `crates/omuzaki-ai-litert/tests`, `tools/models/litert_reference.py`, `nightly.yml` (calls the build workflow) | parity and determinism | 4 |
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
| That `ImageOS` and `ImageVersion` are set on each OS where the `image` step runs (the Linux build runs in a container, where the runner's variables may not be passed through; if not, read them in a step on the host before the container starts) | 2 |
| Whether `XNN_FLAG_DONT_SPIN_WORKERS` in the delegate's `runtime_flags` reaches pthreadpool's flags at this XNNPACK commit (the idle-CPU test proves the effect either way) | 3b |
| Answered (3a): the default XNNPACK delegate is never applied lazily through our C API. `TfLiteInterpreterCreate` builds its resolver with `CreateOpResolver()`, which at `v2.3.0` is `BuiltinOpResolverWithoutDefaultDelegates` (`tflite/core/create_op_resolver_with_builtin_ops.cc`); its constructor clears `delegate_creators_` (`tflite/core/kernels/register.h`), so `ApplyLazyDelegateProviders` has nothing to apply, fully delegated or not. On a partially delegated user model, the nodes XNNPACK doesn't take run on the builtin kernels with the interpreter's thread count, which is why `SetNumThreads` gets the same count | 3a |
| The scan's operator allow-list against the builtin operators the archive's build actually registers, and the scan's measured cost on the landmark graph | 3a |
| Each licence of decision 6 against the actual source tree that was built, including whether FP16 is still a separate dependency (at the pinned XNNPACK commit it appears vendored as `src/xnnpack/fp16.h`) and whether FXdiv is linked (pthreadpool's CMake fetches it; the Bazel build may not) | 2 |
| MediaPipe's default XNNPACK thread count on desktop (`GetXnnpackDefaultNumThreads`), for the comparison in the probe's report | 6 |
| The size of each archive and library, against `installer.size` (runtimes are fetched on demand, but the V0 zip may bundle one) | 2 |
