# Spike: transparent output to OBS, a Windows virtual camera, and NDI

Can a wgpu frame with premultiplied alpha reach OBS and other apps cleanly on Windows? This spike builds the Spout2 path and
a Media Foundation virtual camera source, measures them with receivers written for the purpose, and writes down what NDI would
take. It feeds BR-01, BR-02 and the `omuzaki-stream-api` contract.

Its own workspace, like `spikes/toon-viewport`; nothing here touches the main workspace.

## Verdict

| | Works? | What was shown | What was not |
|---|---|---|---|
| **Spout2 to OBS** | Yes | A DX12 wgpu frame reaches a second process through Spout's shared texture with no CPU copy, bit for bit (hash of sampled frames), 0 torn frames in every run when the sender waits for the GPU (about 2.5 ms from render start to readable). An independent receiver written from the protocol, as OBS's source works, opens and reads it. | OBS itself is not installed here, so OBS was never run. Zoom or other receivers were not run. |
| **Windows virtual camera** | Yes, in-process | A Frame Server style media source (COM DLL) serves NV12 and YUY2 at 1280x720 30 fps to Media Foundation's Source Reader with no registration, correct colours (RGB32 within 1 level of 255) and no leaked objects. `MFCreateVirtualCamera` accepts the request. | `IMFVirtualCamera::Start` was never called: it needs the DLL registered machine-wide (administrator), which is a manual step for the user. Not tested in Zoom, Discord or Meet. |
| **NDI** | Feasible | Terms read (EULA text and licensing pages), wrapper options compared, alpha convention found (NDI wants straight alpha, not premultiplied). | No SDK or runtime installed; nothing sent. |

Three findings change the design:

1. **Tell receivers only after the GPU has finished the copy.** Without a fence wait, 0 to 13% of frames reached an
   event-driven receiver torn (varies with machine load), and 60 to 85% on the CPU path. With the wait: 0 in every run.
2. **A receiver that reads on its own clock (OBS) can still catch a frame mid-copy.** The wait shrinks the exposed window from
   about 1.2 ms to about 15 to 30 microseconds per frame; it does not remove it. That is roughly 0.1% of reads when phases are
   random, and several percent when sender and reader are phase-locked at the same rate.
3. **Alpha encoding is not one thing across outputs.** Spout carries no alpha-mode flag and OBS's Spout2 source ignores alpha
   until the user picks a composite mode; NDI wants straight alpha; a virtual camera has no alpha at all. The render-api note
   that `Premultiplied` is the encoding "for OBS/Spout/NDI" is wrong for NDI and incomplete for the others (see "Alpha").

## What is here

```
spikes/broadcast-out/
  spout-out/            Spout2 sender, two receivers, loopback runner, benchmark (bin: spout-out)
    csrc/               C shim over the Spout2 SDK's spoutDX class, plus two mingw compatibility headers
    src/scene.wgsl      the test picture (premultiplied, soft edges, tag rows, stamp column, known-value patches)
    src/interop.rs      wgpu (D3D12) -> D3D11On12 -> Spout
    src/receiver.rs     receiver on the SDK's receiver class (waits on the sync event)
    src/raw_receiver.rs receiver written from the protocol, reads on its own clock (what OBS's source does)
    tests/              GPU scene tests, and the whole path in two processes
  vcam-source/          Media Foundation virtual camera source (cdylib omuzaki_vcam_source.dll) and vcam-probe
  tools/fetch-spout2.sh clones the pinned Spout2 SDK source (not vendored)
  tools/bench-spout.sh  the benchmark matrix quoted below
```

Build and run (from WSL, as `toon-viewport` does; Windows binaries are cross-built and started through interop):

```
cd spikes/broadcast-out
tools/fetch-spout2.sh                                   # once: Spout2 6b68a5b (2.007.018), BSD-2-Clause
cargo build --release --workspace --target x86_64-pc-windows-gnu
../../target/x86_64-pc-windows-gnu/release/spout-out.exe loopback --frames 600     # sender here, receiver in a child process
../../target/x86_64-pc-windows-gnu/release/spout-out.exe render --out /some/windows/path/scene   # three PNGs, any backend
tools/bench-spout.sh > results.txt
../../target/x86_64-pc-windows-gnu/release/vcam-probe.exe selftest     # DLL loaded by path, read through the Source Reader
../../target/x86_64-pc-windows-gnu/release/vcam-probe.exe env          # read-only machine facts
```

