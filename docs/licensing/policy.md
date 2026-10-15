---
title: "Licence policy"
summary: "Licence policy"
area: licensing
---

# Licence policy

**The rule: nothing non-commercial, ever.** No library, tool, model, dataset or asset with a non-commercial term, in the product or in the pipeline that builds it.

This page says what's allowed and how to add anything new. `register.md` lists what we use, with verdicts and conditions. The evidence is in `docs/research/2026-10-licence-audit.md`. This is an engineering policy, not legal advice; the items that need a lawyer are listed in the release checklist below.

## Scope

Everything the product uses or ships, and everything we use to build it:
- code: Rust crates, JS packages, native libraries
- tools and services, including services whose outputs we keep
- models: the weights, the training data behind them and their runtime dependencies
- datasets and assets: fonts, HDRIs, meshes, textures, audio, video, reference clips

## Verdicts

| Verdict | Meaning |
|---|---|
| `yes` | Commercial use is fine; keep the notice. |
| `conditions` | Fine if the recorded conditions are met (notice files, dynamic linking, attribution, flow-down terms). Conditions are automated where possible. |
| `external` | A GPL tool used only as a separate program (Blender, ComfyUI, v4l2loopback): never bundled, linked or redistributed. |
| `NO` | Banned. |
| `unverified` | **Not cleared, so treated as `NO`** until a primary source or legal review settles it. |
| `private-test` | Material that is not licensed for the product and is used only in private tests; never committed or shipped. |

## Allowed

**Code linked into the product** (where a licence offers a choice, we take the permissive option):
- MIT, MIT-0, Apache-2.0, Apache-2.0 WITH LLVM-exception
- BSD-2-Clause, BSD-3-Clause, ISC, Zlib, BSL-1.0, 0BSD, Unlicense, CC0-1.0
- Unicode-3.0, Unicode-DFS-2016
- MPL-2.0. Its copyleft is file-level, so we keep those crates unmodified or publish our changes to those files.

