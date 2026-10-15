---
title: "Model watermarking and theft protection for exported avatars (2026-10-13)"
summary: "watermark and theft-protection techniques for exported avatars (what survives Blender re-export, JPEG, resize, decimation), a measured recommended texture mark, the attack model and the open legal points."
area: research
---

# Model watermarking and theft protection for exported avatars (2026-10-13)

> Research (SE-07). Spec: v3 §27 (EX-01, EX-02), §31 (SE-07), §37 (exports carry no watermark and no revenue cap), §40 (licences). **This is an engineering reading, not legal advice; open legal points are listed in the licence register.** The mark is the **creator's own and opt-in**, never Omuzaki's branding (§37).
>
> **Update (2026-10-13).** Read in a follow-up pass: Chen and Wornell 2001 (MIT DSP group PDF), Ohbuchi, Mukaiyama and Takahashi 2002 (University of Aizu PDF), Bors CORESA 2007, the Cox patent US 5,930,369 (Google Patents), and the UniVRM and glTF-Blender-IO sources on GitHub. Still unreachable: the Cox 1997 paper itself (IEEE, bot challenge), docs.blender.org and the Eurographics library. Claims changed below are marked "follow-up". No Blender or UniVRM round trip was run (an interactive Blender session must not be touched, and no licence-clean test avatar exists yet).
>
> **Source status as the first pass found it.** The research environment's network policy blocks arxiv.org, doi.org, dl.acm.org, ieeexplore.ieee.org, diglib.eg.org, docs.blender.org, projects.blender.org and most university hosts (HTTP 403 on CONNECT, logged by the proxy; I did not route around it). `raw.githubusercontent.com` works. So:
> - **No watermarking paper was read.** Every paper below is `secondary` (a search-result record or abstract) or `unverified (unreachable from the research environment)`. The follow-up pass should read them from a machine with access (list in section 9).
> - Specs (glTF, VRM 1.0) and the Blender, three-vrm and invisible-watermark sources were read directly, so they are `verified`.
> - **Every number in the survives columns marked `measured` is ours**, from throwaway scripts in a scratch directory (deleted afterwards; the method is in section 6 so it can be re-run). They use synthetic data only; no third-party avatar, no private character, no recording of a person.
>
> Confidence key: `verified` = read at the source or measured here; `secondary` = a record or abstract seen, text not read; `unverified` = unknown, to be treated as unknown.

## 1. Verdict

- **Recommended default: a keyed spread-spectrum mark in a fixed canonical grid of the texture's luma** (section 5). Std-only Rust, **151 lines** in the spike. 64-bit payload, **49.6 dB PSNR or better**, **0.56 s to embed and 0.19 s to detect on a 4096 px texture** (Rust, release build, wall time including a 50 MB read and write). All 64 bits survive PNG, JPEG q90 and q75, resize to 50 percent and 25 percent, blur, a tone change and a 20 percent repaint. It does **not** survive crop, rotation, flip or a UV re-layout, and it dies to a determined repaint of the whole texture.
- **A second layer is worth having, with a caveat:** a keyed normal-displacement mesh mark (our own design, section 5.4). On synthetic meshes it survives float32 and 11-bit quantisation, vertex and index reordering, noise, Laplacian smoothing and a crude 50 percent decimation. It is **not** proven on a real avatar mesh and its visual cost is **not judged** (no render review yet). So: **ship the texture mark first; the mesh mark stays a spike** until a render review and a real-mesh test exist.
- **Canaries** (planted names and `extras`) cost nothing, survive some re-exports and not others (section 4); they identify a leak, they do not stop one. **A signature is not a watermark**: any re-export drops it.
- **Nothing stops a determined thief.** A repaint, a re-bake of the texture from a render, or a full remesh removes every technique here. The mark is for catching a resold commission or a leaked paid asset pack, where the thief re-exports and re-saves but does not rebuild (section 3).

## 2. Technique table

"Measured" cells were run on the synthetic textures and meshes of section 6 (64 bits matched out of 64 unless stated). "n/a by construction" means the detector assumes the original pixel grid, so a resize breaks it before any noise does. Blender and UniVRM are the re-export paths (section 4).

