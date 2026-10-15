---
title: "Windows GPU inference for AMD and Intel without DirectML >= 1.25 (2026-10-15)"
summary: "Which way to run the ONNX models on AMD and Intel GPUs on Windows now that Microsoft ships no DirectML build after ONNX Runtime 1.24.4; the answer is ONNX Runtime's separate WebGPU plugin execution provider, with CPU as the default until tier R shows a win."
area: research
---

# Windows GPU inference for AMD and Intel without DirectML >= 1.25 (2026-10-15)

Research for #959 (parent #580, ML-01). Source marks: **verified** = I read the primary source (a package, licence,
release page or API listing) on 2026-10-15; **secondary** = someone's summary; **unverified** = not confirmed. Page
facts were read through a fetch tool that summarises pages; every figure that decides something was cross-checked
against a package I downloaded and opened, or against a registry API, and says so. Nothing here was measured on tier
`R`; section 6 is the plan for that.

## 1. The answer

- **Rank 1: ONNX Runtime's WebGPU plugin execution provider (EP).** It is a separate Microsoft package, MIT licensed,
  that loads into the ONNX Runtime we already pin (it asks for 1.24.4 or later; we pin 1.28.3). It runs on Direct3D 12
  through Dawn, so one file set covers AMD, Intel and NVIDIA GPUs on any Windows 10/11 that has a D3D12 driver. The
  issue's "no WebGPU archive for 1.28.3" is true but beside the point: since 1.24 the WebGPU EP ships as its own
  package (PyPI, NuGet), not inside the core archive. The Windows x64 payload is 13.1 MB compressed, 30.4 MB unpacked.
- **Rank 2: DirectML at ONNX Runtime 1.24.4 (a lower `ort` API level).** The only option that is proven on AMD and
  Intel GPUs today, and the cheapest in size. It is a dead end: Microsoft says DirectML is in maintenance mode, with no
  new operators or optimisations. Keep it as the fallback if rank 1 fails the `R` measurement.
- **Rank 3: Windows ML.** It gives a DirectML-capable ONNX Runtime 1.27.1 (API 27, which matches our `api-27`) and a
  catalogue of vendor EPs. But its redistribution terms need an end-user agreement and an indemnity to Microsoft, vendor
  EPs need Windows 11 24H2, and AMD's GPU EP (MIGraphX) needs RDNA 3 or newer, which tier `R`'s iGPU is not. A licence
  call for the Director before anyone builds on it.
- **Rank 4: LiteRT's WebGPU accelerator.** It is the same Dawn/D3D12 stack as rank 1, so it adds no hardware. It only
  runs TFLite models through LiteRT's CompiledModel API, which ADR 0061 does not bind. Revisit only for the three face
  graphs, and only if rank 1 shows GPUs help them at all.
- **Rank 5: CPU only.** It stays the shipped default for every model until `R` shows a GPU path wins on `track.face`
  and `cpu.total` (spec v3 section 10). The face graphs are small; a GPU round trip may cost more than it saves.
- **Not worth pursuing now:** Burn (code generated at build time, so no user models), wonnx (archived), ncnn and MNN
  (a third runtime and a model conversion), the OpenVINO plugin EP as an Intel-only extra (optional later, section 4).
- **Decision this supports:** keep `Provider::WebGpu` in the `omuzaki-ai` contract and change what it means: the plugin
  EP, loaded from a second pinned file set, instead of an in-core build. No contract change is needed (section 5).
  Do not pin DirectML 1.24.4 now. Drop `Provider::DirectMl` from the Windows default order, or leave it as a skipped
  provider that says why, until `R` says rank 1 fails.
- **What this note does not settle:** speed. Nobody has measured a GPU on these models. The `R` plan in section 6
  settles it, and it costs one owner command.

## 2. What changed, and what the issue's facts check out as

| Claim in the issue | Result | Source |
| --- | --- | --- |
| v1.28.3 has no DirectML or WebGPU archive | **verified.** `onnxruntime-win-x64-directml-1.28.3.zip` and `-webgpu-1.28.3.zip` return 404; the release lists ten assets (Linux, macOS, `win-x64`, `win-x64-gpu_cuda12`, `-gpu_cuda13`, `win-arm64`, `win-arm64x`) | `api.github.com/repos/microsoft/onnxruntime/releases/tags/v1.28.3`, 2026-10-15 |
| NuGet DirectML stops at 1.24.4 | **verified.** Last versions listed: 1.23.0, 1.24.1, 1.24.2, 1.24.3, 1.24.4. PyPI `onnxruntime-directml` also ends at 1.24.4 | `api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/index.json`, `pypi.org/pypi/onnxruntime-directml/json`, 2026-10-15 |
| Why it stopped | **unverified.** I found no Microsoft statement of a date or reason for the missing 1.25+ packages. The v1.31.0 notes still list DirectML fixes, so the EP source is alive; only the prebuilt package is gone | release pages v1.25.0 and v1.31.0, 2026-10-15 |
| Newer ONNX Runtime exists | **verified.** v1.31.0 (09 Oct), 1.30.0 (10 Sep), 1.29.1, 1.28.3 (02 Oct) | `github.com/microsoft/onnxruntime/releases`, 2026-10-15 |

What Microsoft says about DirectML (all **verified**, read 2026-10-15):