**Native libraries under LGPL** (FFmpeg's default build, librtmp):
- shared libraries only, never statically linked
- the user may relink them
- we keep a source offer, build notes and the licence text

**Fonts:**
- OFL-1.1: never sold on its own; ship the notice
- the Ubuntu Font Licence and Bitstream Vera, which egui's default fonts use; ship their texts

**Assets and data:**
- CC0 and public domain
- CC BY, with the attribution recorded in provenance

**Case by case, recorded with their conditions** (legal review reads them before the first release):
- OpenRAIL-style model licences such as SDXL. Their use restrictions flow down to our users.
- Proprietary runtimes and SDKs with a redistribution grant: NDI, CUDA / cuDNN / TensorRT runtime files, DirectML, and the NVIDIA Open Model License.

## Banned

- **Any non-commercial or research-only term:**
  - CC BY-NC (any variant)
  - "research only", "personal use", "non-profit"
  - the NVIDIA Source Code License (nvdiffrast, nvdiffrec)
  - the Inria Gaussian Splatting licence
  - the FLUX.1 [dev] licence
  - the Basel Face Model licence
- **Models trained on non-commercial data**, unless the model's authors clear commercial use. Examples of such data: WIDER FACE, 300-W, MPIIGaze. This rules out OpenSeeFace and YuNet.
- **GPL or AGPL code linked into the product,** statically or dynamically, including through a dependency. Examples:
  - espeak-ng via `sherpa-onnx`
  - `phonemizer`
  - `pykakasi`
  - GPL or nonfree FFmpeg and GStreamer builds
- **Restricted "source-available" licences:** SSPL, Business Source (BUSL), Functional Source (FSL), Commons Clause, Elastic. FSL is banned in the build and release pipeline as well as the product (2026-10-13: `sentry-cli`).
- **Service outputs the vendor owns or keeps rights to** (the free tiers of Meshy and Tripo), or outputs under unknown terms.
- **Third-party IP in anything shippable:** characters, logos, a real person's likeness or voice, unless licensed and consented.

## What counts as documented lineage

A shipped model's `model.toml` lists its chain under `card.lineage`, and `cargo xtask models` checks it (ML-01):

- **At least one `base_model` or `training_data` link.** A model trained from scratch names its dataset; a fine-tune names its base model and the data it was tuned on. `code` and `conversion` links alone don't document where a model came from.
- **Every link has a non-blank `name` and a source `url`** (`https://`, a valid host, an optional port) **and its licence** (below). Fields are read exactly as written: leading or trailing whitespace is not ignored.
- **The licence must be exactly one entry of the allow-list.** The gate accepts only these whole entries, from the "Allowed" section above: `MIT`, `MIT-0`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `BSL-1.0`, `0BSD`, `Unlicense`, `CC0-1.0`, `Unicode-3.0`, `Unicode-DFS-2016`, `MPL-2.0`, `OFL-1.1`, `CC-BY-3.0`, `CC-BY-4.0`. It doesn't parse expressions: `OR`, `AND`, parentheses and a bare exception id all fail, and a dual-licensed source names the one licence we take it under. Anything else fails, naming the model and the link: a banned licence (`CC-BY-NC-4.0`, `BUSL-1.1`, `SSPL-1.0`, `FSL-1.1-MIT`), words (`research only`), `LicenseRef-…`, a placeholder such as `unknown`, or an id nobody has cleared. Case-by-case licences (OpenRAIL, the NVIDIA Open Model License) are not on the list; a lineage link under one needs a maintainer to add it with its conditions.
- A link whose licence isn't on the list isn't cleared: find the primary source (step 1 under "Adding anything new") or the model doesn't ship.

## User-supplied models

A model a person adds to a model slot (ML-13, `docs/decisions/0016-user-supplied-models.md`) is outside this policy's scope: we don't make, ship, bundle, mirror or auto-download it, so its licence is between the person and its author. The product shows its declared licence and a "not from Omuzaki" disclaimer, records the person's acknowledgement, carries the model into the licence record of everything it makes, and warns at export when a non-commercial or unknown licence would reach a commercial use. Nothing we ship may depend on a user-supplied model, and no default, preset, tutorial or agent suggestion may point people to a model this policy bans.

## User-supplied avatars

An avatar a person imports (VRM, glTF or GLB) is theirs and sits outside the register. We never bundle, mirror or fetch one, and nothing we ship may depend on one.

Its terms are the VRM `meta` fields: avatar permission, commercial use, credit, modification and redistribution. We record them at import, show them with any warnings, and apply them at export ([io-api.md](../design/io-api.md) D5 and D10; [byom.md](../design/byom.md) §3 D5). Warnings never block performing on the person's own machine. The licence gate applies at export.

A third-party avatar never enters the repository, a fixture, a golden or PR evidence. A test that needs one reads it from a folder outside the repo, named by the `OMUZAKI_BYOM_DIR` environment variable.

## GPL tools as separate programs

Blender, ComfyUI and v4l2loopback are fine to use as separate programs. The user installs them, or we run them as tools. The product only talks to them over files, sockets or the command line.

- **Scripts that import `bpy`** are GPL if distributed. Keep them as internal tooling (`tools/`), never in the product.
- **The OBS plugin** (BR-03) ships as a separate GPL-licensed project in its own repository and download, talking to the app over IPC and never linked into it. It ships (decided 2026-10-11); legal review reviews the boundary.

## Private test material

Material that is not licensed for the product (for example a third party's character or a recording of a person) is never committed, shipped, shown or used as training data.

## Ports, and interop with closed products (added 2026-10-12)

How our code may relate to other people's code, from most to least common:

1. **Original code** is the default: written from public specifications (glTF, VRM, the ARKit blendshape names), our own designs and our own prototype.
2. **Ports of permissively licensed code** are allowed. The ported files carry the upstream notice (see `crates/omuzaki-face/src/raster/NOTICES.md`), and the register gets a row. Code validated against another project's *output* without porting its source (for example, spring bones checked against three-vrm's trajectories) says so in a comment and needs no row; generated reference data names its tool, version and licence in a header.
3. **Interop with a closed product** (the iPhone tracker protocols, a VTube Studio-style plugin API, an app's private file format) is built **clean-room**:
   - **A spec author** writes the protocol or format spec in the issue, only from public docs and our own captures (for example, packets recorded from a contributor's own phone). They never read the product's code, decompiled binaries or leaked material.
   - **A separate implementer** builds only from that spec.
   - The issue and the PR record who wrote the spec, from which sources, and that the implementer used only the spec.
4. **Never:** copying code from a non-permissive or unknown-licence source, decompiling or reverse-engineering binaries beyond observing their network or file output, or tracing third-party character art into shipped design data (face profiles, hair, proportions, palettes). Measure our own designs instead.

**Before any public or commercial release,** a provenance scan (ScanCode or similar) runs over the source and its findings are resolved.

## Adding anything new

1. **Read the licence at the primary source:** the LICENSE file, the model card or the service terms. For a model, check three things: the weights licence, the training data and the runtime dependencies.
2. **Add or update its file in `docs/licensing/deps/` in the same PR.** If a verdict needs judgement, research it and record the sources.
3. **For crates, `cargo deny check` must pass** (`deny.toml`). A change to `deny.toml` (allow list, exceptions, bans) needs a maintainer's review, with a register row.
4. **Nothing `unverified` merges into a shippable path.**
5. **For services, record the plan tier and the output-ownership clause** in the asset's provenance.

## Enforcement

- **Every PR:** `cargo deny check` (licences, bans, sources), using `deny.toml`.
- **Repo licence gate**: `cargo xtask lint-licences` checks that every direct dependency, CI tool and committed asset has a file in `docs/licensing/deps/`, and generates the register from them. A file claims assets by exact path (a folder claim is a problem, so a new asset needs an audit), and only an item of kind `tool` may be `external`. Release builds generate the third-party notices.
- **In the product:** the licence gate (spec §7, §15) blocks `publish` of anything whose commercial status is `no` or `unknown`.
- **Release checklist:**
  - the notices bundle
  - NDI attribution and trademark lines
  - the FFmpeg source offer
  - legal review sign-offs