Tests: `cargo test --workspace` (31 tests: unit tests and 5 GPU scene tests, native on Linux) and, after
`cargo build --workspace --target x86_64-pc-windows-gnu`, `cargo test --workspace --target x86_64-pc-windows-gnu`
(37 tests including the two-process loopbacks and the Source Reader self-test; the Windows test exes start through
interop). `cargo clippy --all-targets --workspace -- -D warnings` is clean on both targets.

## 1. Spout2

### The path

```
wgpu (DX12) renders BGRA8, premultiplied          scene texture, RENDER_ATTACHMENT
        |  same D3D12 queue, so submission order is execution order
D3D11On12 wraps the texture as an ID3D11Texture2D (in and out state: RENDER_TARGET, so wgpu never sees a state change)
        |  Spout's SendTexture: CopyResource into its own legacy-shared texture, Flush
        |  fence wait on the queue (the step that prevents torn frames)
Spout sync event signalled  ->  receiver process opens the shared texture and reads
```

The Spout2 SDK is the real thing, unmodified (BSD-2-Clause, 2.007.018), compiled with the cross mingw compiler plus two shims:
`timeapi.h` is missing from mingw-w64 10, and the win32 thread model has no `std::this_thread` (used once, in `HoldFps`).
`-mssse3` for Spout's pixel routines; the C++ runtime is linked statically, so the exe imports only system DLLs. Reproducing
the sender's behaviour in pure Rust was not needed.

### Results

Single sender and receiver on an RTX 4090, 1080p unless noted. The machine was shared with other jobs the whole time (load
average 10 to 39, GPU in use by other agents), so read the ranges, not single figures. "Receiver notices" is the time from the
sender starting a frame to the receiver learning it is readable (the number that matters for a consumer like OBS that uses the
texture on the GPU). "Pixels in RAM" adds this receiver's own readback.

