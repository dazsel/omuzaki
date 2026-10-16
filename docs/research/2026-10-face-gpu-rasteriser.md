---
title: "Face GPU rasteriser: Vello against a custom coverage rasteriser (CB-24)"
summary: "Vello against a custom coverage rasteriser (CB-24)"
area: research
---

# Face GPU rasteriser: Vello against a custom coverage rasteriser (CB-24)

**Decision: use Vello (`vello 0.11`, which requires `wgpu ^30`, the workspace's version) for the GPU face rasteriser.**
Neither candidate reaches 0.5 ms at the real face's 300 to 900 commands at 1548 x 1107, so the implementation must also build the partial-repaint and caching
scheme costed in section 5. Vello is the better base for that: its GPU time is flat in command count (0.7 to 0.8 ms from 17 to 900 commands) and its CPU preparation is 0.05 to 0.14 ms, where the custom path's GPU time grows (2.2 ms at 900) and its CPU preparation is 2.5 to 8 ms. At 300 commands the two GPU times are similar (0.78 and 0.68 ms); the custom path loses on wall time (3.5 against 1.0 ms).
Spike code: `spikes/face-gpu/` (own workspace, throwaway, not in `crates/`). Raw output and images are kept with the project assets, outside this repository.
(`win-vulkan-*`, `win-dx12-*`, `lavapipe-*`; `sheet-probe-*.png` = CPU | custom | Vello | Vello diff).

## 1. What was measured, and on which adapters

- **RTX 4090**: WSL has no Vulkan driver for it (only llvmpipe), so the spike was cross-built (`x86_64-pc-windows-gnu`) and run as a Windows process
  through the native Vulkan and DX12 backends. Adapter strings from the runs: `wgpu Vulkan (NVIDIA GeForce RTX 4090)`, `wgpu Dx12 (NVIDIA GeForce RTX 4090)`.
- **lavapipe** (CI stand-in): `wgpu Vulkan (llvmpipe (LLVM 15.0.7, 256 bits))`, on a PC shared with other agents' builds.
- **GL on the 4090** (Mesa over D3D12) also opens: the custom path runs there; Vello does not (its compute shaders need `ARB_arrays_of_arrays`).
- Texture: made by `WgpuRenderer::create_texture` (dynamic, sRGB, premultiplied), drawn through `PaintTarget::native()` -> `(Device, Queue, Texture)`, the render-api D9 convention.
  Both candidates draw there. The renderer's own device (default limits, timestamp features) was enough for both; no second wgpu in the binary.
- GPU time = two timestamp queries around everything the candidate submits (the 4090 is shared and downclocks when idle: an earlier run with 150 frames varied up to 4x, so the table is one 300-frame run (`win-vulkan-*`, `win-dx12-*`) taken after the review fixes). Preparation (the CPU work that builds the GPU input) is recorded the same way for both paths, reported on its own and **included in wall**; GPU time is the draw only.
- Scenes: the 8 probes (256 x 160, copied verbatim), the mock painter's neutral and happy faces (the **real painter's sections are still stubs**
and draw nothing, so it could not be measured), and a deterministic synthetic face with 300 and 900 commands (eyes with clips, radial
  gradients, round-capped strokes, big soft blush gradients). The synthetic face is a load model, not a quality reference.
- Quality = PSNR of premultiplied RGBA8 against `SkiaRasterizer` on the same list.

## 2. Options table