| # | Technique | Hides in | Payload | Survives (re-export / PNG→JPEG 90, 75 / resize 50% / decimate 50% / remesh / repaint) | Blind? | Visual cost | CPU cost | Code licence | Primary source (retrieved 2026-10-13) | Confidence |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | Visible texture mark (corner logo or tiled overlay) | texture pixels | any image or text | pixels pass a PNG re-export unchanged; JPEG and resize keep it (Blender and UniVRM re-export: `unverified`); can be cropped or painted out; decimate and remesh do not touch it | yes, by eye or OCR | high: the point of it | trivial | ours to write | none needed (follows from the formats) | `unverified`: not measured |
| 2 | Keyed LSB, blue channel, 256 repeats per bit | texture low bits | 64 | PNG round trip **64**; JPEG q90 **29 to 34** and q75 **27 to 37** (chance is 32); blur 35 to 43; tone change 25 to 43; resize n/a by construction; repaint 20% **64** | yes | none: PSNR 74 to 86 dB | 0.03 s (1024) to 0.95 s (4096), numpy | ours | `measured` (§6) | `verified` that it **dies on JPEG** |
| 3 | Block-DCT QIM on luma, 2 coefficients per 8×8 block, step 20, keyed block→bit map | mid-frequency DCT coefficients | 64 | PNG **64**; JPEG q90 **64**, q75 **64**; blur σ1 **46, 46, 63**; tone **64**; repaint 20% **64**; resize n/a by construction (block grid lost); crop, flip would fail the same way | yes | PSNR 47.4 dB, SSIM 0.985 to 0.988 | 9.3 s at 4096 in numpy (not representative of Rust) | ours to write; method: Chen and Wornell, IEEE Trans. Inf. Theory 47(4), May 2001 | `measured`; method record `verified` (read in the follow-up pass: Chen and Wornell, IEEE Trans. Inf. Theory 47(4), May 2001, 1423: QIM and dither modulation embed by quantising with one quantiser per message value; https://dsp-group.mit.edu/wp-content/uploads/2024/11/QuantizationIndexMod.pdf, checked 2026-10-13) | `verified` for the measured edits |
| 4 | **Canonical-grid spread spectrum on luma** (the recommendation) | luma, at a fixed 512×512 canonical scale | 64 (could be higher, §5.3) | PNG **64**; JPEG q90 **64**, q75 **64**; resize 50% (Lanczos and bilinear) **64**; resize 25% **64**; blur σ1 **64**; tone **64**; repaint 20% **64**; crop 12.5% border **33 to 38** (fails); flip **32 to 36** (fails) | yes (key needed) | PSNR **49.6 to 50.0 dB**, SSIM 0.993 to 0.997 at amplitude 0.15 | **0.56 s embed, 0.19 s detect at 4096** in Rust | ours; idea family only: Cox et al., IEEE Trans. Image Processing 6(12), Dec 1997, DOI 10.1109/83.650120. **Follow-up:** the Cox patent text (US 5,930,369, https://patents.google.com/patent/US5930369A/en, checked 2026-10-13) describes embedding in the perceptually significant FFT/DCT coefficients with about 1000 N(0,1) values, `x' = x(1 + a·w)`, and **non-blind** detection (the extractor compares with the original). Our mark is spatial, keyed and blind, so it is a different method in the same family | `measured`; method record `verified` (patent description; the 1997 paper itself unread: IEEE blocked) | `verified` for the measured edits |
| 5 | DwtDctSvd (the `invisible-watermark` Python package) | DWT, DCT and SVD of the **U chroma** channel by default (`scales=[0,36,0]` in [`dwtDctSvd.py`](https://raw.githubusercontent.com/ShieldMnt/invisible-watermark/main/imwatermark/dwtDctSvd.py), read 2026-10-13; the Y and V scales are 0) | its own length | not measured | yes | not measured | not measured | MIT: `LICENSE` read, "Copyright (c) 2021 ShieldMnt" ([raw LICENSE](https://raw.githubusercontent.com/ShieldMnt/invisible-watermark/main/LICENSE)). Python, so we would not ship it; the licence would allow a port | licence `verified`; survives `unverified` | `unverified` (survives) |
| 6 | Mesh mark in vertex or index order (a permutation encodes bits) | the order of vertices or indices | about log2(n!) bits, in theory | **destroyed by Blender's exporter**: it de-duplicates vertices with `fast_structured_np_unique`, whose doc says the result "may not be sorted according to their original type" ([`gltf2_blender_utils.py`](https://raw.githubusercontent.com/KhronosGroup/glTF-Blender-IO/master/addons/io_scene_gltf2/blender/com/gltf2_blender_utils.py), glTF-Blender-IO `master`, addon 5.4.4). **UniVRM (follow-up pass):** `MeshExporter_SharedVertexBuffer.cs` writes `mesh.vertices` (axis-inverted) in the Unity mesh's own order, so UniVRM would keep vertex order while Blender does not | yes | none | trivial | ours | `verified` (code read) | `verified` that Blender reorders; the idea is **not viable** because of Blender |
| 7 | **Mesh mark: keyed displacement along vertex normals** (our design, §5.4) | vertex positions | 64 | synthetic icosphere, 40 962 vertices, amplitude 0.1 of the median edge: float32 **64**; 11-bit quantisation **64**; vertex and index reorder **64**; noise 0.02 edge **64**; Laplacian smoothing ×1 and ×3 **64**; clustering decimation to 50% **64**. With a wrinkled host (wrinkle height 1.0 edge) clean **64**, decimated **64** (z 4.3). Remesh: not measured, expected to fail | yes (key needed) | rms displacement 0.05 percent of the bounding diagonal; visible shading noise **not judged** | not measured (numpy only) | ours | `measured`, synthetic only | `verified` on the synthetic cases; **real meshes `unverified`** |
| 8 | Spectral mesh watermark (Ohbuchi, Mukaiyama, Takahashi, Eurographics 2002, Computer Graphics Forum 21(3), DOI 10.1111/1467-8659.t01-1-00597) | mesh spectral coefficients | not read | **follow-up pass, paper read** (https://web-ext.u-aizu.ac.jp/~shigeo/pdf/eg2002b.pdf, checked 2026-10-13): abstract says "robust watermarking algorithm with informed detection" and "resistant against mesh simplification and remeshing combined with resection, similarity transformation"; extraction compares with the reference (original) mesh, as the Bors CORESA 2007 review also states | **no**: informed detection, the original mesh is needed (**confirmed**) | not quantified in the text read | heavy: eigen-decomposition of a mesh Laplacian | the paper states no code or licence; treated as `NO` by the policy (nothing cleared) | `verified` (paper) | `verified` for the method; licence `unverified` |
| 9 | Canary: planted node, morph target and expression names | `nodes[].name`, `extras.targetNames`, `VRMC_vrm.expressions.custom` keys | about 10 to 40 bits per name, readable | VRM 1.0 allows custom expression names and three-vrm 3.5.5 loads them by name on **import** (it has no exporter, so this is not a round trip); both the glTF and VRM texts keep `name` ([§4](#4-what-survives-each-re-export-path)). Blender: the addon's `export_morph` ("Export shape keys (morph targets)") defaults to **True** (glTF-Blender-IO `__init__.py`, verified 2026-10-13), so shape keys are exported; whether their names land in `extras.targetNames` was not found in the source read (`unverified`). **UniVRM (follow-up pass, source read):** `Vrm10Exporter.cs` writes `Custom = ...CustomClips.ToDictionary(c => c.name, ...)`, so custom expression names survive a UniVRM export, and `MeshExporter_SharedVertexBuffer.cs` writes `gltf_mesh_extras_targetNames` for blend shapes. Any user rename or a retarget to another rig drops them | yes | none | trivial | ours | `verified` (specs, three-vrm) | `secondary` for Blender and UniVRM |
| 10 | Canary: planted `extras` and `extensions` data | glTF `extras`, `extensions` | up to KBs | Blender export of "Custom Properties" is **off by default** (`export_extras ... default=False`, addon 5.4.4); three-vrm's import ignores unknown extras (import only); UniVRM: no code path writing arbitrary node `extras` found in the exporters read (`unverified`) | yes | none | trivial | ours | `verified` (Blender default, spec) | `verified` that a default Blender re-export drops it; UniVRM `unverified` |
| 11 | Canary: unused texture channel (alpha, or a spare channel of an occlusion or roughness map) | texture channel values | 8 bits per pixel | a re-bake or re-pack of the texture drops it; Blender: "Images that need alpha are saved as PNGs" so an alpha channel is kept when used, an unused one: `unverified` | yes | none | trivial | ours | not read | `unverified` |
| 12 | Signature (ed25519 over the canonical hash; the SE-07 baseline, not a watermark) | a sidecar field | 64-byte signature plus key fingerprint | **stripped by any re-export**: the hash changes, and the repo's own design says an unsigned or failing item shows as "unverified publisher" (`docs/design/library.md`, "Publisher", line 132) | yes | none | microseconds | `ed25519-dalek` BSD-3-Clause (already in the register) | `verified` (repo design) | `verified` |

Rows 2, 3, 4 and 7 each have a measurement for every edit they claim. Rows 1, 5, 8 and 11 have survives cells that are `unverified`, which counts as unknown. **The brief's acceptance asks for every row's survives columns to come from a source or a measurement; with the paper hosts blocked, rows 1, 5, 8, 9 (Blender and UniVRM cells) and 11 do not meet it.** This note is a draft for that reason (section 9).

Edits in the brief's list that no technique here was measured against: **glTF to VRM and back** (no VRM exporter ran), **UniVRM re-export** (UniVRM source paths were not found), **remesh** (no remesher), **real decimation** (edge-collapse; ours is vertex clustering). These are `unverified`.

## 3. Attack model

**We want to catch (the mark's job):**
- A **resold commission**: the buyer gets a VRM or glTF, re-exports it through Blender, re-saves textures as JPEG, resizes them for a game, and lists it for sale. The 64-bit payload is the creator's key fingerprint, so a creator can show the mark is theirs.
- A **leaked paid asset pack** (LB-08 marketplace): the files leave the app intact or re-exported; a canary plus the texture mark identifies the build or the buyer if the payload is per-buyer.

**What no technique here stops:**
- **A determined repaint** or a **re-bake of the texture from a render**: the mark lives in pixels; new pixels carry nothing.
- **A full remesh and re-UV**: a new topology and atlas carry no keyed pattern. The texture mark needs the original UV layout; the mesh mark needs the original surface.
- **Crop, rotation, flip and atlas re-layout** of a texture (measured: 33 to 38 bits of 64, chance is 32). A resynchronising detector (search over flips and 90 degree turns, 8 candidates) is a cheap extension; arbitrary crops need a different design.
- **Averaging attacks:** the canonical-grid pattern is the **same on every texture a creator marks with the same key**. Averaging the high-passed canonical fields of many of their textures would estimate it. Mitigation: derive the pattern seed per texture slot and per release (`blake3` derive-key over the creator key, a release id and the slot name). Not measured.
- **A known-original attack:** with the unmarked original, subtraction reveals the mark. Normal for any watermark; the creator's own originals are not public.
- **A key-holder fabricating a mark** on someone else's model. A mark proves "this texture carries pattern P keyed by K", not who made the model first. Provenance needs the signed export and timestamps, which this note does not design.

**Legal use is a matter for legal review**. A watermark is evidence, not a verdict.

## 4. What survives each re-export path

Read 2026-10-13; version noted per source.

| Path | What I read | Result |
|---|---|---|
| glTF 2.0 spec, `Specification.adoc` (KhronosGroup/glTF, branch `main`) | `extras` is "Application-specific data" and "**SHOULD** be a JSON object" ([`extras.schema.json`](https://raw.githubusercontent.com/KhronosGroup/glTF/main/specification/2.0/schema/extras.schema.json)). Any object **MAY** have `extensions`; all used extensions **MUST** be in `extensionsUsed`. Names are optional and "not guaranteed to be unique". The `extras.targetNames` array is described as something "many tools add" ([spec text](https://raw.githubusercontent.com/KhronosGroup/glTF/main/specification/2.0/Specification.adoc)). | The format allows canaries; it does not oblige a tool to keep them. |
| VRM 1.0 spec, `VRMC_vrm` (vrm-c/vrm-specification, `master`) | `meta` has `name`, `authors`, `copyrightInformation`, `contactInformation`, `licenseUrl` and more; `expressions.custom` holds custom expressions whose names "cannot be the same as any Preset Expressions" ([meta.md](https://raw.githubusercontent.com/vrm-c/vrm-specification/master/specification/VRMC_vrm-1.0/meta.md), [expressions.md](https://raw.githubusercontent.com/vrm-c/vrm-specification/master/specification/VRMC_vrm-1.0/expressions.md)) | `copyrightInformation` is a **visible** claim, not a watermark; custom expression names are a canary slot. |
| three-vrm 3.5.5 (pixiv/three-vrm, `dev`, `VRMExpressionLoaderPlugin.ts`) | Custom expressions are loaded by name from `schemaExpressions.custom`; an unknown **preset** name is dropped with a warning ([source](https://raw.githubusercontent.com/pixiv/three-vrm/dev/packages/three-vrm-core/src/expressions/VRMExpressionLoaderPlugin.ts)) | A canary in a **custom** expression name is read by three-vrm on **import** only: the v3.5.5 tree has no exporter, so preservation through a three-vrm round trip is `unverified`. An unknown preset name is dropped on import. |
| Blender glTF addon 5.4.4 (glTF-Blender-IO, `master`, `__init__.py`) | `export_extras`: "Export custom properties as glTF extras", **`default=False`**. `export_image_format` `AUTO`: "Save PNGs as PNGs, JPEGs as JPEGs". `JPEG`: "Images that need alpha are saved as PNGs though". `export_jpeg_quality` default **75** ([source](https://raw.githubusercontent.com/KhronosGroup/glTF-Blender-IO/master/addons/io_scene_gltf2/__init__.py)). The primitive extractor de-duplicates vertices with `fast_structured_np_unique` ([`primitive_extract.py`](https://raw.githubusercontent.com/KhronosGroup/glTF-Blender-IO/master/addons/io_scene_gltf2/blender/exp/primitive_extract.py)); that helper's doc says the output order may differ from the input's. | A default Blender re-export **drops `extras`**, **re-orders vertices**, and may re-encode JPEG at q75, which is exactly the edit we measured. Our texture mark survives it; a vertex-order mark does not. |
| UniVRM (`vrm-c/UniVRM`, `master`, read in the follow-up pass 2026-10-13) | `LICENSE.txt`: MIT ("Copyright (c) 2020 VRM Consortium"). `Packages/UniGLTF/Editor/.../EditorTextureSerializer.cs`: a texture is exported as the **original asset file bytes** (png or jpg) when the importer's max size is at least the original size, otherwise `RuntimeTextureSerializer` calls `EncodeToPNG` (**PNG, never JPEG**). `MeshExporter_SharedVertexBuffer.cs`: positions are `mesh.vertices` after `axisInverter.InvertVector3`, in the mesh's order, with `extras.targetNames` written for blend shapes. `Vrm10Exporter.cs`: custom expressions are exported keyed by clip name. | A UniVRM export does not JPEG-recompress and does not reorder vertices, but it **inverts an axis**: a mesh mark keyed by absolute position (§5.4) would see mirrored coordinates, so the detector must also try the mirrored axis (inference, not measured). Texture resize depends on Unity import settings (`maxTextureSize`). A real round trip is still not run. |

## 5. The recommended default

### 5.1 Why this technique

It is the only texture technique measured that survives **resize** (the thief's most common edit, usually forced by a game's texture budget) as well as JPEG, and its code is small. LSB dies at the first JPEG save (rows above). Block QIM survives JPEG but is tied to the pixel grid, so a resize breaks it. A signature does not survive any re-export.

### 5.2 How it works (the spike is 151 lines, std only)

- **Payload:** the first 64 bits of the creator's ed25519 key fingerprint (`docs/design/library.md` line 132: "a display name plus the fingerprint of an ed25519 public key"). The fingerprint is public, so the payload alone proves nothing; the **secret** is the pattern seed.
- **Seed:** a 64-bit value from `blake3` derive-key over the creator's private key, the release id and the texture slot (`blake3` is already in the register as `yes`). Not the fingerprint.
- **Canonical grid:** 512×512 cells. For each cell `x`, one 64-bit hash `h(seed, x)` (a splitmix64-style finaliser; bit `i` of `h` is the sign of pattern `i` at that cell). The mark field is `W(x) = Σ_i (2·b_i − 1)(2·h_i(x) − 1) = 64 − 2·popcount(h(x) XOR payload)`: one hash and one popcount per cell for all 64 bits.
- **Embed:** upsample `W` bilinearly to the texture size, add `amp · W` to R, G and B equally (a pure luma change), round, clamp. Amplitude `amp = 0.15` gives the PSNRs above.
- **Detect:** convert to luma, area-resample to 512×512 (works for any size and for a resized texture), subtract a 5×5 box mean (so smooth image content does not drown a white pattern), then for each bit `i` correlate with the pattern: `z_i = Σ H(x)·s_i(x) / (σ_H · 512)`. Decoded bit = sign of `z_i`. One hash per cell, 64 multiply-adds per cell: 16.8 million per detection.
- **Decision rule:** compare the decoded bits with the claimed payload. For an unrelated texture the matches are Binomial(64, 0.5); measured nulls were 26 to 38. Tail probabilities: ≥44 of 64 is 1.8e-3, **≥48 is 3.9e-5**, ≥52 is 2.3e-7, ≥56 is 2.8e-10, 64 is 5.4e-20. **Recommend ≥56 of 64 as "match"** (false positive about 3e-10 per test; every measured edit scored 64) and 48 to 55 as "possible, collect more textures".
- **Detection op shape (a design sketch, no contract):** `watermark.detect { texture, claimed_payload?, keys[] }` → `{ matches: u8, bits: [i8;64], z_mean: f32, verdict: match | possible | none }`. It needs the secret seed, so it runs only with the creator's key present (read-only, no network); it is a `read` op for the maintainers to classify in the ops registry.

### 5.3 Measured cost (Rust spike, release, wall time incl. file IO, 4-core VM; single thread)

| Texture | Embed (s) | Detect (s) | PSNR, amp 0.15 | SSIM, amp 0.15 | PSNR, amp 0.3 | z mean after JPEG q75, amp 0.15 |
|---|---|---|---|---|---|---|
| 1024² | 0.06 | 0.05 | **50.0 dB** | 0.993 | 44.4 | 20.5 |
| 2048² | 0.17 | 0.09 | **49.6 dB** | 0.995 | 44.0 | 11.3 |
| 4096² | **0.56** | **0.19** | **49.6 dB** | 0.997 | 43.9 | 8.6 |

(Embed and detect seconds were taken at amp 0.3; the embed work does not depend on the amplitude. SSIM is the mean over luma with a 7×7 uniform window, K1 0.01, K2 0.03. PSNR is over RGB.) The brief's bar is **40 dB or above for an invisible mark**: met with 9.6 dB to spare, so the amplitude can double and still meet it. At amp 0.1 the 4096 texture scored 52.6 dB with z mean ≥ 4.8 on every edit.

**Margins and limits of this measurement:**
- The textures are **synthetic** (value noise, stripes, shapes, grain). A hand-painted texture with sharp detail has a larger high-passed variance `σ_H`, which lowers `z`. The `z` values above are in units of the host's own noise, so they already scale with `σ_H`; real textures will land lower. **Re-measure on a real licensed texture before the amplitude is fixed** (needs an approved asset; not a private character).
- The 64-bit payload is a choice, not a limit: the same code with more patterns per hash word, or two hashes per cell, carries 128 bits; each bit costs sensitivity.
- Time for the Python scripts' own LSB and QIM numbers (rows 2, 3) is numpy and not comparable with Rust.

### 5.4 The mesh layer (a spike, not yet a recommendation)

Each vertex gets a coarse cell from its position (cell edge = 3 median edge lengths, so the mark's own displacement does not change the cell). A keyed hash of the cell picks the payload bit and a ±1 chip. The vertex moves along its normal by `amp · chip · (2·bit − 1)`. Detection computes each vertex's Laplacian residual along its normal (vertex minus the mean of its neighbours) and correlates it with the chip, per bit. It keys on **position**, not index, which is why a reorder does nothing.

Measured on a 40 962-vertex icosphere with low-frequency bumps (and a harder host with wrinkles up to one edge length high): see row 7. Where it will fail: **Draco-style quantisation on a real avatar** (11 bits over a 2 m bounding box is 1 mm steps; a 0.1-edge amplitude on a 3 mm edge is 0.3 mm, so it is below the step: **not measured on a real scale**), a remesh, a re-sculpt, and a mesh with creases (the residual `σ` grows). **Open before shipping:** a render review of the shading noise (the displacement tilts normals by about `atan(amp/edge)`, 5.7 degrees at 0.1) reviewed by a maintainer on a licence-clean test avatar (never a private character), and a test on a real VRM mesh.

## 6. Method and licence-clean test plan

All inputs are made by script: no third-party avatar, no private character, no recording of a person.

- **Textures:** `synth(n, kind, seed)`: sums of 3 octaves of value noise, plus stripes (kind 1) or 60 random discs (kind 2), plus fine Gaussian grain, at 1024, 2048 and 4096 px. Seeds 1784, 1785, 1786.
- **Meshes:** an icosphere (subdivision 6, 40 962 vertices) times low-frequency bump functions; a harder variant adds wrinkles at 0.3 and 1.0 edge lengths.
- **Edits (Pillow 12.3 and numpy 2.5.3):** PNG round trip; JPEG q90 and q75 (Pillow's libjpeg quantisation tables, 4:2:0); resize 50 percent (Lanczos and bilinear) and 25 percent; Gaussian blur σ=1; `×1.1 + 8` tone; repaint a 45×45 percent block (≈20 percent of the area) in flat green; crop a 12.5 percent border; horizontal flip. Mesh: float32 round trip; 14, 12 and 11-bit bounding-box quantisation; random vertex and index permutation; Gaussian noise 0.02 edge; Laplacian smoothing; vertex-clustering decimation to 50 percent.
- **Nulls:** the unmarked texture with the right key and the marked texture with a wrong key: matches 26 to 38 of 64, as chance predicts.
- **The CI test that ships with the real code** (when SE-07 builds it): a seeded synthetic texture, embed, then each edit above, with the bit-match counts above as golden thresholds (≥56 of 64 for the survives list, ≤44 for crop and flip); property test: a wrong key never reaches 56 over 1000 keys. No image from outside the repo.
- **Not run here and needed:** a real Blender and UniVRM round trip on a generated avatar (the rig is not available here: Blender runs headless on a maintainer's machine only), and a render review of the mesh mark.

## 7. Licences and the register

| Item | Verdict | Evidence |
|---|---|---|
| **Our own texture mark** (the canonical-grid spread spectrum above) | `yes`: written by us, no dependency except `blake3` (already `yes`) | the 151-line spike, std only |
| `invisible-watermark` (ShieldMnt) | `yes` licence (MIT, read), but Python and not shipped; the algorithm family is free to reimplement | [`LICENSE`](https://raw.githubusercontent.com/ShieldMnt/invisible-watermark/main/LICENSE), retrieved 2026-10-13 |
| UniVRM | MIT, read ("Copyright (c) 2020 VRM Consortium") | [`LICENSE.txt`](https://raw.githubusercontent.com/vrm-c/UniVRM/master/LICENSE.txt); a tool for the round-trip test, not shipped |
| Spectral mesh watermark code (Ohbuchi et al.) and other academic mesh code | **`unverified`, so treated as `NO`** | no code or licence was read; do not use |
| Neural or learned watermarks (Stable Signature, tree-ring and similar) | out of scope here: they need a trained model, and no licence for any of them was read | `unverified` |
| Patents (spread-spectrum and mesh-mark families) | open for legal review | n/a |

One register row is added: our own mark, so the next reader finds it (`docs/licensing/register.md`).

## 9. What the follow-up must do

1. **Papers (partly done 2026-10-13):** read Chen and Wornell 2001 and Ohbuchi et al. 2002 (both confirmed); the Cox 1997 paper itself and the blind mesh-watermark survey literature are still to read. Original list: the papers the research environment could not reach, and fill the `secondary` cells: Cox et al. 1997 (DOI 10.1109/83.650120); Chen and Wornell 2001, IEEE Trans. Inf. Theory 47(4); Ohbuchi, Mukaiyama, Takahashi 2002 (DOI 10.1111/1467-8659.t01-1-00597); the histogram- and spectral-domain blind mesh-watermark literature (author list not verified here; read a survey first). Record whether any gives code and under what licence.
2. **Blender manual and UniVRM (partly done):** UniVRM exporter source read (section 4); the Blender manual page is still unreachable (HTTP 403). Original item: read the Blender manual's glTF page (docs.blender.org, unreachable) for the extras, shape-key and image options, and **UniVRM's exporter** source (path not found from here), then fill rows 9 to 11's Blender and UniVRM cells.
3. **Run a real Blender and UniVRM round trip** on a generated, licence-clean test avatar, on both the texture and the mesh mark, and record the bit matches.
4. **Judge the mesh mark by render** (reference, before, after) and **re-measure the texture mark on a real texture** a maintainer approves.

## 10. Follow-ups to file when this lands

- SE-07 implementation task: the texture mark as a `omuzaki-export` function plus a detect op, with the tests of section 6 (`area:export`, P1).
- Research task: mesh-mark viability on a real VRM mesh (render review, Draco quantisation, real decimation and remesh).
- Spec edit (legal review first): §37 "no Omuzaki watermark; creator's own mark allowed".
