---
title: "Layered PSD read and write for the Krita and Photoshop round trip (2026-10-15)"
summary: "Which Rust or permissive C libraries read and write layered PSD (names, groups, blend modes, opacity, masks), and which approach to take: wrap the MIT-licensed ag-psd crate behind our own subset API, with a clean-room writer as the fallback."
area: research
---

# Layered PSD read and write for the Krita and Photoshop round trip (2026-10-15)

Research for #1117 (parent #573, CN-08). Source marks: **verified** = I read the primary source (licence file, manifest, source, official page); **secondary** = someone's summary (forum, bug tracker, search snippet); **unverified** = could not be confirmed. All pages were read on 2026-10-15. No account was made, nothing was downloaded into the repo, and no code was written.

## 1. The answer

- **Recommendation 1: use the `ag-psd` crate (MIT) behind a thin `omuzaki-io-psd` adapter.** It is the only licence-clean Rust crate found that both reads and writes layers, groups, names, opacity, blend modes (including pass-through), clipping and layer masks, in 8, 16 and 32 bit RGB. Through our adapter the round trip is **8-bit only**: the crate decodes editable pixels to RGBA8, so 16 and 32 bit files are read, edited and written back at 8 bit (section 3.1). The adapter exposes only our subset, so the crate can be swapped out.
- **Condition:** `ag-psd` is young (crate created 2026-06-28, v0.3.0 released 2026-09-06), has one maintainer, about 1,500 downloads, and says in its own README that most of it was written by an AI and "has not been hand-audited line by line". So we gate it: our own synthetic fixtures, a read, write, read stability test, a fuzz target on the reader, and a pinned version. It is a derivative of a mature MIT library (`Agamnentzar/ag-psd`, TypeScript, v31) whose test fixtures drive it.
- **Recommendation 2 (fallback): a clean-room reader and writer for our subset**, about 1,500 to 2,500 lines of Rust, because a PSD is a closed product's file format (`docs/licensing/policy.md`, rule 3). One author writes the format spec in the issue from Adobe's public spec and our own synthetic files only. A separate implementer builds from that spec alone and may not read `rawpsd` or other code for it. The issue and PR record who wrote the spec, from which sources, and that the implementer used only the spec. (`rawpsd` is CC0, so a licensed port with notices is the other route; that is a Director choice, see section 9.) Take this path if `ag-psd` fails the gates, goes unmaintained, or the licence review asks for a smaller surface.
- **Not recommended:** the `psd` crate (read only, no layer masks, no blend in flatten, no write), `psd-rs` (a binding to C++, no licence file on GitHub, tiny and unmaintained), `psd_sdk` (C++; limited export; BSD-2 is fine, but it adds a C++ build for little gain).
- **Open fallback exchange format:** OpenRaster (`.ora`) is the cleaner interchange format on paper (PNG per layer, groups, opacity, visibility, blend modes), supported by Krita, GIMP, MyPaint and Scribus, but **not Photoshop** and no layer masks in its layer-stack spec as I read it. Offer it as a second format, not a replacement. Its spec carries no licence statement (see section 6).
- **Decision this supports:** CN-08's "Krita and Photoshop through a layered PSD round trip" is buildable now without writing a PSD codec first. It unlocks the tasks in section 10.
- **One legal caveat that applies to every option, including clean-room:** Adobe's spec says "The software described in this document is furnished under license and may only be used or copied in accordance with the terms of such license." That is the Photoshop software, not the format document, but the commercial-implementation terms were not confirmed. This needs a lawyer's one-time look before release. See section 4.

## 2. Summary table