Event-driven receiver (the SDK's receiver class), 60 fps, 1000 frames, runs of the same configuration:

| path | sender waits for GPU? | sender hand-off CPU, mean | receiver notices, mean / p95 | torn frames per 1000 | frames missed |
|---|---|---|---|---|---|
| GPU texture | no | 0.2 to 0.3 ms | 1.6 to 5.8 / 1.0 to 17.8 ms (stale reads) | 0 to 129 (runs: 8, 129, 114, 73, 0) | 40 to 153 |
| GPU texture | **yes** | 1.3 to 1.8 ms | **2.0 to 2.7 / 3.4 to 4.6 ms** | **0 (every run)** | 0 to 19 |
| CPU pixels (readback, `SendImage`) | no | 6.9 to 10.4 ms | 7.5 to 11.1 ms | 599 to 853 | 0 to 11 |
| CPU pixels | yes | 7.7 to 11.2 ms | 8.4 to 12.1 ms | 0 | 0 |
| GPU, 4K, waits | yes | 0.7 to 1.0 ms | 1.6 to 1.8 / 2.7 to 4.4 ms | 0 | 13 to 17 of 600 |

Free-running sender with the wait (1080p, 3000 frames): hand-off 0.3 ms, receiver notices in 0.45 to 0.6 ms; the receiver,
bound by its own readback, sees about 230 to 300 frames per second of the thousand-plus sent. "Frames missed" are mostly the
receiver being descheduled on a busy machine; with the wait they are 0 in most runs. About 100 sampled frames across the runs
that waited were compared byte for byte with the sender's own readback: no mismatch.

Receiver that reads on its own clock with no sync and no mutex (how OBS's Spout2 source reads), 60 fps sender, 1200 frames:

| sender waits? | reads per second | reads that were torn |
|---|---|---|
| no | 1000 | 7.4% |
| yes | 1000 | 0.16% |
| yes (CPU path) | 1000 | 0.084% |
| yes | 60 (phase-locked with the sender in that run) | 3.9%, tear lines near the top of the frame; 0 in other phases |

A tear line is located to the row by the stamp column (see `frame_tag.rs`); in these runs it is a single horizontal line where
frame N meets frame N+1.

### What this means

- **Wait for the GPU before announcing a frame.** The shared texture is written by a GPU copy that can run at any time after the
  CPU hands it over; an event-driven receiver that wakes immediately reads the previous frame or half of each. D3D11On12's
  Flush does not give the implicit cross-process ordering a plain D3D11 sender gets. Cost: about 1.2 ms of waiting per frame,
  so it belongs on an output thread, not the render thread (see the API shape below).
- **A residual tear risk remains for OBS.** OBS's source takes no mutex and no event, so the sender cannot prevent a read during
  the copy. The copy of a 1080p frame takes about 15 to 30 microseconds; at random phase that is a torn frame every 15 to 30
  seconds at 60 fps, lasting one frame; phase-locked it can be much worse. Mitigations to test with a real OBS: make the copy
  as short as it is now, avoid locking the sender's cadence to the display refresh, and offer an NDI or window-capture route.
- **Spout's frame counter is off on a clean machine.** The SDK enables it from a per-user registry value (`HKCU\Software\Leading
  Edge\Spout\Framecount`, written by its settings app); without it `IsFrameNew()` is always true and the sender's frame
  number reads 0. This spike therefore tells frames apart by a number drawn into them and paces on the sync event, which the
  SDK lets a program enable in memory. It never writes that registry value. OBS's source uses neither.
- **Format and handle.** The sender registers a 32-bit legacy shared handle (for example `0x40002C82`), 1920x1080, DXGI format 87
  (`B8G8R8A8_UNORM`), in Spout's shared memory; the OBS source names that format in its log and opens exactly that kind of
  handle (`gs_texture_open_shared` on the 32-bit value). This was read back independently with PowerShell from the `SpoutSenderNames`, `ActiveSenderName` and per-sender mappings, and
  by the raw receiver.
- **Receivers that want other formats** (RGBA8, 10-bit, 16-bit float) exist; BGRA8 matches the renderer's target and avoids a
  conversion pass.

### Alpha, and what OBS does with it

The Spout protocol has no alpha-mode field, so what a frame means depends on a setting in the receiving app. Reading OBS's
Spout2 plugin source (GPL; read for behaviour only, nothing copied; release 1.12.0): the Spout2 Capture source has a
"Composite mode" with four values: **Opaque**, **Converted Premultiplied Alpha (legacy)**, **Default** (source-alpha blend, so
straight alpha) and **Premultiplied Alpha** (`ONE, INV_SRC_ALPHA`). No default is stored, and an unset mode falls through to
Opaque: **a premultiplied Spout frame shows an opaque background in OBS until the user picks a mode.**

Measured against a ground-truth render of the same picture over a known background (8-bit levels, 1080p scene):

| frame sent | receiver blends as | max error | pixels over 8 levels |
|---|---|---|---|
| premultiplied | Premultiplied Alpha | 1.1 | 0 |
| premultiplied | Converted Premultiplied (legacy) | 1.1 | 0 |
| premultiplied | Default (straight) | 64.7 (dark halo) | about 98,000 |
| straight | Default (straight) | 1.4 | 0 |
| straight | Premultiplied Alpha | 215 (bright fringe) | about 107,000 |

So: send premultiplied (what the renderer already has, nothing to convert) and have the user, or a scene template we provide,
pick "Premultiplied Alpha". The product BR-03 plugin decision would remove the manual step. Converting premultiplied to
straight in 8 bits afterwards costs at most 1.4 levels in this scene, but when a straight output is needed (NDI) do it on the
GPU in float before quantising.

### Licence and dependencies

Spout2: BSD-2-Clause, already in the register (keep the notice). New to the register: `windows`
(windows-rs, MIT OR Apache-2.0) and `cc` (MIT OR Apache-2.0); the rest are the toon spike's set (`wgpu`, `clap`, `image`,
`bytemuck`, `pollster`, `serde`, `serde_json`, `thiserror`). The spike's own workspace keeps them out of the
main lockfile. `cargo deny check bans sources licenses` passes on it.

## 2. Windows virtual camera (Media Foundation)

### What was built and shown

`vcam-source` is a Frame Server style custom media source, as Microsoft's "Frame Server custom media source" page specifies:
an in-process COM server (`DllGetClassObject`, `DllCanUnloadNow`, a class factory) whose source implements `IMFMediaSourceEx`,
`IMFGetService` (no services) and `IKsControl` (every control answers "set not found"), whose stream implements
`IMFMediaStream2`, with event queues from `MFCreateEventQueue`, `MENewStream` posted at `Start`, live characteristics and no
seek or pause. It offers one stream, 1280x720 at 30 fps, NV12 (current) and YUY2, both tagged BT.709 limited range. A worker
thread paces frames against the system-time clock and draws the next frame right after delivering one.

`vcam-probe selftest` does what a test host does (Microsoft's own sample tests load the source in-process the same way): it
loads the DLL by path with no COM registration, makes the source from its class factory, and reads it with the Source Reader.
Results on this machine (also asserted by `tests/selftest.rs`):

- the source implements the three required interfaces, refuses an unknown CLSID, reports itself live
- NV12: 150 frames in 4.98 s (30.1 fps), no wrong-sized buffer, no frame-number gap, sample timestamp to `ReadSample` return
  1.1 ms on average; YUY2 the same
- the Source Reader's video processor converts NV12 to RGB32 using the declared matrix and range: worst bar colour error 1 level
- after everything is released `DllCanUnloadNow` is `S_OK` (no leaked objects; the source and stream do not keep each other alive)
- `MFCreateVirtualCamera(SoftwareCameraSource, Session, CurrentUser, ...)` returns an object for an ordinary user, without
  starting or registering anything
- `vcam-probe env` (read-only): Windows 11 build 26200 (needs 22000 or later), Frame Server running (the Monitor service starts
  on demand), Smart App Control off, camera privacy Allow, the spike's CLSID not registered anywhere

### What installing it takes (a manual step)

Nothing was installed, registered or started on this machine. `vcam-probe print-registration` prints the exact commands for a
DLL path; `vcam-probe start-camera --confirm-registered` refuses to run without that flag. What is needed:

| step | who | needs administrator? | why |
|---|---|---|---|
| copy `omuzaki_vcam_source.dll` somewhere Frame Server can read, for example `C:\Program Files\Omuzaki\` | installer | yes (Program Files) | Microsoft: the source runs as Local Service in session 0. A community sample adds that the Frame Server Monitor service (Local System) loads it too, and that a folder under a user profile, a network share or the WSL file system fails with access denied |
| register the COM class: `HKLM\Software\Classes\CLSID\{...}\InprocServer32` (default value = DLL path, `ThreadingModel` = `Both`) | installer | yes (HKLM) | Microsoft's own Virtual Camera sample registers it from an MSI under HKLM; a community sample states that a per-user HKCU registration does not work because several processes load the source |
| `MFCreateVirtualCamera(SoftwareCameraSource, Session or System, CurrentUser, name, "{CLSID}")` then `Start` | the app, as a normal user | no for `CurrentUser` | `AllUsers` access requires an administrator (docs); a non-administrator may only use the Video, Capture, Camera and Sensor-Camera categories (docs) |
| camera privacy must allow the app | the user | no | the API returns `E_ACCESSDENIED` when the webcam privacy control denies it (docs) |

Lifetime: `Session` removes the camera when the creating object is disposed; surviving a reboot needs `System` (docs). The docs
do not say whether `System` needs elevation; Microsoft's sample uses it from an unelevated app after an MSI install. Untested.

Windows version: build 22000 or later (Windows 11). Windows 10 users get no virtual camera from this route.

### Open risks

- **Signing: unverified.** Microsoft documents no signing requirement for the `MFCreateVirtualCamera` route (the older stub
  driver route needs a WHQL-certified driver package; this one does not). But an unsigned DLL loaded into a service host can
  be blocked by Smart App Control in enforcement mode, Windows Defender Application Control, or AV; it is off on this machine,
  so this was not exercised. Plan to Authenticode-sign the DLL regardless (the Rust-stack research notes individuals can use
  Azure Artifact Signing only in the US or Canada). Needs a test on a machine with Smart App Control enforced.
- **Frames from the app to the source.** The source runs inside Frame Server, as Local Service in session 0, in another process
  from the app. A file mapping in the `Local\` namespace is invisible to it, and creating one in `Global\` from a user session
  needs `SeCreateGlobalPrivilege` (documented for `CreateFileMapping`; opening an existing one does not). Options: the source
  creates the `Global\` mapping itself (it runs in session 0) and the app opens it, which also tells the app when a consumer has
  the camera open; or the app serves a named pipe the source connects to. Not built; this spike's source draws its own picture.
- **No alpha.** NV12, YUY2 and RGB32 carry none. The app must flatten the frame over a background (or a key colour for chroma
  keying) before this stage.
- **Which apps see it.** Docs: a camera registered under the Video and Capture categories is enumerable by legacy DirectShow
  apps, and YUY2 should be offered because such apps assume it; `MFCreateVirtualCamera`'s default categories include both.
  Zoom, Discord, Meet and Teams were not run. A community sample notes the camera previews in Teams but did not reach the
  other party.
- **Tracing.** The source runs in several processes at once (the app, Frame Server, Frame Server Monitor, each consumer), which
  makes it hard to debug; ETW tracing is the usual answer.

## 3. NDI feasibility

**Feasible; licence logistics are the cost.** No SDK or runtime was downloaded or installed.

**Terms** (read from the NDI licensing and software-distribution pages, and from the NDI SDK 6.1.1 licence agreement text that
the `ndi-sdk` crate vendors; the pages say 6.3.2 updated the terms, and that agreement is only inside the SDK download, so
legal review must read the current one):

- royalty-free, but proprietary; accepting it is part of downloading the SDK
- a link to ndi.video wherever NDI is selected in the product, on our website and in documentation; the line "NDI is a registered
  trademark of Vizrt NDI AB" in the About box and near first use; ask NDI before putting NDI in a product name
- our end-user licence must flow down: no modification or reverse engineering of the SDK, warranty and liability disclaimers,
  NDI copyright notice
- the SDK DLLs go in our own application folder, never the system path; only the files the SDK lists as distributable may ship;
  the headers are MIT only for open-source projects
- a product release must use an SDK less than 30 days old when one exists (already in the register)
- the SDK is confidential information: do not commit it
- "Products" excludes embedded and appliance devices and lists Linux and others as embedded operating systems; for a desktop app
  this should not apply, and legal review should say so for a Linux desktop build

**Technical.** Send-only needs about six calls. Video frames are CPU memory (BGRA, RGBA, UYVY, UYVA, P216, PA16, ...); there
is no GPU texture path, so NDI goes through the CPU lane. NDI's BGRA and RGBA are documented as **not pre-multiplied**, so the
frame must be un-premultiplied (on the GPU, in float) first. Latency was not measured (no SDK).

**Rust options** (crates.io, checked 2026-10-12):

| crate | licence | last release | how it links | verdict |
|---|---|---|---|---|
| `grafton-ndi` 1.0.0 | Apache-2.0 | 2026-06-15 | bindgen at build time from the SDK (needs the SDK and clang on every build machine) | complete, but it puts the SDK in our build and CI |
| `ndi-sdk` 0.2.0 | MIT | 2025-05-14 | `libloading`, vendored MIT headers | send and receive not implemented yet |
| `ndi` 0.1.2, `ndi-sys` 0.1.1 | non-standard, MIT | 2022 | build-time | stale |

**Recommendation:** a small in-house binding (about 300 lines) that loads `Processing.NDI.Lib.x64.dll` at run time through
`NDIlib_v6_load` (from the app folder, `NDI_RUNTIME_DIR_V6`, or a link asking the user to install the NDI runtime), declaring
the few structs and calls itself from the documented C ABI. That keeps the SDK out of CI and the repository, and lets NDI be an
optional output that simply reports "runtime not found". Do it after legal review clears the terms. NDI is also the cross-platform
OBS route, since Spout is Windows-only.

## 4. Alpha across outputs

| output | alpha | encoding the receiver expects |
|---|---|---|
| Spout (Windows) | yes | not declared. OBS source: user picks Opaque, Default (straight) or Premultiplied; unset means opaque. Send premultiplied |
| NDI | yes | straight ("not pre-multiplied", NDI frame-types page). Un-premultiply on the GPU first |
| Media Foundation virtual camera | no | not applicable; flatten over a background or key colour |
| Syphon, CoreMediaIO, v4l2loopback | not checked | see the follow-ups (macOS, Linux) |

## 5. Recommended `omuzaki-stream-api` shape

Fits `docs/design/render-api.md` ("live outputs share the target's GPU texture; the CPU path is `read_target`; the target's
`AlphaEncoding` is the output's"), with four corrections from the measurements.

```rust
/// One live output: a Spout sender, an NDI source, a virtual camera, a recorder.
pub trait Output: Send {
    fn info(&self) -> OutputInfo;                                    // what it needs, see below
    fn start(&mut self, config: &OutputConfig) -> Result<(), StreamError>;   // name, size, fps
    fn submit(&mut self, frame: &Frame<'_>) -> Result<SubmitReport, StreamError>;
    fn status(&self) -> OutputStatus;                                // receivers: Option<u32>, dropped, last error
    fn stop(&mut self);
}

pub struct OutputInfo { pub kind: OutputKind, pub alpha: AlphaNeed, pub lanes: Lanes }
pub enum AlphaNeed { Premultiplied, Straight, Opaque }               // the stream layer converts, outputs do not
pub struct Lanes { pub gpu: Option<GpuBackend>, pub cpu: bool }      // Spout: Dx12; NDI and v4l2: cpu only
pub struct Frame<'a> {
    pub size: UVec2, pub index: u64, pub capture_time: Ticks,         // monotonic, comparable across processes (QPC)
    pub gpu: Option<&'a mut dyn Any>,                                 // (wgpu::Device, wgpu::Queue, wgpu::Texture), the painters' convention
    pub cpu: Option<&'a Image>,                                       // from read_target, already converted to `alpha`
}
pub struct SubmitReport { pub cpu_ms: f32, pub gpu_wait_ms: f32, pub dropped: u32 }
```

1. **The render target for live outputs is BGRA8, premultiplied, `COPY_SRC | RENDER_ATTACHMENT | TEXTURE_BINDING`**, so the
   Windows path needs no conversion. This needs one addition to render-api: a way to reach a target's native texture,
   `Renderer::target_native(TargetId) -> Result<&mut dyn Any, RenderError>` with the same `(Device, Queue, Texture)` convention
   as painters. The wgpu hal access this spike uses (`as_hal`) is `unsafe` and wgpu-version-specific, so it lives
   only in the Windows Spout output of `omuzaki-stream`, covered by this spike's loopback test as a CI lane.
2. **Alpha adaptation belongs to the stream layer**: one conversion pass per distinct need, shared between outputs
   (premultiplied to straight in float for NDI, flatten over a colour for a camera, nothing for Spout).
3. **Outputs run on their own thread behind a small ring of GPU copies.** The render thread only records a
   `copy_texture_to_texture(target -> slot)` and posts (slot, submission index); the output thread waits for the GPU and does
   the hand-off. The wait measured here (about 1.2 ms at 1080p) must never be on the render thread.
4. **"Complete before announced" is a contract of every GPU output** (finding 1), enforced in the output, tested by a loopback
   with tag rows and hashes like this spike's (`spout-out loopback`), which becomes the GPU lane of `omuzaki-test` on Windows.
5. **Status the scheduler needs**: Spout cannot tell whether anyone is reading (`receivers: None`), a virtual camera can (its
   IPC connection), and both report cpu/gpu-wait time and dropped frames to the frame-budget scheduler. Outputs are
   toggled per user, since an idle Spout sender still costs a copy per frame.
6. Conventions of the repo apply: outputs self-register with a macro in one file each (`spout.rs`, `ndi.rs`, `vcam_windows.rs`,
   ...), platform-gated, with a recording mock in `omuzaki-stream-mock`. The virtual camera is two artefacts: the output in the
   app, and the installed `omuzaki_vcam_source.dll` (this spike's, plus the IPC feed).

## 6. Not verified

- OBS, Zoom, Discord, Meet, Teams: none installed or run. The OBS findings come from its source and from a receiver that reads
  the way that source does.
- `IMFVirtualCamera::Start`, and anything Frame Server does beyond what the Source Reader exercises (sensor-group enumeration,
  `IKsControl` queries, privacy prompts).
- Signing under Smart App Control; the `System` lifetime without elevation.
- Latency inside OBS (up to one OBS render tick, 16.7 ms at 60 fps, plus its compositing) and inside NDI.
- Syphon, CoreMediaIO, v4l2loopback.
- Numbers come from one machine that was busy with other jobs; the ranges say how much they moved.

## 7. Follow-ups

- macOS: Syphon and the CoreMediaIO camera extension (needs a Mac and an Apple developer account)
- Linux: v4l2loopback and transparent options
- Windows virtual camera: the register-and-start steps, the frame feed, signing
- verify in real OBS: composite mode, tearing, latency (needs OBS)
- render-api: reach a target's native texture; correct the alpha note
- licence register rows for `windows` and `cc`, and the Spout2 pin
- NDI sender through a runtime-loaded library (blocked on legal review)
- NDI agreement and signing questions; running Windows tests through interop already works today