| | (a) Vello 0.11 | (b/c) custom: CPU flatten + stroke expansion, GPU analytic coverage (fragment shader, edge lists bucketed in 16-row bands) |
|---|---|---|
| 4090 Vulkan GPU p50 / p95, mock neutral, 17 cmds, 1548 x 1107 | 0.71 / 1.05 ms | 0.25 / 0.45 ms |
| 4090 Vulkan GPU p50 / p95, 300 cmds, 1548 x 1107 | 0.78 / 1.21 ms | 0.68 / 0.92 ms |
| 4090 Vulkan GPU p50 / p95, 900 cmds | 0.82 / 1.16 ms | 2.15 / 3.40 ms |
| 4090 Vulkan GPU p50 / p95, brows layer 774 x 554, 300 cmds | 0.74 / 1.06 ms | 0.64 / 1.01 ms |
| 4090 DX12 GPU p50 / p95, 17 / 300 / 900 cmds | 1.10 / 1.31 / 1.40 ms (p95 1.4 / 2.8 / 2.0) | 0.45 / 0.99 / 2.78 ms (p95 0.9 / 1.6 / 4.2) |
| **CPU preparation per frame, 300 / 900 cmds** (scene build and encode for Vello; flatten, stroke expansion and band bucketing for custom) | 0.05 / 0.14 ms | **2.5 / 8.1 ms** (unoptimised, single thread) |
| **wall p50 / p95 (preparation + GPU), 4090 Vulkan, 300 cmds** | 0.98 / 1.70 ms | 3.54 / 4.32 ms (2.52 preparation + the draw) |
| wall p50, 4090 Vulkan, 900 cmds | 1.14 ms | 10.46 ms |
| lavapipe p50 (GPU), 17 / 300 / 900 cmds | 41 / 101 / 185 ms | 52 / 212 / 571 ms |
| PSNR, mock faces | 47 to 56 dB | 50 to 58 dB |
| PSNR, synthetic faces | 34.2 to 37.0 dB | 39.3 to 42.8 dB |
| GPU memory (allocator report, process) | 220 MiB | 52 MiB |
| Lines of integration | `vello_path.rs` 225 (scene translation 100, premultiply resolve pass 70) | `geom.rs` 501 + `custom.rs` 184 + `fill.wgsl` 127 = ~810, and a stroker to own |
| New dependencies | 24 crates over wgpu (21 plus vello, vello_encoding, vello_shaders; registered in `docs/licensing/register.md`): vello, vello_encoding, vello_shaders, peniko, kurbo, color, skrifa, read-fonts, font-types, png, flate2, miniz_oxide, fdeflate, crc32fast, adler2, simd-adler32, euclid, guillotiere, svg_fmt, polycool, futures-intrusive, futures-core, linebender_resource_handle (+ lock_api dup) | none (wgpu, bytemuck) |
| Licences of those | all MIT or Apache-2.0 or dual (a few `MIT OR Zlib OR Apache-2.0`, `0BSD OR MIT OR Apache-2.0`); none non-commercial | n/a |
| wgpu constraint | `vello 0.11` = `wgpu ^30` (0.10 and older target older wgpu); one wgpu in `Cargo.lock` verified | workspace wgpu |
| WebGPU / WASM | `cargo check -p vello --target wasm32-unknown-unknown` passes; not run in a browser | pure render pipeline, 5 storage buffers visible to vertex and fragment stages (needs the core, not compat, limits) |
| GL (Mesa/D3D12) | **fails** (compute) | runs |

Rlib sizes (debug-info build, so only an indication): vello 5.1 MB, vello_encoding 4.5 MB, skrifa 10.4 MB, read-fonts 25 MB (pulled in for glyph support the face never uses).

**Quality notes.** Every probe passes the 34 dB bar for both candidates. Hairlines: Skia modulates sub-pixel strokes, both candidates draw exact
coverage, which is why that probe sits at 41 dB for both. The synthetic faces are near the bar for Vello (34.2 to 37.0 dB on 300 to 900 overlapping translucent
gradient layers; cause not isolated: 8-bit blend order, gradient evaluation or the AA filter, to chase in the implementation with a per-section diff). Round caps, miter joins (limit 10),
non-zero rule, nested clips, straight-colour gradients, per-command opacity and transform stacks all map one to one onto Vello's `Scene`
(`fill`, `stroke` with `kurbo::Stroke`, `push_clip_layer`, `peniko::Gradient` with `InterpolationAlphaSpace::Unpremultiplied`; the default
premultiplied interpolation fails the gradient probe at 33.5 dB, the straight one gives 46.3 dB).

## 3. Integration findings (for the implementation)

1. `native()`'s texture has no `STORAGE_BINDING` usage and Vello renders only to a storage texture, so the spike renders to a private `Rgba8Unorm` storage
   texture and draws it into the target with a full-screen pass. That pass is also needed because **Vello writes straight alpha**
   and the face texture is premultiplied. Cost: one extra texture of the layer's size and a pass (inside the numbers above).
   Alternative: the renderer adds `STORAGE_BINDING` to dynamic `Rgba8Unorm` textures and a resolve compute step premultiplies in place; saves the copy, not the conversion.
2. Vello's renderer object compiles its pipelines at creation (not timed here), so it is held for the app's life, one per device.
3. Frame timing on `native()` needs the caller's queue; everything went through the `(Device, Queue, Texture)` tuple with no extra handle.