- ONNX Runtime docs: "**Note: DirectML is in sustained engineering.**" and "DirectML continues to be supported, but new
  feature development has moved to WinML." It uses DirectML 1.15.2 and opsets up to 20
  (`onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html`).
- DirectML repo: "DirectML is in maintenance mode", "no new functionality or feature updates are planned", security
  and compliance fixes continue, and it "will continue to ship with future versions of Windows"
  (`github.com/microsoft/DirectML`).
- Windows ML WebGPU page: "no new operator coverage, no new hardware-specific optimizations, and no major performance
  investments are planned. Treat DirectML as a compatibility backstop ... assume DirectML's relative position will
  decline over time" (`learn.microsoft.com/en-us/windows/ai/new-windows-ml/webgpu-ep`).

So DirectML still works and will not get better. Any plan that pins it is a plan with an expiry date.

## 3. The five options, one by one

### 3.1 Option 2: the WebGPU plugin EP (recommended)

| Item | Finding | Source and mark |
| --- | --- | --- |
| What it is | "ONNX Runtime WebGPU Plugin Execution Provider": a shared library registered into a running ONNX Runtime. Not part of the core archive | package README inside the wheel; **verified** (downloaded and opened) |
| Versions | 0.1.0, 0.2.1, 0.3.0, 0.4.0 (released 2026-09-22). v1.31.0 notes say "WebGPU Plugin EP v0.5.0 are matched with ONNX Runtime 1.31.0" | NuGet index; GitHub release `plugin-ep-webgpu/v0.4.0`; **verified** |
| Maturity | PyPI classifier "Development Status :: 4 - Beta". Windows ML docs call the packaged form "experimental" and "not yet recommended for performance-critical or ultra-optimized workloads" | wheel `METADATA`; `learn.microsoft.com/.../webgpu-ep`; **verified** |
| ONNX Runtime it needs | "version `1.24.4` or later"; "If the installed ONNX Runtime is incompatible, the plugin EP will report an error when its library is registered" | wheel README; **verified**. That 1.28.3 works is **unverified** until run |
| Contrib-op caveat | v1.31.0 notes: "Core and plugin must use the same contrib-op schemas. Building both from the same ONNX Runtime revision is recommended." Core graph fusions that make `com.microsoft` ops may not match a plugin built from another revision | release v1.31.0; **verified** as written, effect on our models **unverified** |
| Windows x64 payload | wheel `onnxruntime_ep_webgpu-0.4.0-py3-none-win_amd64.whl`, 13,149,847 bytes. Inside: `onnxruntime_providers_webgpu.dll` 10,954,080; `dxcompiler.dll` 17,986,360; `dxil.dll` 1,508,704 (30.4 MB unpacked); `LICENSE`, `ThirdPartyNotices.txt` | PyPI JSON and the opened wheel; **verified** |
| Other channels | NuGet `Microsoft.ML.OnnxRuntime.EP.WebGpu` 0.4.0 (46.7 MB, all platforms). The GitHub release for 0.4.0 has no binary assets | `api.nuget.org`; GitHub release API; **verified** |
| GPUs it reaches | "Windows: Direct3D 12 or Vulkan" through Dawn. Windows ML: "compatible with almost all DirectX 12-capable GPUs"; "roughly 11th-gen Intel integrated graphics or NVIDIA Turing-class and newer recommended" | `onnxruntime.ai/docs/execution-providers/WebGPU-ExecutionProvider.html`; `.../webgpu-ep`; **verified** |
| Known limits | FP32 and FP16; "Quantized non-LLM INT8 models are not supported on the WebGPU path"; unsupported operators fall back to the CPU and give "mixed CPU/GPU execution and unpredictable performance" | `.../webgpu-ep`; **verified** |
| Device choice | README: "The WebGPU EP currently accepts one EP device and selects the physical GPU independently." We cannot pick the adapter on a laptop with two GPUs by choosing among ORT devices | wheel README; **verified** |
| Releases | v0.4.0 notes: kernel speed-ups, "subgroup size pinned to 32" in subgroup-matrix MatMul/Gemm; v1.31.0: "subgroup-matrix kernels on AMD GPUs". Whether pinning 32 is safe on Intel (subgroup sizes 8/16/32) is **unverified** | release pages; **verified** as written |

Licence (the decider): the wheel's `LICENSE` is "MIT License / Copyright (c) Microsoft Corporation", and `METADATA`
says `License-Expression: MIT`. Its `ThirdPartyNotices.txt` (344 KB) lists Dawn ("Copyright 2017-2023 The Dawn & Tint
Authors"), abseil-cpp (Apache-2.0), protobuf, and the DirectX Shader Compiler's LLVM "Release License" (University of
Illinois/NCSA, a permissive BSD-style licence). The only "non-commercial" hits in the file are the phrase "for any
purpose, commercial or non-commercial" in grants. **Verified** by reading the files; the per-component licence rows
for `docs/licensing/deps/` are follow-up task F2. This is the MIT package, not the Windows ML MSIX of the same EP, which
carries different terms (section 3.2).

Packaging fit: it is one zip with three DLLs, which our pinned-archive loader already handles (zip, size cap, atomic
rename). It does not fit the existing `Build` table, which assumes one core library under `lib/` (section 5).

