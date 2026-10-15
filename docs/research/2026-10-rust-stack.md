---
title: "Rust stack survey (2026-10-11)"
summary: "Rust crates for rendering, UI, assets, ML, media, MCP, plugins, collab."
area: research
---

# Rust stack survey (2026-10-11)

> Research pass for the v3 spec. Versions and licences as checked on 2026-10-11; "(?)" = unverified.

Everything below was checked on crates.io, GitHub or docs.rs on 2026-10-11 unless I mark it "(?)", which means unverified or from memory. Crate-page links are `https://crates.io/crates/<name>`.

## 1. Rendering / engine
| Option | Version, status | Licence | Verdict |
|---|---|---|---|
| wgpu ([releases](https://github.com/gfx-rs/wgpu/releases)) | 30.0.1 (Aug 2026). A new major arrives every 2–3 months. Compute, ray-tracing, and WebGPU/WASM backends are all supported. | MIT/Apache | Use it. |
| Bevy ([0.20](https://bevy.org/news/bevy-0-20/)) | 0.20.0 (8 Oct 2026), with breaking changes about quarterly. No editor has shipped. [0.19](https://bevy.org/news/bevy-0-19/) added the `bsn!` scene macro and Feathers widgets; the `.bsn` file loader is still pending. | MIT/Apache | Fine as an engine, not as an editor base. |
| Fyrox ([1.0](https://fyrox.rs/blog/post/fyrox-game-engine-1-0-0/)) | 1.0.1, stable since Mar 2026, ships its own editor. | MIT | Smaller ecosystem and no VRM; skip. |
| Custom wgpu renderer | Toon shading, outlines and MToon are plain WGSL. | – | Recommended. |

- **Toon crates for Bevy are stale.** `bevy_mod_outline` and `bevy_toon_shader` stop at Bevy 0.12–0.13. You would write the outline pass yourself either way.
- **Browser WebGPU** is shipped in Chrome, Firefox 141+ (Windows Jul 2025, Apple Silicon Nov 2025) and Safari 26 ([Wikipedia](https://en.wikipedia.org/wiki/WebGPU)). wgpu can fall back to WebGL.
- **rend3-likes:** not checked.

## 2. UI
| Option | Version | Licence | Fit |
|---|---|---|---|
| egui | 0.36.2 | MIT/Apache | Best fit for a dense editor. Docking: [egui_dock](https://crates.io/crates/egui_dock) 0.21.1, [egui_tiles](https://crates.io/crates/egui_tiles) 0.17.1. Node graph: [egui-snarl](https://crates.io/crates/egui-snarl) 0.12.0; `egui_node_graph2` has been dormant since Nov 2024. |
| Tauri | 2.12.3 stable (3.0 is still alpha) | MIT/Apache | A web UI gets mature docking, timeline and node-graph libraries. A native wgpu viewport inside a webview is awkward. |
| Slint | 1.18.1 | GPL-3 or royalty-free or commercial | Polished, but you must choose a licence. |
| Iced | 0.14.0 (Dec 2025) | MIT | Slower release cadence. |
| Dioxus | 0.7.10 | MIT/Apache | Desktop uses a system webview. The native Blitz renderer is [pre-alpha](https://cdn.jsdelivr.net/gh/dioxuslabs/blitz@main/README.md). |
| GPUI | crates.io copy is 0.2.2 (Oct 2025); [pre-1.0](https://github.com/zed-industries/zed/tree/main/crates/gpui), sparse docs | Apache | Used via git; no docking widgets. |
| Xilem | 0.4.0 | Apache | Alpha. |

## 3. Assets
| Need | Option | Notes |
|---|---|---|
| glTF | [gltf](https://crates.io/crates/gltf) 1.4.1 | Stable but last release was May 2024. |
| VRM | [bevy_vrm](https://docs.rs/crate/bevy_vrm/latest) 0.4.0 (Jul 2026, Bevy 0.19) | A Bevy-only loader with a `spring_bones` module and MToon, but only 14% documented. The [README](https://github.com/unavi-xyz/bevy_vrm) says VRM 1.0 is "in progress", which conflicts with docs.rs. `serde_vrm` 0.4.0 has the data types only. `bevy_vrm1` has VRM 1.0 spring bones but targets Bevy ≤0.16. |
| FBX | [ufbx](https://crates.io/crates/ufbx) 0.11.5 | Wraps the C ufbx library. Best option. |
| Live2D | Cubism Core is proprietary and not on GitHub ([docs](https://docs.live2d.com/en/cubism-sdk-manual/cubism-sdk-for-native/)) | The [help page](https://help.live2d.com/en/?p=2558) says no contract is needed under ¥10M annual sales. The [licence page](https://www.live2d.com/en/sdk/license/) cites ¥20M for VTuber tracking software. Avatar and VTuber apps are "Expandable Applications" needing approval and a special agreement. Rust bindings: [live2d-cubism-core-sys](https://github.com/James2022-rgb/live2d-cubism-core-sys) (MIT wrapper, Windows, Linux x64 and wasm only), and `mocari` 0.4.0 (pure-Rust runtime; legal status (?)). |
| Inochi2D | [inox2d](https://github.com/Inochi2D/inox2d) | BSD-2. The README calls it a prototype with no MeshGroup support. Use it only as an import path. |

## 4. ML inference
| Option | Version | Verdict |
|---|---|---|
| [ort](https://crates.io/crates/ort) | 2.0.0-rc.13 (Jul 2026), bundles ONNX Runtime 1.31, MIT/Apache | Primary choice. Execution providers: CUDA, TensorRT, DirectML, CoreML, and WebGPU ([docs](https://ort.pyke.io/perf/execution-providers)). WebGPU is flagged experimental. Still no stable 2.0. |
| candle | 0.11.0 | Good for custom models. |
| burn | 0.22.0 (6 Oct 2026) | Good for custom models. |
| tract | 0.23.8 | CPU-only, small. |

- **Face and hand tracking.** I found no Rust MediaPipe wrapper. An [ONNX port of Face Mesh V2](https://huggingface.co/fernandotonon/QtMeshEditor-facemesh-onnx) exists, but the 52-blendshape model is missing, so you would convert it yourself.
- **Speech.** [whisper-rs](https://crates.io/crates/whisper-rs) 0.16.0 (Unlicense). The [sherpa-onnx](https://docs.rs/sherpa-onnx) Rust crate (1.13.1) covers ASR, TTS and VAD but links GPL espeak-ng, so we don't use it: Silero VAD through `ort` plus Whisper through `whisper-rs` is the measured replacement (`spikes/speech/README.md`). Kokoro crates such as `kokoroxide` are small and young.

## 5. Media I/O
| Need | Option | Notes |
|---|---|---|
| Camera | [nokhwa](https://crates.io/crates/nokhwa) 0.10.11 | Backends are V4L2, MSMF and AVFoundation. Maintenance looks thin (last repo push Nov 2025, per a third-party tracker). |
| Audio | [cpal](https://crates.io/crates/cpal) 0.18.2 | Apache-2.0. |
| Codecs | ffmpeg-next 9.0.0, gstreamer-rs 0.25.4 | FFmpeg and GStreamer carry LGPL/GPL terms. NVENC, QSV and VideoToolbox come through them. [gpu-video](https://crates.io/crates/gpu-video) 0.4.0 (renamed from vk-video) is Vulkan Video with wgpu; Linux/Mesa focus (?). |
| Virtual camera | See below. | No mature Rust crates. |
| Texture sharing | [syphon-wgpu](https://docs.rs/crate/syphon-wgpu/latest) (macOS, wgpu 29) | No Spout crate found; you would write FFI to the Spout SDK. |
| NDI | [grafton-ndi](https://crates.io/crates/grafton-ndi), [ndi-sdk](https://crates.io/crates/ndi-sdk) | The [SDK](https://docs.ndi.video/docs/sdk/licensing) is free to ship if you link to ndi.video. NDI 6 is licensed per product. The Rust crates were last updated in 2025. |
| WebRTC | str0m 0.24.1 (sans-IO), webrtc-rs 0.21.1 (pre-1.0, moving to a sans-IO core) | Both MIT/Apache. |
| RTMP/SRT/WHIP | None found | Use FFmpeg or GStreamer. WHIP is [RFC 9725](https://www.forasoft.com/blog/article/whip-whep-replace-rtmp-live-streaming-2026). |

- **Virtual camera per OS.**
  - Windows 11: [MFCreateVirtualCamera](https://learn.microsoft.com/en-us/windows/win32/api/mfvirtualcamera), through the `windows` crate, is video-only. The OBS virtual cam works only via OBS (?).
  - macOS: a CoreMediaIO camera extension is needed. It is [Swift/ObjC](https://github.com/ldenoue/cameraextension), and a [forum thread](https://developer.apple.com/forums/thread/817478) reports flicker on macOS 26.
  - Linux: v4l2loopback via raw ioctls. The [`v4l2loopback` crate](https://docs.rs/v4l2loopback) only manages devices, it doesn't write frames.

## 6. Agent / plugins
| Option | Version | Notes |
|---|---|---|
| [rmcp](https://github.com/modelcontextprotocol/rust-sdk) | 3.5.1 (5 Oct 2026), Apache-2.0 | Official SDK. Implements spec 2026-07-28 and stays compatible with 2025-11-25. Transports: stdio and streamable HTTP; legacy SSE is not provided. Also OAuth, elicitation, tasks, and `#[tool]` macros emitting schemars JSON Schema 2020-12. Three minors in one month, so pin it. |
| schemars | 1.2.2, MIT | – |
| wasmtime | 49.0.2 stable (2 Oct 2026); 50.0.0-rc.1 out | Monthly majors. WASI p3 is on in wasi-http, but the [release notes](https://github.com/bytecodealliance/wasmtime/releases) don't say whether the async component model is stable. |
| extism | 1.41.0, BSD-3 | Simpler plugin ABI. |
| mlua | 0.12.2 | MIT. |
| rhai | 1.26.1 | MIT/Apache. |

## 7. Physics / animation
- **Physics.** rapier3d 0.36.1 (Apache-2.0) and avian3d 0.7.0 (Jun 2026, Bevy-native).
- **Spring bones.** Only `bevy_vrm` has them. Otherwise write your own Verlet solver, which is small.
- **IK.** The crates are stale: `bevy_fabrik` targets Bevy 0.14 and `bevy_mod_inverse_kinematics` targets 0.8. Write your own.
- **Animation blending.** Bevy's built-in animation graph, or `ozz-animation-rs` 0.11.0 (MPL-2.0, Oct 2025).

## 8. Collaboration
| Crate | Version | Notes |
|---|---|---|
| loro | 1.16.2 | MIT. Movable-tree CRDT, which suits scene graphs (?). |
| automerge | 0.12.0 | MIT. |
| yrs | 0.28.0 | MIT. |
| quinn | 0.11.12 | MIT/Apache. |
| iroh | 1.3.0 | MIT/Apache. QUIC peer-to-peer. |

Verdict: loro for the scene document, quinn or iroh for transport.

## 9. Packaging
- **cargo-dist** [0.32.0](https://crates.io/crates/cargo-dist) (May 2026) suits CLI and MCP binaries. For GUI installers, use the Tauri bundler.
- **Crash reporting.** sentry 0.49.3 (MIT) plus [sentry-minidump](https://crates.io/crates/sentry-rust-minidump) 0.18.1 for native crashes.
- **Updater.** The Tauri updater was not checked.
- **Windows signing.** [Azure Artifact Signing](https://learn.microsoft.com/azure/artifact-signing/quickstart) has been GA since Jan 2026 at $9.99/month. Individuals must be in the US or Canada.
- **macOS.** Developer ID plus notarization (?).

## 10. Precedents
- **Rerun:** [egui, wgpu and its own `re_renderer`](https://github.com/rerun-io/rerun/blob/main/ARCHITECTURE.md), with a web build that falls back to WebGL.
- **Zed:** GPUI.
- **Graphite:** Rust compiled to WASM on wgpu and Vello ([LWN](https://lwn.net/Articles/1051242)). Alpha, with a web frontend; exact current stack (?).
- **Tiny Glade:** Bevy per secondary sources only (low confidence).
- **Fyrox:** its own editor.

## Recommended stack
- **UI and viewport:** eframe/egui with egui_dock or egui_tiles, egui-snarl for node graphs, and a custom wgpu toon renderer drawn through `egui_wgpu` callbacks. This is the Rerun pattern, and it gives you a WebGPU build from the same code. Use Bevy plus `bevy_egui` 0.43.1 plus `bevy_vrm` only for quick prototypes.
- **Assets and ML:** `gltf` + `serde_vrm` + a custom spring-bone solver; `ort` for tracking and Silero VAD; `whisper-rs` for ASR (not sherpa-onnx, see the speech spike).
- **Automation and collaboration:** `rmcp` + `schemars`; wasmtime or extism for plugins; loro + iroh for collaboration.
- **Media:** FFmpeg for encoding and RTMP/SRT; str0m for WHIP; native per-OS virtual-camera shims.
- **Shipping:** Sentry for crashes; `cargo-dist` for CLI binaries.

## Five biggest risks
1. **Version churn.** wgpu, Bevy, egui and rmcp each ship breaking releases every few weeks to months, and `ort` is still a release candidate. Everything must share one wgpu version.
2. **Avatar-format gaps.** VRM 1.0, MToon and spring bones exist only in young crates, there is no Rust path from MediaPipe to blendshapes, and Live2D needs an agreement for VTuber apps.
3. **OS glue.** Virtual cameras, Spout and NDI, and RTMP/WHIP clients have no mature crates. This means native FFI plus a Swift extension on macOS.
4. **Pro-editor UI.** No Rust toolkit offers docking, timelines, node graphs, IME and accessibility out of the box. GPUI, Xilem and Blitz are pre-1.0, and Bevy has no editor.
5. **Licensing and signing.** Slint's GPL option, FFmpeg/GStreamer LGPL obligations, NDI attribution, Live2D, and Windows signing limits for individuals.