## 4. Risks of Vello

- Alpha-state crate (its README says so): conflation artefacts, no filters; the face needs neither. Pin the exact version; wgpu bumps are gated on Vello's.
- 220 MiB of GPU memory (bump and intermediate buffers) against 52 MiB; per layer sizes grow it.
- Fixed per-frame overhead of ~0.7 ms GPU on the 4090 even for 17 commands: a floor above half the 0.5 ms budget. Frame time is flat in command count (17 to 900 commands: 0.71 to 0.82 ms).
- GL fallback is impossible (compute). Vulkan, DX12, Metal and WebGPU are fine; the software path stays `SkiaRasterizer`.
- Variance: an earlier noisy run (shared, downclocked GPU) showed p95 up to 8 ms at 900 commands; the final run is quiet. The implementation needs a longer benchmark on a quiet GPU.

## 5. Budget: what has to change

Spec §10: <= 0.5 ms at 1548 x 1107 (774 for brows) on the 4090, <= 3 ms on lavapipe. **Neither candidate meets either bar at 300 to 900 commands**
(Vello 4090 Vulkan: 0.8 ms GPU + 0.05 to 0.14 ms CPU at 300 to 900 commands; lavapipe: 100 to 185 ms). What gets there, in order of payoff:

1. **Partial repaint by section.** Most of a face is static each frame; only the eyes, mouth and brows change. Keep one cached `Scene` per section (Vello's `Scene::append`),
   re-encode only sections whose `DrawList` fingerprint changed, and render only the changed rectangle. A frame that changes ~25% of the commands should cost
   about the 17-command figure (an estimate, not measured) (about 0.7 ms GPU, the fixed floor). Cost: a per-section fingerprint in the face system and a damage rect in the painter. About 2 days.
2. **Cache glyph-like pieces** (irises, highlights, lashes) in a small atlas of pre-rasterised sprites composited instead of redrawn; saves commands, costs atlas memory and loses crispness at extreme zoom (re-bake on zoom).
3. **Smaller texture under load**: `px_per_m` is already a runtime knob; 0.5x is 4x fewer pixels and 0.7 ms at 900 commands is dominated by command count, so this helps less than 1.
4. **Lavapipe**: 3 ms is unreachable by two orders of magnitude for either rasteriser at full size. Recommend the CI gate runs the GPU rasteriser on lavapipe only at
   probe size (256 x 160, ~15 ms) for correctness (PSNR against `SkiaRasterizer`), and the timing gate runs on a real GPU runner. This needs a spec §10 edit.
5. The custom path would need the same measures plus a GPU stroker and tile binning to fix its 2 ms GPU at 900 commands and 2.5 to 8 ms CPU at 300 to 900 commands; rejected on cost.

## 6. Recommendation for the implementation and the strokes work

- Fills, gradients and clips: add `vello` to `[workspace.dependencies]` (`default-features = false, features = ["wgpu"]`), the register rows are already added, implement `GpuRasterizer` as
  `DrawList -> Scene` (the spike's `vello_path.rs` is the starting point), the storage texture and the per-repaint decode-premultiply-mip pass of section 7 item 4, and per-section scene caching.
- Strokes: reduces to mapping `Cap`, `Join` and the miter limit 10 onto `kurbo::Stroke`; the probes already pass. Remaining work is hairline parity (Skia's modulated hairlines) if wanted.

## 7. Decisions

1. **Vello 0.11 is adopted** (one wgpu in the lockfile; all 24 added crates MIT or Apache-2.0).
2. **Partial repaint by cached section layers.** Neither candidate repaints a full face in 0.5 ms, so each section renders to its own cached layer and a frame repaints only the sections whose inputs changed (eyes, mouth, brows); glyph or piece caching applies within a section. Re-measure on the real sections once they land.
3. **CI gates.** lavapipe checks correctness at probe size only. The timing gate needs a real-GPU job; until it exists, every PR touching the face painter attaches 4090 numbers (native Windows, as this spike did). Spec v3 §10 is updated to say so.
4. **Storage texture, then a per-repaint premultiply pass (revised).** The first decision (STORAGE_BINDING on the target, premultiply on sampling) was reversed. The implementation guidance is:
   - the rasteriser writes straight alpha into a private `Rgba8Unorm` storage texture;
   - a pass per repaint decodes sRGB, premultiplies in linear light and writes the face layer's `Rgba8UnormSrgb` target, building alpha-weighted mips that cover odd extents;
   - the material samples that target with the normal hardware sampler (anisotropy, wrap, trilinear) as premultiplied.
   This keeps the spike's resolve pass (and its private storage texture) as the design, not a stopgap; sections 3.1 and 6 describe the spike's version of it (it premultiplied in sRGB space into `Rgba8Unorm`, which this decision supersedes).
5. **Quality.** 34 to 37 dB on the synthetic faces is acceptable for now; re-check on the real sections. **Correction after the diff fix:** the first diff images wrapped differences of 32 or more to white, which hid them. The regenerated `sheet-synth-*-diffs.png` show Vello's differences are not only edge anti-aliasing: the radial-gradient irises differ across their whole area (the custom path's diffs are edges only). Gradient interpolation in Vello versus the CPU rasteriser needs a look in the implementation before the 34 dB bar is trusted for gradients.

**Gradient measurement.** Vello against the reference on the radial-gradient irises: max difference 8 levels, p99 5 levels, in rings (gradient banding), which explains the whole-area diffs seen in the synthetic sheets. **Requirement:** the gradient comparison against the CPU rasteriser must be part of the implementation's acceptance (tolerance max 8 / p99 5 on rings, or tighter), not left to the 34 dB overall bar.

## 8. After the implementation: where the time went (#2074)

Measured on the development PC (`Dev`, provisional: the CPU's turbo capped at 99 % until its repair, other agents
compiling, 1-minute load average 16 to 36), native Windows, Vulkan, the mock face at 1440 x 1044 px, GPU times from
timestamp queries.

**The old probe measured the CPU, not the GPU.** Its two timestamps sat in separate submissions around the whole
repaint, so they measured the GPU waiting for the CPU to prepare and submit Vello's work (5.8 ms on #1658, 1 to 5 ms
in this session, in an order unrelated to the size of the change). Keeping the GPU busy in front of the repaint (a
12 ms compute pass, so the timestamps only see GPU work) gives what `vello::Renderer` costs:

| whole-face repaint, `vello::Renderer` (before) | where | p50 ms |
|---|---|---:|
| scene encoding (`SceneEncoder`) | CPU | 0.105 |
| Vello's prepare, upload and submit (`render_to_texture`) | CPU | 0.774 |
| the resolve's record and submit | CPU | 0.125 |
| Vello's 17 compute passes, uploads included | GPU | 0.199 |
| the resolve | GPU | 0.021 |

So section 4's "fixed 0.7 ms floor" was CPU: Vello rebuilds its recording, about 20 bind groups, a buffer-pool lookup
per buffer and a gradient texture on every call, 816 allocations per repaint. The GPU work was 0.2 ms.

**What changed.** `raster::gpu::engine` runs the same `vello_shaders` stages on buffers, textures and bind groups kept
between repaints (grown only when a scene needs more), as one compute pass recorded into the renderer's encoder; the
renderer submits it and times it (render-api, "GPU interop"). Same pixels: every probe and section case still passes
its rule against the CPU raster, on lavapipe and on the 4090.

| after, the same GPU kept busy | GPU raster | GPU resolve | GPU total | CPU (encode + record) |
|---|---:|---:|---:|---:|
| full face | 0.159 | 0.005 | 0.190 | 0.20 |
| mouth only | 0.069 | 0.001 | 0.096 | 0.12 |
| eyes + mouth + brows | 0.115 | 0.003 | 0.145 | 0.19 |
| brows only | 0.063 | 0.001 | 0.091 | 0.11 |

A warm repaint now makes 27 allocations, all wgpu's (the passes and three queue writes), against 816.

**Clocks decide the rest.** The GPU times above hold while the GPU is busy. In the live stage unpaced, the face paint
(the face's commands and the mip rebuild, the renderer's `PaintTextures`) is 0.24 ms p50 and 0.30 ms p95 with the face
changing every frame. Paced at 30 fps with the mock character, the 4090 has so little to do that it stays at its idle
clock, and everything on it takes about 14 times as long: the face paint 3.4 ms p50, the rest of the frame 0.98 ms
(0.07 ms unpaced). Reading `gpu.face_paint` paced (spec v3 §10) therefore needs the budget scene's real load, or a fixed
GPU clock, on `Dev`; with the mock character it measures the GPU's power policy. Trusted shader modules (no bounds
checks) would take another 20 % off the raster; they need `unsafe`, so they are not used.