Risks, in order: (1) speed on small models is unknown; (2) INT8 models cannot use it; (3) first-run shader compilation
may stall the first frame; (4) "Beta" means operator and driver bugs on some AMD and Intel drivers; (5) plugin and core
version skew (contrib ops). All five are measurable in section 6.

### 3.2 Option 1: Windows ML

| Item | Finding | Source and mark |
| --- | --- | --- |
| What it is | "the Windows-supported and maintained copy of ONNX Runtime (ORT)", system-wide or self-contained, plus an `ExecutionProviderCatalog` that downloads vendor EPs on demand | `learn.microsoft.com/en-us/windows/ai/new-windows-ml/overview`; **verified** |
| Version shipped | Windows ML 2.4.89 (current release, 2026-09-22) ships ONNX Runtime 1.27.1 (2026-07-10). ORT 1.27 is API level 27, equal to our `api-27`. Experimental 2.7 ships 1.30.0 | `.../onnx-versions`; **verified**. "ORT 1.N is API N" is **secondary** (the 1.25.0 notes say "ORT_API_VERSION of 25") |
| Included GPU EP | "CPU" and "DirectML (legacy)" are included; the rest are "downloaded on demand" | `.../supported-execution-providers`; **verified** |
| Vendor EPs (2.x) | MIGraphX (AMD), NvTensorRtRtx, OpenVINO (Intel), QNN, VitisAI (AMD NPU), WebGPU (experimental) | same page; **verified** |
| AMD GPU limit | MIGraphX: "AMD RDNA 3 or later GPU", driver 25.10.13.09 or later, "not supported for GenAI scenarios today". Tier `R`'s iGPU is a Radeon 680M, which is RDNA 2 (`docs/research/2026-10-reference-machine-definition.md`), so MIGraphX cannot serve the reference tier | same page; **verified** |
| OS limit | "Hardware-optimized execution providers for NPUs and specific GPU hardware require Windows 11 version 24H2 (build 26100) or greater." CPU and DirectML work on all Windows versions Windows App SDK supports | `.../overview`; **verified** |
| Size, self-contained | Package `Microsoft.Windows.AI.MachineLearning` 2.4.89 is 51.6 MB. Win-x64 natives: `onnxruntime.dll` 23,086,440; `DirectML.dll` 18,700,264; `Microsoft.Windows.AI.MachineLearning.dll` 971,504 (about 42.8 MB). Docs say about 41 MB | the opened nupkg; **verified** |
| C access | A plain C header, `WinMLEpCatalog.h` (`WinMLEpCatalogCreate`, `...EnumProviders`, `...FindProvider`, ready state, library path), plus ONNX Runtime headers under `include/winml/`. No WinRT needed for self-contained C/C++. "Framework-dependent C/C++ support is not currently available." | the opened nupkg; `.../distributing-your-app`; **verified** |