| Candidate | Licence (verified) | Last release | Layers / groups | Blend modes | Layer masks | Depth | Write | Verdict |
|---|---|---|---|---|---|---|---|---|
| `ag-psd` 0.3.0 | MIT (crate and upstream) | 2026-09-06 | yes / yes | yes, incl. pass-through | yes (read bitmaps, write) | 8, 16, 32 RGB | yes | **Rank 1 (gated)** |
| Own clean-room crate | ours | n/a | planned | planned | planned | 8 first | planned | **Rank 2 (fallback)** |
| `psd` 0.3.5 | MIT/Apache-2.0 | 2024-01-04 | yes / yes | field only, flatten ignores it | no (only transparency channel) | 1, 8, 16, 32 header | no | Reject |
| `rawpsd` 0.2.2 | CC0-1.0 | 2025-05-11 | yes / yes (hierarchy) | yes (as strings) | yes (`image_data_mask`) | 8 only | no | Reference only |
| `psd-rs` 0.3.0 | MIT (crates.io); no file on GitHub | 2025-07-10 | via C++ | unknown | unknown | unknown | `save` | Reject |
| `psd_sdk` (C++) | BSD-2-Clause | pushed 2026-08-11 | yes / yes | not checked | yes (user and vector) | 8, 16, 32 | "limited export" | Reject (C++ build) |
| OpenRaster 0.0.6 | spec: none stated | 2018 | yes / yes | yes (svg: ops) | not in layer-stack spec | PNG | n/a | Second format |

## 3. Candidates

### 3.1 `ag-psd` (Rust, MIT)