Licence: the package's `license.txt` is "MICROSOFT SOFTWARE LICENSE TERMS / Microsoft Windows ML Runtime" and
`requireLicenseAcceptance` is true. The clause that decides it, section 3(b): "For any code you distribute, you must:
... (ii) require distributors and external end users to agree to terms that protect it and Microsoft at least as much
as this agreement; and (iii) indemnify, defend, and hold harmless Microsoft from any claims ... related to the
distribution or use of your applications". Section 1(a) limits use to "solely for use on Windows". The WebGPU EP MSIX
has the same terms (`.../webgpu-ep-license`). **Verified.** This is not non-commercial, so it is not an automatic NO.
But it is exactly the shape ADR 0010 refused for Live2D ("an indemnity on to every distributor and user ... which a
free, open-format product should not ask of its users"). The policy admits a proprietary runtime "with a redistribution
grant" whose conditions are recorded. Whether this grant is acceptable is the Director's and owner's call; I would not
build on it unless rank 1 and 2 fail.

Cost if taken: the app would load Windows ML's `onnxruntime.dll` (1.27.1) instead of our pinned 1.28.3 on Windows.
`omuzaki-audio` loads the same library (`builds.rs` doc), so both crates would move together, and the CUDA path would
still need a separate pin. The upside over rank 1 is vendor EPs (OpenVINO for Intel) and the OS-managed update path.

### 3.3 Option 3: DirectML 1.24.4 at a lower `ort` API level

| Item | Finding | Source and mark |
| --- | --- | --- |
| Package | `Microsoft.ML.OnnxRuntime.DirectML` 1.24.4, 12,458,649 bytes; depends on `Microsoft.AI.DirectML` 1.15.4. PyPI wheels are about 25 MB | nuspec and `Content-Length`; **verified** |
| DirectML DLL | `Microsoft.AI.DirectML` 1.15.4: `bin/x64-win/DirectML.dll` 18,527,776 bytes | opened nupkg; **verified** |
| ONNX Runtime licence | MIT, as for the other pinned builds (`builds.rs` doc, register row `ort`) | **secondary** (repo note); the nupkg `LICENSE` file is the thing to read when pinning |
| DirectML licence | "MICROSOFT DIRECTX MACHINE LEARNING (DIRECTML)". Section 1(a): "you may install and use any number of copies of the software, and solely for use on Windows and Xbox. You may copy and distribute the software (i.e. make available for third parties) in applications and services you develop". No indemnity clause (searched for "indemn"; none). `requireLicenseAcceptance` false | opened `LICENSE.txt`; **verified**. ADR 0010 already lists DirectML among admitted grants |
| `ort` fit | `ort` 2.0.0-rc.13 has features `api-17` through `api-28`; `api-17`..`api-27` are default and each enables the one below. So `api-24` exists. The workspace pins `api-27`; ONNX Runtime 1.24.4 is API 24, so the pin has to drop to `api-24` | docs.rs feature list; **verified**. That an `api-27` build refuses a 1.24 library is **unverified** but expected |
| Cost | Lower the workspace `ort` level (every crate that uses an API above 24 loses it: I did not audit which), move `omuzaki-audio` and `omuzaki-ai-ort` to 1.24.4 together, and keep two ONNX Runtime versions if CUDA stays at 1.28.3 (one library per process, so it cannot be both) | **unverified**; follow-up F7 |
| Limits | DirectML EP supports opsets up to 20; no memory-pattern optimisation or parallel run; frozen at DirectML 1.15.x | DirectML EP page; **verified** |

It would work on tier `R` today, so it is the safest fallback. But it ties the whole workspace to an ONNX Runtime from
spring 2026 for a provider Microsoft has stopped improving, and it is the one option whose cost lands on other crates.

### 3.4 Option 4: CPU only

Every shipped model already runs on the CPU, and ADR 0061 moves the face graphs to a faster CPU path (LiteRT with
XNNPACK: 6.4 to 6.7 ms against 14 to 16.6 ms on a 4-vCPU box, #1732). That ADR's budget is `track.face` at 5 ms p95 on
`R`. A GPU path has to beat a CPU path that is itself still being tuned, and a GPU adds a copy in, a copy out and a
synchronisation per frame. For per-frame face models, CPU may simply be right. GPU matters more for the ONNX-native
models the ADR keeps on `onnx` (`vad@1`, `voice_conversion@1`, `voice_to_face@1`, `tts@1`), and the `R` plan includes a
non-face model for that reason.

### 3.5 Option 5: LiteRT GPU

| Item | Finding | Source and mark |
| --- | --- | --- |
| Windows GPU backend | Platform table: "Windows: WebGPU (Direct3D)", "Linux: WebGPU (Vulkan)", "macOS: Metal", "Android: OpenCL + OpenGL" | `developers.google.com/edge/litert/next/gpu`; **verified** |
| API | Hardware is chosen "as a parameter when creating a Compiled Model" (the `CompiledModel` API). ADR 0061 binds the TFLite interpreter C API and calls CompiledModel "the later path" | same page; ADR 0061; **verified** |
| Prebuilt Windows x64 | v2.3.0: `libLiteRt.dll` 12,173,824 bytes and `libLiteRtWebGpuAccelerator.dll` 30,891,008 bytes, from `storage.googleapis.com/litert/binaries/2.3.0/windows_x86_64/` | `developers.google.com/edge/litert/next/cpp_sdk`; `Content-Length` of both URLs; **verified** |
| Extra file need | "Windows needs DirectXShaderCompiler. Download the dxc_2025_07_14.zip ... copy `dxil.dll` and `dxcompiler.dll` into the same directory as the executable" (about 19.5 MB more) | GPU page; sizes from the plugin wheel; **verified** |
| Maturity | Desktop GPU backends "prebuilt for Windows (x64)" since v2.1.0 (19 Dec; the page omits the year); the Windows Python wheel got GPU in 2.1.3; no stability label on the GPU page. v2.3.0 is the latest release (07 Oct) and is the tag ADR 0061 builds | `github.com/google-ai-edge/LiteRT/releases`; **verified** |
| Licence | LiteRT: Apache-2.0 (`LICENSE` file read). The accelerator links Dawn and uses DXC; I could not read the prebuilt DLL's notices, so its licence set is **unverified** | GitHub `LICENSE`; **verified** for LiteRT only |
| Build from source | ADR 0061 builds LiteRT with Bazel. Whether the Windows WebGPU accelerator target builds in that workflow is **unverified**; the C++ SDK page's GPU build instructions are Android-only | cpp_sdk page; **verified** as written |

LiteRT's Windows GPU path is the same Dawn on Direct3D 12 as rank 1, so it brings no extra hardware. It only reads
TFLite models, so it cannot serve the ONNX-native slots, and it needs a second API surface (CompiledModel) that ADR
0061 explicitly leaves out. Treat it as a later optimisation for `face_tracker@1` if and only if section 6 shows the
WebGPU plugin helps the face graphs.

## 4. Options outside the five

| Option | Licence (mark) | Maturity and fit | Verdict |
| --- | --- | --- | --- |
| OpenVINO plugin EP (Intel only) | wheel `METADATA`: MIT (**verified**). The wheel ships no LICENSE or notices file for the bundled OpenVINO libraries (**verified**, opened wheel); OpenVINO itself is Apache-2.0 (**secondary**) | PyPI `onnxruntime-ep-openvino` 1.7.0 (2026-08-20), Windows x64 wheel 67.7 MB; needs ORT >= 1.23.0 (wheel README). Docs: the built-in OpenVINO EP "is deprecated", the plugin is recommended. GPU part: `openvino_intel_gpu_plugin.dll` 34.3 MB, `openvino.dll` 16.3 MB, about 60 MB without the 82 MB NPU compiler | Optional Intel-only extra after rank 1. Notices gap is a licence blocker until fixed upstream or by us (F2) |
| MIGraphX (AMD GPU) | See Windows ML | Only through the Windows ML catalogue here; RDNA 3 or newer | Does not serve tier `R` |
| Burn with the wgpu backend | Burn: MIT (`LICENSE-MIT` read, **verified**) | `burn-onnx` 0.21.0 (2026-05-14) generates Rust source from an ONNX file at build time: "No graph runtime". 160 of 201 ONNX operators; 717 of 1,615 upstream backend tests pass | **Reject.** Models would be compiled into the binary, which breaks the registry and user models (ADR 0016, 0017) |
| wonnx (wgpu) | Apache-2.0 or MIT (**verified**) | Repository "archived by the owner on May 7, 2025"; DX12 "(W10 only)"; 64-bit integers and several MatMul shapes unsupported | **Reject** |
| tract | not checked | I found no GPU backend (**unverified**) | Not a GPU option |
| ncnn (Vulkan) | BSD-3-Clause (**verified**) | Needs ONNX converted offline with `pnnx`; its own model format and runtime | **Reject** as a third runtime and conversion step; only a last resort if WebGPU fails |
| MNN (Vulkan) | Apache-2.0 (**verified**) | Own format via a converter (158 ONNX operators cited); README does not mention Windows | **Reject** for the same reasons |

## 5. What the `omuzaki-ai` seam needs

Read at origin/main: `crates/omuzaki-ai-api/src/{session,runtime}.rs`, `crates/omuzaki-ai-ort/src/{builds,graph}.rs`,
`docs/design/ai-api.md` section 9, ADR 0061.

**Contract (`omuzaki-ai-api`): no change for rank 1.**
- `Provider` is `#[non_exhaustive]` and already has `WebGpu`, documented as "ONNX Runtime's native WebGPU provider
  (Dawn)". It keeps its meaning, but the doc comment should say it is the plugin EP.
- A later `OpenVino` or `MiGraphX` variant can be added without breaking callers, but needs a serde name and a manifest
  change, so it is out of scope here.
- `Runtime::load` already "tries `options.providers` in order, falls back to the CPU, [and] reports which one it got";
  `Skipped` carries the reason in words. Both fit.

**`omuzaki-ai-ort`: the real work.**
1. **Registration.** Today `graph.rs::dispatch` builds `ep::WebGPU::default()` and calls `is_available()`, which asks
   the loaded core library about an in-core WebGPU EP that 1.28.3 does not have, so it always says "not compiled with
   it". The plugin path is different. Per the ONNX Runtime docs: register a library once per process
   (`RegisterExecutionProviderLibrary`), list devices (`GetEpDevices`), pass one to
   `SessionOptionsAppendExecutionProvider_V2`; "Must only unregister a library after all sessions that use the library
   have been released."
2. **`ort` already exposes this** (**verified** on docs.rs, rc.13): `Environment::register_ep_library(name, path)`
   (features `api-22` and `std`; the workspace has `api-27`), returning an `ExecutionProviderLibrary` with `unregister`;
   `Environment::devices()`; `SessionBuilder::with_devices(devices, options)` and `with_auto_device(policy)` (`api-22`).
   Options "must be prefixed with the EP name and a dot". So no `ort` upgrade is needed. Not confirmed: that
   `with_devices` accepts the plugin's device on a 1.28.3 core (needs a run).
3. **Process-wide state.** Registration lives next to the existing once-per-process environment. The library handle
   must outlive every session; unregistering belongs in an orderly shutdown path, not in `Drop` of one session.
4. **Build table.** `Build.providers` assumes the provider is inside the core archive, and `Build::has` answers from it.
   Rank 1 needs a second table of plugin pins: URL (PyPI file URL or NuGet, as an ordered list), SHA-256, size, the three
   members to extract, and a rule that they land in one folder. `Archive::Zip` works for a wheel (a wheel is a zip), but
   the `lib/` layout assumption in `Build::lib_dir` does not. `has(Provider::WebGpu)` becomes "this OS and arch has a
   plugin pin". This is a small change in `omuzaki-ai-ort/src/builds.rs` and the shared loader (`omuzaki-ai-native`).
5. **Messages.** The skip reasons must separate "no plugin pin for this platform", "library failed to register
   (incompatible core)" and "no WebGPU device (driver)". The README names the last two as its two failure modes.
6. **Adapter choice.** The plugin picks the GPU itself. If `R`-class laptops with a dGPU pick the wrong one, there is
   no ORT-level fix; whether Dawn honours an option or an environment variable is **unverified** (F6).
7. **Contract gaps for GPU providers.** (a) `run` "allocates nothing after warm-up" (ADR 0061, and a test in
   `omuzaki-ai-ort/tests/allocations.rs`): a GPU EP allocates staging buffers and compiles shaders on first use, so the
   guarantee has to be stated per provider or tested after a warm run inside `load`. (b) "Runtimes ... never spin-wait":
   the idle-CPU test (under 1 ms of CPU per second) must be run with a WebGPU session loaded; Dawn's idle behaviour is
   **unverified**. (c) ADR 0009 tier 2: GPU results differ across vendors, so parity tests against the CPU need a
   tolerance and the CPU stays the reference. Part (a) is an ADR-level decision, not a task.
8. **Docs to correct.** `docs/design/ai-api.md` section 9 still says "the `DirectML` build on Windows" and "the WebGPU
   build where none of these exist"; the second is no longer how WebGPU ships, and the first has no pin.

**If rank 2 is ever chosen instead:** drop `ort` to `api-24` in `Cargo.toml`, pin NuGet 1.24.4 plus `Microsoft.AI.DirectML`
1.15.4 (`DirectML.dll` next to `onnxruntime.dll`), and keep `ep::DirectML` in `dispatch` as it is. No new code, but
section 3.3's cost.

## 6. Tier `R` measurement plan (task spec)

Cloud sessions measure nothing on `R`. The plan has two parts, in the order the cloud lane's rule (CI builds, the owner
runs one command) needs.

### 6.1 What CI builds

One Windows x64 artifact, `r-gpu-probe-<run>.zip`, built by a workflow that does not run on the owner's PC. It holds:

- **The probe**: the `track_roi` timing probe from #1731, or a sibling bench in `crates/omuzaki-test/benches/` with the same output
  shape, extended with `--provider cpu|webgpu` (and later `directml`, `openvino`). It loads the three face graphs
  (the ONNX conversions that ADR 0061 keeps) through the real `Runtime` seam, so the numbers include our wrapper.
- **The pinned ONNX Runtime**: the same `onnxruntime-win-x64-1.28.3.zip` the app pins, plus the WebGPU plugin wheel
  `onnxruntime_ep_webgpu-0.4.0-py3-none-win_amd64.whl`, each checked against a SHA-256 in the workflow. The three DLLs
  are extracted next to each other.
- **A Python A/B script** (`python -I`, run from a bundled `python-3.x-embed-amd64.zip`) that times the same models with
  stock `onnxruntime` 1.28.3 + the plugin, and with `onnxruntime-directml` 1.24.4. It exists because the Rust probe is
  built at `api-27` and cannot load a 1.24.4 library, and it keeps DirectML out of the workspace entirely.
  The embeddable Python holds neither ONNX Runtime nor NumPy, and the C archive has no Python extension, so the bundle
  also carries ABI-matched wheels (cp3x, win_amd64, each with a SHA-256): `onnxruntime` 1.28.3, `onnxruntime-directml`
  1.24.4 and `numpy`. The stock and DirectML wheels both install a module called `onnxruntime`, so they are unpacked
  into two folders (`py-stock/`, `py-directml/`) and the script runs once per folder as a separate `python -I`
  subprocess, with only that folder on `sys.path`. They are never imported together.
- **Models**: the three face graphs from the verified bundle, and one ONNX-native non-face model (`vad@1`, the smallest),
  as fixtures the probe already knows how to fetch through `FileSource` (no private clips; the probe uses the kit's
  synthetic faces).
- **A CI smoke job on a hosted Windows runner** (no AMD/Intel GPU there; **unverified** whether Dawn exposes the WARP
  software adapter): registers the plugin, lists devices, runs one inference and compares with the CPU. If it works it
  proves the plumbing and parity before the owner spends time; if not, it fails with the skip reason, which is also
  worth knowing.

### 6.2 The one command the owner runs on `R`

`powershell -ExecutionPolicy Bypass -File .\run-r-probe.ps1` from the unzipped folder. It runs everything below, writes
`r-gpu-probe-result.json` and a short `.txt`, and prints no CPU name, machine name or user name. The adapter is recorded
only as a vendor label (AMD, Intel, NVIDIA) plus the driver version string. The lead files it as a `needs-owner` issue
with the artifact name and the command.

### 6.3 What the probe measures

For each of {CPU 1 thread, CPU 4 threads, WebGPU plugin, DirectML 1.24.4 (Python only)} x {landmarks, detector,
blendshapes, `vad`}:

| Metric | How | Why |
| --- | --- | --- |
| Cold load | `load` wall time, including plugin registration and first shader compile | registry budget; first-run stall |
| First `run` | wall time of run 1 after load | shader compilation can land here |
| Steady latency | paced at 30 fps, at least 20 s warm-up, then at least 3,000 timed runs: mean, p50, p95, p99. Deadline-based pacing, so a late run does not shift the next | the tier-R method (spec v3 section 10): a tight loop keeps the GPU at high clocks and reads faster than a stream will. Per-graph numbers are diagnostics |
| CPU per frame | process CPU time (Windows `GetProcessTimes`) over the 3,000 paced runs, divided by 3,000 | `cpu.total` counts every cycle (spec v3 section 10) |
| Idle CPU | load the session, then 10 s of no `run` calls; CPU ms per second | the "never spin-wait" rule: must be under 1 ms/s |
| Placement | session log or `with_disable_cpu_fallback`: nodes on GPU against CPU | partial coverage means mixed execution |
| Parity | max absolute difference of each output against the CPU output on the same inputs, judged against the per-output tolerances in section 6.4 | ADR 0009 rule 2: a cross-platform numeric test states its tolerance |
| Memory | as spec v3 section 10 defines `ram.runtime`: the maximum over the run of the summed private working set (`PrivateWorkingSetSize`) of the process and its children. Also reported: working set, commit, an empty-device baseline taken before load, and `gpu.mem` (dedicated and shared, from the GPU counters; no budget yet) | shared iGPU allocations do not show in a process working set, so `gpu.mem` is needed to see the plugin's real cost |
| Missing-file ablation | WebGPU with `dxcompiler.dll` removed | tells us whether DXC is required or only helps |

### 6.4 Pass/fail rules (proposed; the Director owns the thresholds)

The per-graph numbers in 6.3 are diagnostics. They cannot decide the default, because `track_roi` leaves out the
detector, the frame hand-off and the rest of `track.face` (the bench says so itself). The decision uses the complete
tracker path (camera frame in, channels out) in the budget scene of spec v3 section 10, once with the CPU provider and
once with the candidate, both paced at 30 fps with the same warm-up and frame count. F3 therefore extends the probe
to drive `FaceTracker` end to end, not only single graphs.

**Parity tolerances (proposed, fixed here before the probe is built; a change needs a commit before the run).** FP32
graphs on a GPU differ from the CPU in the last bits, so ADR 0009's `1e-5` channel default does not apply. Proposed
per output, as max absolute difference against the CPU output on the same inputs:

| Output | Tolerance |
| --- | --- |
| Landmarks (normalised image coordinates) | 2e-3 (about half a pixel on a 256 px crop) |
| Blendshape scores (0 to 1) | 1e-2 |
| Detector box coordinates (normalised) | 2e-3 |
| Detector scores after sigmoid | 1e-2 |
| `vad` speech probability | 1e-2 |
| Final published tracker channels (full path) | 2e-2 |

**Rules.** A GPU provider becomes the Windows default for a slot only if, on `R`:
1. every node runs on it (no CPU fallback) for that model;
2. the complete tracker path's p95 is at most the CPU path's p95 and at most the 5 ms `track.face` budget;
3. CPU per frame is at least 25% lower than the CPU path, **and** `cpu.total` of the full budget scene, both
   configurations (phone and webcam), passes its absolute gate (mean at most 3% of 8 cores). The relative drop alone
   does not pass: 8% to 6% would still be twice the budget;
4. idle CPU is under 1 ms per second;
5. every output stays inside the tolerance table above;
6. first-run latency is hidden by a warm-up inside `load`;
7. `ram.runtime` of the budget scene with the provider on stays at or under 400 MB on each hardware graphics backend, measured as in section 6.3.

If rank 1 fails 1 to 7 for the face graphs but passes for a larger model such as `tts@1`, GPU is enabled per slot, not
globally (manifests already list providers per model). If it fails everywhere and DirectML 1.24.4 passes, the question
for the Director is whether the api-24 cost in section 3.3 is worth it. If both fail, CPU is the answer and #959 closes
with the numbers.

## 7. Open questions and what settles them

| Question | What settles it |
| --- | --- |
| Does plugin 0.4.0 register against core 1.28.3, and do contrib-op schemas match? | The CI smoke job and the Python A/B script (section 6.1) |
| Does the WebGPU plugin help any model at tier `R`? | Section 6.3 on `R` |
| Does it work on Intel GPUs (the "subgroup size 32" pin)? | No Intel GPU is available in CI or at `R`. Needs an Intel owner machine or a volunteer run of the same bundle: new `needs-owner` item |
| Is `dxcompiler.dll` required? | The ablation row |
| Is the Windows ML redistribution grant acceptable (indemnity, end-user terms)? | Director and owner; only matters if rank 3 is wanted |
| Which `ort` features above API 24 does the workspace use? | A grep for APIs gated at `api-25`..`api-27` (F7); needed only for rank 2 |
| Can Dawn be steered to the iGPU or dGPU? | Read the plugin's option list for the pinned version; test on a two-GPU laptop |
| Why did Microsoft stop DirectML packages after 1.24.4, and will they return? | Ask upstream on microsoft/onnxruntime; no answer found. Not blocking |
| Does LiteRT's prebuilt WebGPU accelerator carry a complete notices file? | Download and open the DLL's neighbours; only if option 5 is revived |
| ONNX Runtime version bump (1.31.0 is out, WebGPU plugin 0.5.0 matches it)? | A separate decision: it moves `omuzaki-audio` too. Rank 1 works at 1.28.3 without it |

## 8. Follow-up tasks

Sizes are changed lines, models per the haiku-first rule. Each acceptance includes `timeout 3600 cargo xtask check --affected`.

- **F1. WebGPU plugin loader in `omuzaki-ai-ort`** (sonnet, about 250 lines). Fence: `crates/omuzaki-ai-ort/src/{graph,lib,library}.rs`
  and tests. Contract: none changes. Acceptance: registers the plugin once per process; selects its device with
  `with_devices`; unregisters after sessions drop; the three skip reasons from section 5 item 5; unit tests with a fake
  library path; a gated test with `OMUZAKI_ORT_CACHE` that runs a synthetic `Add` graph on WebGPU and compares with CPU.
- **F2. Plugin pin and licence rows** (haiku, about 120 lines, after F1's table shape). Fence: `omuzaki-ai-ort/src/builds.rs`,
  `docs/licensing/deps/*.toml`. `docs/licensing/register.md` is generated from those files (`cargo xtask lint-licences
  --print`); do not edit it by hand. Acceptance: the wheel's URL, SHA-256 and size; rows for the
  plugin (MIT), Dawn (BSD-3-Clause), the DXC/LLVM release licence (NCSA), abseil and protobuf, each `conditions: ship
  the notice` and each checked against the wheel's `ThirdPartyNotices.txt`.
- **F3. `R` probe bundle** (sonnet, about 300 lines). Extends #1731 and #1734. Fence: the existing app test crate,
  `crates/omuzaki-test/benches/track.rs` (or a sibling bench there), which already depends on both `omuzaki-track` and
  `omuzaki-ai-ort`, so the real `FaceTracker` runs on the real `OrtRuntime` with no new implementation-to-implementation
  dependency; plus `tools/r-probe/**` and one workflow job. Acceptance: section 6.1 to 6.4, with every metric
  printed and no machine name.
- **F4. Hosted-runner smoke job** (haiku, about 80 lines). Fence: one job in the probe workflow. Acceptance: the plugin
  registers on `windows-latest`, or the job records the exact skip reason.
- **F5. `docs/design/ai-api.md` section 9 correction** (haiku, about 20 lines). Replace "the `DirectML` build on Windows"
  and the WebGPU line with this note's result; link this note.
- **F6. Adapter selection research** (sonnet, research). Read the pinned plugin's configuration options and Dawn's
  adapter-selection behaviour; answer in `docs/research`.
- **F7. `ort` API-24 audit** (haiku, research). List every use of an API above level 24 in the workspace. Only needed if
  rank 2 is chosen.
- **F8. Decision record: GPU providers and the allocation-free contract** (Director, ADR). Section 5 item 7(a)-(c).
- **F9. Owner item for the Intel run** (lead files it): run the same bundle on an Intel GPU machine.

## 9. Sources and retrieval dates

All retrieved 2026-10-15.

| What | URL |
| --- | --- |
| ONNX Runtime release v1.28.3 assets | `https://api.github.com/repos/microsoft/onnxruntime/releases/tags/v1.28.3` |
| ONNX Runtime release list, v1.31.0 and v1.25.0 notes | `https://github.com/microsoft/onnxruntime/releases` |
| WebGPU plugin EP v0.4.0 release | `https://github.com/microsoft/onnxruntime/releases/tag/plugin-ep-webgpu%2Fv0.4.0` |
| WebGPU plugin EP wheel (opened: README, LICENSE, ThirdPartyNotices) | `https://pypi.org/project/onnxruntime-ep-webgpu/0.4.0/` |
| WebGPU plugin EP NuGet | `https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.ep.webgpu/index.json` |
| ONNX Runtime WebGPU EP docs | `https://onnxruntime.ai/docs/execution-providers/WebGPU-ExecutionProvider.html` |
| Plugin EP usage docs | `https://onnxruntime.ai/docs/execution-providers/plugin-ep-libraries/usage.html` |
| ONNX Runtime DirectML EP docs | `https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html` |
| DirectML repository | `https://github.com/microsoft/DirectML` |
| DirectML ONNX Runtime NuGet and PyPI listings | `https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/index.json`, `https://pypi.org/pypi/onnxruntime-directml/json` |
| `Microsoft.AI.DirectML` 1.15.4 (opened: LICENSE.txt) | `https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg` |
| Windows ML overview, EPs, deployment, WebGPU EP, EP licence, ORT versions | `https://learn.microsoft.com/en-us/windows/ai/new-windows-ml/{overview,supported-execution-providers,distributing-your-app,webgpu-ep,webgpu-ep-license,onnx-versions}` |
| Windows ML package (opened: license.txt, WinMLEpCatalog.h, file sizes) | `https://api.nuget.org/v3-flatcontainer/microsoft.windows.ai.machinelearning/2.4.89/microsoft.windows.ai.machinelearning.2.4.89.nupkg` |
| Windows EP components list | `https://support.microsoft.com/en-us/servicing/os/windows/ai-components/2026/01/windows-execution-provider-components` |
| OpenVINO EP docs; plugin wheel (opened) | `https://onnxruntime.ai/docs/execution-providers/OpenVINO-ExecutionProvider.html`, `https://pypi.org/project/onnxruntime-ep-openvino/1.7.0/` |
| `ort` 2.0.0-rc.13 docs | `https://docs.rs/ort/2.0.0-rc.13/ort/` (modules `ep`, `environment`, `session::builder`; features page) |
| `ort` licence | `https://raw.githubusercontent.com/pykeio/ort/main/LICENSE-MIT` (MIT) |
| LiteRT GPU docs, C++ SDK, releases, licence | `https://developers.google.com/edge/litert/next/gpu`, `.../cpp_sdk`, `https://github.com/google-ai-edge/LiteRT/releases`, `.../blob/main/LICENSE` |
| LiteRT prebuilt Windows binaries | `https://storage.googleapis.com/litert/binaries/2.3.0/windows_x86_64/` |
| Burn ONNX 0.21.0; Burn licence | `https://tracel.ai/blog/release-burn-onnx-0.21.0`, `https://github.com/tracel-ai/burn/blob/main/LICENSE-MIT` |
| wonnx; ncnn; MNN | `https://github.com/webonnx/wonnx`, `https://github.com/Tencent/ncnn`, `https://github.com/alibaba/MNN` |

Repo files read: ADR 0061, ADR 0010, `docs/design/ai-api.md` section 9, `docs/research/2026-10-reference-machine-definition.md`,
`crates/omuzaki-ai-api/src/{session,runtime}.rs`, `crates/omuzaki-ai-ort/src/{builds,graph}.rs`, workspace `Cargo.toml`.