**Licence (verified).**
- Manifest: `license = "MIT"` (https://raw.githubusercontent.com/Vasyanator/ag-psd-rs/main/Cargo.toml, 2026-10-15).
- LICENSE file: "The MIT License (MIT) / Copyright (c) 2016 Agamnentzar", plus "This crate (`ag-psd` for Rust) is a from-scratch Rust port derived from the ag-psd TypeScript library ... licensed under the MIT License ... The original copyright (c) 2016 Agamnentzar is preserved." (https://raw.githubusercontent.com/Vasyanator/ag-psd-rs/main/LICENSE, 2026-10-15).
- Upstream TypeScript `ag-psd`: `package.json` `"license": "MIT"`, LICENSE file MIT, same copyright (https://github.com/Agamnentzar/ag-psd, 2026-10-15).
- The LICENSE adds: "Any image or brush files included in this repository are not covered by this license". The crate's manifest has `exclude = ["tests/", ...]`, so the published crate ships no such files. We must not use the upstream fixture images (third-party art).
- Only dependency: `flate2` (MIT OR Apache-2.0, as listed on crates.io; not read in detail here). Rust 1.85, edition 2024.
- GitHub's licence detector says `NOASSERTION` for both repos because of the extra paragraph. The text is plain MIT. Register status `conditions`: keep the notice and the copyright line.

**Maintenance.**
- crates.io: crate created 2026-06-28 (repo the same day); v0.3.0, released 2026-09-06, is the latest (https://crates.io/crates/ag-psd, 2026-10-15). 1,478 total downloads. GitHub: 0 open issues, 7 stars, last push 2026-09-06.
- One maintainer (Vasyanator). The changelog credits 16/32-bit writing to a second fork (`minerva-studio/ag-psd-rs`).
- Quote, README: "The overwhelming majority of this code was written by **Claude** (Anthropic) ... it has **not** been hand-audited line by line. Treat it accordingly".
- Quote, README: "It is used in production in a comic typesetting application".
- Upstream TypeScript is mature: v31.0.3, 731 stars, pushed 2026-10-07, 53 open issues.
- Hardening is visible in the 0.3.0 changelog: bounded memory on read (2 GiB default), PackBits bomb guard, iterative tree walk (no stack overflow), and a bugfix to animation descriptors that a round-trip test could not see. That last item is a warning: round-trip tests alone do not prove the bytes are what Photoshop wants.

**What it reads and writes (verified from README, changelog, usage guide; not yet run by us).**
- Layers and groups: "nested layer trees, names, opacity, blend modes, bounds, visibility, clipping, layer masks". A group is a `Layer` with `children`, normally with `BlendMode::PassThrough` (usage guide).
- Masks: `get_layer_mask_image_data` (user mask) and `get_layer_real_mask_image_data` (vector-derived mask) decode bitmaps on demand.
- Pixel data: PackBits/RLE and ZIP on read and write; pixels as RGBA8 (`PixelData`); 8, 16 and 32 bit RGB written (changelog 0.3.0). Grayscale, indexed, bitmap, duotone read but are not re-emitted; CMYK is rejected on read.
- Also: text layers, effects, vector/shape, smart-object metadata, artboards, ICC profile in image resources (the usage guide's tree lists ICC under `image_resources`), companion `.abr`, `.csh`, `.ase`. Photoshop 2026's long-form blend enum names (`BlnM.colorBurn`) are accepted.
- Stubbed or partial: vector gradient and pattern content, linked-file storage, smart-object filter subtree, thumbnail generation on write.
- Upstream warning (README, line "This library does NOT generate new composite canvas based on the layer data"): after we change layers, the stored composite and thumbnail go stale. Photoshop does not need the composite; other apps may show a stale or blank preview. Our writer must write a fresh composite, or none. Photoshop's "Maximize Compatibility" preference does the same job for files Photoshop saves (see section 5).
- I did not run the crate. Whether ICC bytes survive a read, write cycle is not verified (open question).

**Fit with Omuzaki's subset.** High. Everything in section 7 maps onto `Layer`/`LayerAdditionalInfo` fields. The adapter converts to and from our own `LayerStack` type, keeps unknown blocks out, and reports what it drops (section 7). The `PixelData` RGBA8 buffer is 4 bytes per decoded pixel whatever the file depth (a 16-bit RGBA file holds 8 bytes per pixel natively, so this is a quantisation to 8 bit, not extra memory). Compressed file size gives no fixed multiplier, so budget by pixel count times 4, and count any retained raw channels and the input bytes on top. The crate keeps full precision only through its verbatim raw-channel path, which our adapter leaves out; so 16 and 32 bit files lose precision on a read, edit, write cycle. Deep-colour round trip needs raw or full-precision storage in `LayerStack`, which is out of scope for the first version.

**Open questions.**
1. Does a file written by it open without a repair prompt in Photoshop, and with groups and masks intact in Krita? Needs a person with Photoshop to check once (section 10).
2. Fuzz result on the reader (the changelog shows past amplification bugs).
3. Maintainer continuity. Mitigation: pin the version; the adapter keeps a swap path to the clean-room crate.

### 3.2 `psd` (Rust, MIT/Apache-2.0)

**Licence (verified).** Manifest `license = "MIT/Apache-2.0"` (https://raw.githubusercontent.com/chinedufn/psd/master/Cargo.toml, 2026-10-15). Both `LICENSE-MIT` ("Copyright (c) 2018 Chinedu Francis Nwafili") and `LICENSE-APACHE` are in the repo. README ends "## License / MIT". Only dependency: `thiserror`. Status `yes`.

**Maintenance (verified).** crates.io v0.3.5 published 2024-01-04; GitHub tags end at 0.3.4 (2022-08-16); last push 2025-01-30; 33 open issues. Open issue #70 (opened 2026-06-21): "Panic (out-of-bounds slice) parsing a crafted PSD — DoS". Open issues #1 "Writing down PSD files" (2019) and #61 "Ability to save PSD's" (2024). Quote, README: "My approach was to support as much of the PSD spec as I needed".

**What it reads and writes.**
- Reads: layers, groups (`groups()`, `get_group_sub_layers`), name, visibility, opacity, `blend_mode()` field, clipping flag, bounds; header depth enum 1, 8, 16, 32 (`file_header_section.rs`).
- Does not write at all (issues #1, #61; no write API in `lib.rs`).
- Layer masks: I found no layer-mask parsing in `layer.rs` (grep for "mask" shows only the transparency channel and the clipping flag). Mark: **verified by source search, not by running**.
- Blend modes: `flatten_layers_rgba` does not use them. Quote, `lib.rs` TODO: "Take the layer's blend mode into account when blending layers. Right now we just use ONE_MINUS_SRC_ALPHA blending regardless of the layer."
- `rawpsd`'s README says of `psd`: "The `psd` crate's API makes it impossible to figure out the exact layer group hierarchy, so you can only use it on very simple PSDs." (secondary, a competitor's claim.)

**Fit.** Poor. No write, no masks. **Open questions:** none worth chasing.

### 3.3 `rawpsd` (Rust, CC0-1.0)

**Licence (verified).** Manifest `license = "CC0-1.0"`; README: "CC0, and I have no patents on anything here." (https://raw.githubusercontent.com/wareya/rawpsd-rs/main/README.md, 2026-10-15). CC0 is on our allowed list. Status `yes`.

**Maintenance (verified).** v0.2.2, 2025-05-11, one commit burst in May 2025, 0 open issues, 4 stars. Optional `serde`; no other dependency.

**What it reads and writes.**
- Read only. Quote: "rawpsd currently only supports 8-bit RGB, CMYK, and Grayscale PSDs ... It does not yet support the large ment PSB format variant." It "draws a compatibility support line at Photoshop CS6".
- Returns minimally processed layer records, with exact group hierarchy, strings left as strings, and `image_data_rgba`, `image_data_k` and `image_data_mask` fields (README example). So layer masks decode.
- The README itself says its docs "do not document the entire PSD format".

**Fit.** Good as a small, CC0, easy-to-read reference for the clean-room fallback reader. Not enough alone: no write, 8-bit only.

### 3.4 `psd-rs` (Rust wrapper over C++)

**Licence.** crates.io `psd-rs` 0.3.0 says MIT; GitHub reports no licence (`license: null`) and the repo has no LICENSE or README (contents: `.github`, `Cargo.toml`, `src`, ...). It depends on `psd-sys` (MIT on crates.io, "Rust ffi bindings for the psd-cpp library") whose repo has a `build.rs` and no `.gitmodules`; which C++ source it builds, and under what licence, is **unverified**. Status `unverified` (treated as NO).

**Maintenance.** Last push 2025-07-03; 1,771 downloads; no issues, no releases.

**What it reads and writes.** Source shows `Document::new`, `push`, `save(path)` over `psd_document_save`; masks and blend modes not found. **Fit:** reject (unclear licence, C++ build, path-based API).

### 3.5 `psd_sdk` (C++, BSD-2-Clause)

- **Licence (verified):** GitHub reports `BSD-2-Clause` (https://github.com/MolecularMatters/psd_sdk, 2026-10-15). Allowed class.
- **Maintenance:** last push 2026-08-11; 5 open issues.
- **Features (README, quoted):** "Groups; Nested layers; Smart Objects; User and vector masks; Transparency masks ...; 8-bit, 16-bit, and 32-bit data in grayscale and RGB color mode" and "limited export functionality is also supported".
- **Fit:** reads well, writes only a limited set, and costs a C++ toolchain and FFI. No maintained Rust binding with a clear licence exists (see `psd-rs`). If the Rust options fail review, this is the reader of last resort, not the writer.

### 3.6 Other things looked for

- Search for other Rust PSD crates found `ag-psd`, `psd`, `rawpsd`, `psd-rs`; `zune-psd` is mentioned by `rawpsd` as thumbnail-only (secondary). I did not find a second Rust crate that writes layered PSD. Python `psd-tools` (MIT) can write, but it is Python and out of scope.
- GPL sources (Krita, GIMP) were **not read**; only their manuals and public bug trackers.

## 4. Adobe's public specification

| Item | Finding | Source | Mark |
|---|---|---|---|
| Where | https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/ , dated November 2019, copyright 1991 to 2019 | the page, 2026-10-15 | verified |
| Purpose | Written for third parties who want to "read and write the Photoshop native file format" (my paraphrase of the page's intro) | same | verified |
| Terms | Notice: "The information in this document is furnished for informational use only, is subject to change without notice, and should not be construed as a commitment by Adobe Systems Incorporated." and "The software described in this document is furnished under license and may only be used or copied in accordance with the terms of such license." | same | verified |
| Commercial implementation terms | Not stated on the page. An Adobe forum thread (2019) says third-party readers and writers exist (GIMP) but is not a legal source | community.adobe.com "PSD format licensing" | secondary |
| Media type | IANA registers `image/vnd.adobe.photoshop`: "As this is not an open standard, interoperability is left up to the discretion of the developer, Adobe Systems." | https://iana.org/assignments/media-types/image/vnd.adobe.photoshop (via search snippet) | secondary |
| Coverage | Layer and mask information section: layer records, layer mask / adjustment layer data (rectangle, default colour, flags, real flags), blend-mode keys (`norm`, `mul `, `lum `, and so on, 27 modes incl. pass through), channel image data, global layer mask info, tagged blocks | same | verified |

**Is a clean-room reader and writer for our subset realistic?** Yes. Estimate (lines of Rust, with tests excluded):
- header, colour-mode and image-resource skip/pass-through: about 150
- layer records, names (Pascal and Unicode `luni`), flags, blend keys: about 350
- channel data: raw and PackBits RLE read and write, ZIP read via `flate2`: about 300
- layer masks (user mask channel `-2`, rectangle, default colour, flags): about 250
- groups (`lsct` section dividers, open/closed, pass-through): about 200
- composite image and `Lr16`/`Lr32` handling for depth: about 250
- error type, limits, public subset API: about 250
Total about 1,500 to 2,500 lines. The risk is not size; it is the many undocumented edge cases (the Krita manual says PSD "needs to be reverse engineered" and "It is impossible to have any program outside it support it 100%", verified at https://docs.krita.org/en/general_concepts/file_formats/file_psd.html, 2026-10-15). That is why we prefer to start from the existing, fixture-tested `ag-psd`.

The Krita manual also says "doesn't have an official spec online". That contradicts the Adobe page above; the Adobe page exists but is partial. Either way the spec is a reading aid, not a licence grant.

## 5. What survives a Krita and a Photoshop round trip

The first four rows are from the manuals and bug trackers (documentation only; no GPL source was read).

| Feature | Krita (reads and writes PSD) | Photoshop | Our plan | Mark |
|---|---|---|---|---|
| Raster layers, names, order | Manual: "Krita supports loading and saving raster layers, blending modes, layer styles, vector shapes ... text layers, layer groups, and transparency masks from PSD." (Krita Manual 5.3.0, https://docs.krita.org/en/general_concepts/file_formats/file_psd.html, 2026-10-15) | native | import/export | verified |
| Blend modes | Supported per the manual line above; names map to Adobe keys | native | map table; unknown mode becomes `normal` plus a finding | verified (list) |
| Groups | Group saving was added in Krita 2.9.5 ("saving of group layers to PSD", pass-through as a switch on groups; krita.org release post 2015) | pass-through is Photoshop's default group mode | read both; write groups as pass-through or normal | secondary |
| Group semantics differ | Bug 317829 (2013): in Krita a group is composited first, in Photoshop it is "more like tags" with a linear stack | | write group blend explicitly; do not rely on defaults | secondary |
| Pass-through | Bug 339686 reported Krita had no pass-through composite op (resolved duplicate of 185448); later Krita 5.0.6 and 5.2.6 notes mention pass-through groups | | check on the Krita version in the test (open question) | secondary |
| Layer (transparency) masks | The manual lists "transparency masks" for PSD | native | read and write user mask as a greyscale rectangle with default colour | verified (manual) |
| Clipping masks | Draft merge request 1561 (invent.kde.org) improved PSD clipping-layer import; the author notes that on save "clipping still gets removed" | native clipping flag | report clipping as a finding, or flatten into the mask; do not promise it | secondary |
| Filter, fill and adjustment layers, layer styles | Only partly; 5.2.6 fixed "Advanced Export of the image with filter masks or layer styles" | native | out of subset: reported as findings | secondary |
| Text layers, vector shapes | listed as supported in the manual | live text, editable | import the raster fallback and report "text became pixels" | verified (manual); behaviour unverified |
| Smart objects | the manual says nothing | native; embedded content | import only the rendered pixels, report it | unverified |
| 16-bit, ICC | the manual page says nothing about either | native | 8-bit first; keep ICC bytes opaque if the library preserves them | unverified |
| "Maximize Compatibility" (flattened composite stored in the PSD) | n/a | Preference named "Maximize PSD and PSB File Compatibility", options Ask or Never (Adobe performance guide). Without it other apps may show the file blank. Lightroom needs it for multi-layer PSD. A file saved without it needs Save As to get it back | on export write a fresh composite so previews work; on import do not depend on the composite | secondary |
| Krita's own advice | "We recommend using any other file format instead of PSD if possible." and "PSDs can be expected to become very heavy" | | offer `.ora` too | verified |

Photoshop-side pages (helpx.adobe.com) returned HTTP 403 to my fetches. The Photoshop column is therefore from Adobe search snippets and community answers (secondary), not Adobe's primary page. A person with Photoshop should confirm rows marked unverified.

## 6. OpenRaster (`.ora`) as the open fallback

| Item | Finding | Source | Mark |
|---|---|---|---|
| Version and status | "Open Raster Specification version 0.0.6", footer "©2018, create@lists.freedesktop.org" | https://www.openraster.org/ , 2026-10-15 | verified |
| Licence of the spec | **No licence statement** on the home, file-layout or layer-stack pages; only the copyright footer | same | verified (absence) |
| Who supports it | "Applications that support OpenRaster are Krita, GIMP, MyPaint and Scribus." Photoshop is not listed | https://www.openraster.org/ | verified |
| Layer fields | `name`, `x`, `y`, `opacity` (0.0 to 1.0), `visibility` (`visible`/`hidden`), `composite-op` (default `svg:src-over`) | https://www.openraster.org/baseline/layer-stack-spec.html | verified |
| Groups | "The `stack` element describes a group of layers." | same | verified |
| Blend modes | `svg:` ops: multiply, screen, overlay, darken, lighten, color-dodge, color-burn, hard-light, soft-light, difference, color, luminosity, hue, saturation, plus; Porter-Duff ops | same | verified |
| Layer masks | not mentioned in the layer-stack spec (a Krita-specific extension may exist, not checked) | same | verified (absence) |
| Mandatory merged image | `mergedimage.png` mandatory since 0.0.2 | https://www.openraster.org/baseline/file-layout-spec.html | verified |
| Krita's view | ".ora ... is an interchange format", each layer is a PNG, "making it fully open and easy to support" | https://docs.krita.org/en/general_concepts/file_formats/file_ora.html | verified |

Because there is no spec licence, the format is a ZIP of PNGs and one XML file, and we would write our own code from the spec page, the risk is low. But it is not cleared by a licence text, so register status stays `unverified` until the Director rules on whether a missing licence on a public spec is acceptable (the code would be ours either way). Cost: about 300 to 500 lines, using the `zip` and `png` crates that are already common. It cannot reach Photoshop users.

## 7. The subset Omuzaki needs (proposed)

**Imported (8-bit RGBA first):**
- raster layers: name, order, visibility, opacity (0 to 255), blend mode from the 27 Adobe keys, offset and size
- one layer mask per layer: greyscale, rectangle, default colour 0 or 255, "mask disabled" flag
- groups: nesting, name, open/closed, visibility, opacity, blend mode, including pass-through
- document: width, height, depth (8 now; 16 later), colour mode RGB

**Reported as a finding, not imported** (each finding names the layer and the thing lost):
- text layers: imported as their raster pixels; "text became pixels"
- smart objects: rendered pixels only
- adjustment, fill and filter layers, layer styles/effects, vector masks and shapes, clipping masks (until proven), blending ranges, channels beyond RGBA and the layer mask
- CMYK, Lab, indexed, duotone, multichannel: refused with a clear error; PSB (large document): refused or later
- unknown blend mode: `normal` plus a finding
- images over a size limit: refused before decoding (the memory limit lesson from both libraries)

**Written:** the same subset, plus a fresh composite image so previews work in other tools.

**Safety:** all decoding goes through a byte budget (bytes, like `ag-psd`'s `total_memory_limit`); read the structure first, check sizes, then decode layers one at a time.

## 8. Licence register rows (for the PR that follows)

This note does not edit `docs/licensing/` (the repo's register is generated; the rows go in `docs/licensing/deps/` in the implementing task). Proposed content:

| Item | Kind | Status | Condition |
|---|---|---|---|
| `ag-psd` | crate | `conditions` | MIT; keep the notice with "Copyright (c) 2016 Agamnentzar"; do not use upstream fixture images; AI-written, so gated by our fixtures and fuzzing |
| `psd` | crate | `yes` (not used) | MIT/Apache-2.0; rejected on features, not licence |
| `rawpsd` | crate | `yes` (reference only) | CC0-1.0 |
| `psd-rs` / `psd-sys` | crate | `unverified` | no licence file on GitHub; vendored C++ source unknown |
| `psd_sdk` | tool (C++) | `conditions` (not used) | BSD-2-Clause |
| Adobe PSD spec | spec | `unverified` | page says "The software described in this document is furnished under license ..."; commercial implementation terms not confirmed; lawyer check before release |
| OpenRaster spec 0.0.6 | spec | `unverified` | no licence statement found; formats of this kind are normally implementable; Director to rule |

## 9. Open questions and what settles them

1. **Adobe terms for implementing PSD commercially.** Settled by the release-checklist legal review (the same one that covers the other items in the policy). Fallback if the answer is no: ship `.ora` only and drop Photoshop from CN-08's claim.
2. **Does `ag-psd` output open cleanly in Photoshop?** A one-time manual check by someone with Photoshop, using synthetic files only. Until then the Photoshop column in section 5 stays secondary.
3. **Does `ag-psd` survive fuzzing and keep ICC bytes?** Settled by the fuzz target and the ICC test in task T2 below.
4. **Pass-through groups and masks in the current Krita.** Settled by loading our synthetic file in the current stable Krita and reading the layer panel (no source reading).
5. **Is a missing licence on the OpenRaster spec acceptable?** Director ruling.
6. **Maintainer continuity of `ag-psd`.** Revisit at each release; the adapter keeps the swap path open.
7. **How does a layer stack reach the registry?** `Importer` returns `IoScene` and `Exporter` takes `&IoScene`, with no layer type. Either `omuzaki-io-ops` depends on `omuzaki-io-psd` directly (no API change; recommended for v1), or a separate layer-stack contract is proposed and approved first. Director ruling; T1 and T4 wait on it.
8. **Fallback route:** a clean-room build (spec author and implementer split) or a licensed port of `rawpsd` with notices. Director ruling, with the lawyer's look from section 4.

## 10. Tasks this unlocks

Fixtures are synthetic and generated by us in code (solid and gradient rectangles, a mask, a group of two layers, every blend mode once). No third-party art, no character art, and no test clips.

| ID | Task | Fence | Size | Model |
|---|---|---|---|---|
| T1 | `omuzaki-io-psd` skeleton: `LayerStack` types, subset mapping table, findings list. It does **not** register with `omuzaki-io-api`: that registry's `Importer` and `Exporter` carry `IoScene`, which has no layers (see section 9, question 7) | new crate `crates/omuzaki-io-psd`, its Cargo.toml and docs | about 400 lines | Sonnet |
| T2 | `ag-psd` adapter: read, write, byte budget (covers decoded pixels, retained raw channels and input bytes), ICC test, fuzz target | same crate, `src/adapter.rs`, `fuzz/` | about 500 lines | Sonnet |
| T3 | Synthetic fixture generator and golden round-trip tests (read, write, read stable; findings reported) | same crate, `tests/`, a fixture-builder test helper | about 300 lines | Haiku |
| T4 | Import and export ops that use the format (layers become parts or texture layers; export from a texture) | `omuzaki-io-ops` PSD ops module (the op layer for `io.*`, per `docs/design/io-api.md`; `omuzaki-ops` is the generic executor and must not depend on `omuzaki-io-psd`) | about 400 lines | Sonnet |
| T5 | Manual interop check list for Krita and Photoshop with synthetic files, recorded as a short doc | `docs/` page | about 80 lines | Haiku (a person runs it) |
| T6 | `.ora` read and write (second format) | same crate, `src/ora.rs` | about 400 lines | Haiku |
| T7 | Clean-room fallback reader/writer, only if T2 fails review | new crate, own fence | about 2,000 lines | Opus or Sonnet |
| T8 | Licence-register rows (section 8) | `docs/licensing/deps/` | about 7 small files | Haiku |

## Sources (all read 2026-10-15)

- https://crates.io/crates/ag-psd , https://github.com/Vasyanator/ag-psd-rs (Cargo.toml, LICENSE, README.md, CHANGELOG.md, docs/usage.md)
- https://github.com/Agamnentzar/ag-psd (LICENSE, package.json, README.md)
- https://github.com/chinedufn/psd (Cargo.toml, LICENSE-MIT, README.md, src/lib.rs, issues)
- https://github.com/wareya/rawpsd-rs (Cargo.toml, README.md)
- https://github.com/weqeqq/psd-rs , https://github.com/weqeqq/psd-sys , https://github.com/MolecularMatters/psd_sdk
- https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/
- https://docs.krita.org/en/general_concepts/file_formats/file_psd.html , https://docs.krita.org/en/general_concepts/file_formats/file_ora.html
- https://www.openraster.org/ , https://www.openraster.org/baseline/layer-stack-spec.html , https://www.openraster.org/baseline/file-layout-spec.html
- Secondary: bugs.kde.org 317829 and 339686, invent.kde.org krita MR 1561, krita.org "Krita 2.9.5 Released", Adobe Photoshop performance guide and Lightroom help (via search), Adobe Community threads on Maximize Compatibility, IANA media type entry.
