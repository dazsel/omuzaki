---
title: "`omuzaki-io-api`: the glTF / VRM import and export contract"
summary: "the glTF / VRM import and export contract"
area: design
---

# `omuzaki-io-api`: the glTF / VRM import and export contract

**Status:** proposal. **Spec:** v3 §9 (`omuzaki-io`), §27 (EX-01, EX-02), §19 (FE-09), §7 (provenance), §39.
**Code:** `crates/omuzaki-io-api` (the contract), `crates/omuzaki-io-mock` (a small working GLB / VRM 1.0 implementation for tests).
**Read on 2026-10-11** (specs change; check dates): the glTF 2.0 and VRM texts cited in §9.

This page explains the decisions, so a format implementer or a reviewer can disagree with a reason. The types are documented in the code; this page doesn't repeat them.

## 1. What the contract covers

| Issue asked for | Where it is |
|---|---|
| Importer and exporter traits that self-register by format | `Importer`, `Exporter`, `register_importer!`, `register_exporter!`, `sniff`, `formats()` (`format.rs`, `io.rs`) |
| The intermediate scene | `IoScene` and friends (`scene/`) |
| VRM meta and licence fields mapped to the provenance record (§7) | `VrmMeta`, `Vrm0Meta`, `LicenceEvidence`, `imported_provenance`, the rights rules (`meta/`, `licence.rs`, `check/rights.rs`) |
| Perfect-sync blendshape baking on export | `ARKIT_52`, `PerfectSync`, `ExpressionBaker`, `BakeReport`, completeness rules (`perfect_sync.rs`) |
| Validation reports | `Report`, `Finding`, `Severity`, `IoScene::check`, `preflight`, `vrm_profile` (`report.rs`, `check/`) |
| A mock that round-trips a tiny glTF made by the test itself | `omuzaki-io-mock`: `fixtures::tiny_glb` (hand-assembled bytes), `tiny_avatar`, the `glb` and `vrm1` formats, `MockBaker` |

Not covered, on purpose: any real parsing (`omuzaki-io-gltf`, `omuzaki-io-vrm`), FBX, PNGTuber, Inochi2D, sprite sheets, VRChat/Unity packages (EX-03 to EX-09), VRMA animation import (RG-08), and the ops that drive all this (§5). Animation *export* (EX-16) is D11 and `docs/design/vrma-export.md`.

## 2. The shape of it

```
 bytes ──► Importer ──► IoScene ◄── bake ◄── ExpressionBaker (face side)      character / face / look
 (host reads files,     │  ▲                                                   (runtime types)
  checks `fs`)          │  │  omuzaki-io-ops (op layer): the only code that
 run_import adds        │  │  knows both IoScene and the runtime types
 limits, invariants,    ▼  │
 provenance, notes     scene ──► run_export: preflight (scene, rights, perfect sync)
                                  + Exporter::check ──► Exporter ──► OutputFile[] ──► host writes (checks `publish`)
```

Platform exports (VRChat, Roblox, cluster, Resonite, engine packages) sit in the op layer, between gather and `run_export`: a profile reshapes and fits the `IoScene` to a limit table and then names one exporter (`docs/design/platform-export.md`, ADR 0047).

Format crates (`omuzaki-io-gltf`, `omuzaki-io-vrm`) depend on `omuzaki-io-api` and on the shared format library `omuzaki-gltf` (a lib crate, ADR 0011). They parse and write bytes; they never see a character, a renderer or a file system.

## 3. Decisions

### D1. The scene is its own model, not the character or render types

`IoScene` is shaped like glTF: arenas of nodes, meshes, skins, materials, textures and images, linked by typed indices (`NodeIx`, `MeshIx`, …).

- **Why not reuse `omuzaki-char-api` / `omuzaki-render-api` types.** They are runtime models for editing and 1.5 ms frames. A file model must also carry what the runtime has no use for: the thumbnail, `thirdPartyLicenses`, morph target names, collider groups, `isBinary`. It must also not change when the renderer does. And the char, face and anim contracts are still being designed, so depending on them would serialise the work (render-api has merged; the mesh it put in `omuzaki-core` is shared, below).
- **What is shared.** The CPU mesh is the shared `MeshData` (positions, normals, UVs, vertex colours, four joints and weights, a triangle list), which the renderer, the character kit and the importers all speak, so an imported primitive becomes a drawable one by moving it, not converting it. It lives in `omuzaki-core` (`omuzaki_core::mesh`, from the render-api contract), and `omuzaki_io_api::MeshData` is a re-export. `IoScene::check` keeps its finer, located rules and falls back to `MeshData::validate` for anything they miss (`scene.mesh_invalid`), so the definition of a valid mesh stays in one place. A `Primitive` is `MeshData` + material + morph targets (render-api has no morph targets in v1; here they carry expression bakes and file fidelity).
- **What that forces on importers.** The shared mesh always has normals and indices, glTF lets a file omit both. Importers fill them in: `Primitive::generate_flat_normals` (one shared implementation, as glTF 2.0 requires of clients; vertices stop being shared, morph deltas follow) and sequential indices, each with an info finding.
- **Colours stay linear `f32` (`glam`), not `Colour`.** glTF and MToon store linear float factors; keeping them lets an import/export round trip be exact, where an 8-bit sRGB `Colour` would nudge every value by up to 1/255 and re-exporting a bought model would no longer reproduce its numbers. `Colour` is the palette type; the op layer converts at the boundary (`Colour::from_linear` / `to_linear`). See Q10.
- **Textures stay encoded (`EncodedImage`).** Original PNG/JPEG bytes round-trip bit-identically and hash the same. PNG and JPEG decode into the renderer's pixels and `TextureDesc` lives in the format crates / `omuzaki-io-ops`, as the render-api doc says. The type is named `EncodedImage` so it can't be confused with render-api's decoded `Image`.
- **Cost.** `Toon` mirrors `VRMC_materials_mtoon` field for field, and render-api's `Toon` is smaller (shade, shift, softness, rim, emissive, skin): MToon's GI equalisation, matcap, UV animation and render-queue offset have no renderer equivalent in v1, but a faithful round trip needs them. Conversion is a pure function in the op layer (§5); the fields the renderer can't use become an info finding. If it hurts, unifying is a later, deliberate API change; the other way round (splitting a shared type) would be worse. The gap is a tracked feature, not a permanent one: LK-07 extends the renderer to full MToon 1.0 (matcap, GI equalisation, UV animation, render-queue offset, outline width modes) in phase 2, and the info finding then disappears.
- **N+N, not N×N.** FBX, PNGTuber and future formats each add one importer or one exporter against the same scene.

### D2. The scene is always in glTF 2.0 / VRM 1.0 space

Right-handed, +Y up, the model's front facing **+Z**, metres, radians, unit quaternions, linear colour factors (glTF 2.0, "Coordinate System and Units"). VRM 0.x models face −Z, so the 0.x importer rotates them 180° about Y on the way in (`x, z → −x, −z` on translations and vertex data) and the 0.x exporter rotates back. UniVRM's migrator does exactly this (`RotateY180`) and refuses non-identity bone rotations, because 0.x requires bones without local rotation. Nothing downstream ever sees a 0.x orientation.

### D3. Importers and exporters are pure; the host does the I/O

Importer: bytes in, scene out. Exporter: scene in, `OutputFile`s out. Sidecar files (`.gltf` + `.bin`) come through a `Resolver` the host supplies; the host checks the `fs` grant there and the `publish` grant before writing.
The sidecar `Resolver` is confined to the folder of the file it reads: `io.inspect` is the first op to use that, and `io.import` is to share it.

- **Safe on untrusted input**, testable with no disk, runnable in the web build.
- **Deterministic:** the same scene and options give byte-identical files (the exporter writes no time or version of its own; the host passes `generator`). The content-addressed store then dedups exports, and golden tests can compare bytes. The mock test `exports_are_deterministic_and_canonical` also checks `export(import(export(x))) == export(x)`.
- **Limits before allocation.** `Limits` caps input size, nodes, vertices and images. Importers check *declared* sizes (accessor counts, buffer lengths) before allocating; `run_import` re-checks the result.

### D4. Self-registration per format; `run_import` / `run_export` enforce the contract

Registration mirrors `register_op!` (inventory, no central list, §39 rule 2): `register_importer!(T)` and `register_exporter!(T)` with `T: Default`. A format id is a slug (`glb`, `vrm1`, `vrm0`). Importers and exporters register separately (FBX is import-only, PNGTuber export-only); `formats()` merges them for `io.list_formats`. Plain glTF is **one** registration, id `glb`, extensions `glb` and `gltf`, for both containers: the importer reads either, and the exporter's `Layout` picks one GLB or a `.gltf` with its sidecar files (`omuzaki-io-gltf`). `omuzaki-io-mock` registers `glb` too, as its oracle; a binary linking both gets whichever the linker collected first from `find_importer("glb")`, and nothing links the mock outside tests.

`Importer::sniff` returns `No / Possible / Certain`, so the op layer can pick a format for a dropped file: a GLB containing `VRMC_vrm` is `Certain` for `vrm1` and `Possible` for `glb`.

Callers use `run_import` and `run_export`, never the trait methods directly. They add what no format should repeat or forget:

| Entry point | Adds |
|---|---|
| `run_import` | size limits; `IoScene::check` on the result (an importer bug is rejected here with a rule id and location instead of crashing a consumer); provenance derived from the file's meta if the importer set none; notes about the file's licence, chosen by the importer's `LicenceEvidence` (below); strict mode |
| `run_export` | `preflight` (scene invariants, rights, perfect-sync completeness) then `Exporter::check`; nothing is written if any error (or any warning in strict mode); one merged report |

### D5. Rights: the file's facts and Omuzaki's record are kept apart

`VrmMeta` is **exactly the shape of `VRMC_vrm.meta`** (VRM 1.0 `meta.md`): it serialises to and from that JSON object, so 1.0 meta is one `serde_json` call, and it is the scene's vocabulary for "what the file grants". `Vrm0Meta` is the 0.x object (with its `…UssageName` typos) and converts both ways. `Provenance` (from `omuzaki-store-api`, merged) is what the licence gate reads. The scene carries both: `meta` and `provenance: Option<Provenance>`.

The derivation file → record is **conservative**: anything the file doesn't settle is `Commercial::Unknown`, which the gate treats as `No` (§40, "unverified is banned"). A person can correct the record after reading the licence.

**Import: VRM 1.0 meta → `Provenance`** (`VrmMeta::imported_provenance`)

| File says | Record |
|---|---|
| `licenseUrl` is a VRM Public License 1.0 page (`https://vrm.dev/licenses/1.0/` or `…/en/`) | `licence = LicenseRef-VRM-Public-License-1.0` |
| `commercialUsage` = `personalNonProfit` | `commercial = No` |
| `commercialUsage` = `personalProfit` or `corporation` | `commercial = Ok` (see Q3) |
| any other `licenseUrl` | `licence = LicenseRef-VRM-Other`, `commercial = Unknown`, warning `rights.unrecognised_license_url` |
| no licence information at all (`LicenceEvidence::Absent`: a plain glTF) | `licence = LicenseRef-Unknown`, `commercial = Unknown`, info `rights.no_license_info` |
| licence data the importer left behind (`LicenceEvidence::PresentNotImported`: a VRM read as plain glTF) | same record (the importer read none of it), but warning `rights.license_not_imported` quoting the data and naming the importer that keeps it (`vrm0` / `vrm1`); **never** `rights.no_license_info` |
| `authors` | `author` = authors joined with `, ` |
| `creditNotation` = `required` | `attribution` = "name - copyright holder (or authors)" |
| (nothing in a VRM maps to it) | `consent = None`, `private_test = false`; only the user marks private material |
| `allowRedistribution = false` or `modification = prohibited` | info `rights.import_restricted`: use as is to perform, don't include in exports |

**Import: VRM 0.x meta → `Provenance`** (`Vrm0Meta::imported_provenance`, straight from 0.x because the 1.0 shape has already lost `licenseName`)

| 0.x `licenseName` | SPDX / `LicenseRef` | Redistribution | Modification | Credit | `commercial` |
|---|---|---|---|---|---|
| `Redistribution_Prohibited` | `LicenseRef-VRM-Redistribution-Prohibited` | no | prohibited | required | from the avatar flag |
| `CC0` | `CC0-1.0` | yes | yes | no | from the flag |
| `CC_BY` / `CC_BY_SA` | `CC-BY-4.0` / `CC-BY-SA-4.0` | yes | yes | required | from the flag |
| `CC_BY_ND` | `CC-BY-ND-4.0` | yes | no | required | from the flag |
| `CC_BY_NC`, `CC_BY_NC_SA`, `CC_BY_NC_ND` | `CC-BY-NC-4.0`, … | yes | by variant | required | **`No` whatever the flag says** (policy: nothing non-commercial) |
| `Other` or missing | `LicenseRef-VRM-Other` / `LicenseRef-Unknown` | no | no | required | `Unknown` (the real terms are at a URL we can't evaluate) |

"From the flag" = `commercialUssageName`: `Allow` is `Ok`, `Disallow` or missing is `No`. The CC links are the ones the 0.x spec attaches to each name (4.0 deeds).

**Import: other 0.x → 1.0 field mapping** (`Vrm0Meta::to_meta`; the avatar flags follow UniVRM's migrator)

| 0.x | 1.0 |
|---|---|
| `title`, `version`, `author`, `contactInformation`, `reference` | `name`, `version`, `authors[0]`, `contactInformation`, `references[0]` |
| `allowedUserName` `OnlyAuthor` / `ExplicitlyLicensedPerson` / `Everyone` | `onlyAuthor` / `onlySeparatelyLicensedPerson` / `everyone` |
| `violentUssageName`, `sexualUssageName` (`Allow`/`Disallow`) | `allowExcessivelyViolentUsage`, `allowExcessivelySexualUsage` |
| `commercialUssageName` `Allow` / `Disallow` | `personalProfit` / `personalNonProfit` (0.x can't say `corporation`) |
| (none) | `allowPoliticalOrReligiousUsage = false`, `allowAntisocialOrHateUsage = false` |
| `otherLicenseUrl`, `otherPermissionUrl` | `otherLicenseUrl`: the one present, or both joined `'a', 'b'` when they differ (UniVRM's migrator does the same; an export splits them again) |
| any other `meta` key (a vendor's `"noAiTraining": true`) | `VrmMeta::extras_vrm0`, verbatim, written back by a 0.x export; a 1.0 export lists them in `vrm.meta_unknown_keys` |
| `texture` (a *texture* index) | `thumbnailImage` (an *image* index; the importer resolves it) |
| (no licence document) | `licenseUrl` = the 1.0 page, because the flags are now in 1.0 vocabulary |

**Export: what the scene may write.** The scene's `meta.terms` is what the file will say; `preflight` stops it from saying more than Omuzaki holds.

| Rule | Fires when |
|---|---|
| `rights.provenance_missing` | `scene.provenance` is `None` |
| `rights.gate_non_commercial`, `_unknown`, `_private_test`, `_no_consent` | `Purpose::Publish` and `omuzaki_store_api::licence_gate` fails; the message is the gate's own sentence |
| `rights.terms_exceed_provenance` | one error per permission (`allowRedistribution`, `modification`, `commercialUsage`, `avatarPermission`, the content flags, `creditNotation`), at `meta/<field>`, naming its value, when it grants more than the strictest value and the record holds no clearance for it: provenance is not `Ok` (non-commercial, non-profit, `Unknown`) or the record is private test material. Even a private export is refused, because a private file leaks. Exception: for an *imported* file that is not cleared, the file's own terms are the source's and are written back as they were; only `commercialUsage`, the one permission Omuzaki's record settles, is capped there (the model's own sharing terms are the next rule) |
| `rights.private_test_export` (info) | private test material is being exported (allowed with `Purpose::Private` only); the file must never be shared |
| `rights.licence_forbids_redistribution`, `rights.licence_forbids_modification` | `Purpose::Publish` of an *imported* model (`Source::Imported`) whose own terms do not allow sharing it (spec VF-08): `allowRedistribution` is false, or `modification` is not `allowModificationRedistribution` (an export may differ from the original, so redistributing modified copies must be allowed). The error names the field, its value and the creator, at `meta/<field>`. Run in `preflight`, so **every** exporter gets it |
| `rights.private_export_restricted` (info) | the same terms, `Purpose::Private`: allowed, with a note that the file must not be shared |

`Purpose::Private` (default) is a file for the user's own machine, such as trying a VRM in VSeeFace: the gate isn't applied, the terms cap is. `Purpose::Publish` is anything that leaves the machine. The op's `purpose` argument sets it, and `Publish` needs the `publish` grant.

**Export to 0.x** (`Vrm0Meta::from_meta(meta, provenance)`) is lossy and says so, and never grants more than the scene's terms. It picks the most restrictive 0.x licence that is still faithful:

1. The provenance says the model came from a known 0.x `licenseName` (`CC0`, `CC_BY`, ... `Redistribution_Prohibited`): keep that name as long as it grants nothing the terms no longer do. If the terms were narrowed below it, write `Redistribution_Prohibited`. Terms *wider* than the name keep the name and are listed.
2. The licence is `Other` (a 0.x `Other`, or a 1.0 file whose `licenseUrl` is not the VRM Public License): write `Other` with that document.
3. Otherwise (the VRM Public License 1.0, or a model made in Omuzaki): redistribution off is `Redistribution_Prohibited`; redistribution on has no 0.x name, so `Other` pointing at the licence document.

A 1.0 model never becomes `CC0` or `CC_BY` (and a `personalNonProfit` one keeps `commercialUssageName: Disallow`); a `CC_BY_NC*` model keeps its NC name. `an_export_is_never_more_permissive_than_its_source` (in `meta/vrm0.rs`) holds that line over every combination of document, redistribution, modification and credit. What 0.x cannot say is a warning per field, with the field in its location and its value in the message: `vrm0.meta_licence` (any term the chosen name cannot state), `vrm0.meta_commercial` (`corporation`), `vrm0.meta_content_flags` (the political and anti-social flags), `vrm0.meta_third_party_licenses` (the text is appended to `reference`, which has no such field). The table that turns a 0.x name into the 1.0 flags and the SPDX id is public (`Vrm0Licence`), so the two directions share it.

**Keys no typed field names** (`extras`, `extensions` and vendor keys in a 1.0 `meta`; unknown keys in a 0.x one) are kept verbatim in `VrmMeta::extras` (1.0 vocabulary, flattened into the JSON) and `VrmMeta::extras_vrm0` (0.x vocabulary), noted on import (`vrm.meta_unknown_keys`, info) and written back by an exporter of the same version. An exporter of the other version lists them (`vrm.meta_unknown_keys`, warning, via `VrmMeta::unwritable_extras`). An extra never overrides a typed field on the way out.

**`LicenceEvidence`: who says "no licence".** `ImportOutcome::new(scene, licence)` takes the importer's statement of what it found: `Imported` (it filled `meta`; `run_import` describes the terms), `PresentNotImported { summary, hint_format }` (the file has licence data it did not read), or `Absent`. There is no default, so a new importer has to decide. `run_import` reports `rights.no_license_info` for `Absent` only. The VRM importers say `Imported`; `omuzaki-io-gltf` says `PresentNotImported` when it drops a `VRM` or `VRMC_vrm` root extension whose `meta` holds licence fields (and `Absent` when it holds none). This is a deliberate decision: rights reporting is a legal surface, a string rule id is easy to drop silently, and a report that says "no licence" about a file under CC BY-NC is wrong in the direction that matters.

### D6. Perfect sync: custom expressions named like ARKit, baked before export

A model is "perfect sync" when it defines an expression per ARKit blendshape, so face trackers drive it directly. In VRM 1.0 these are **custom expressions** (`expressions.custom`) whose names are Apple's 52 camelCase keys (`jawOpen`, `eyeBlinkLeft`, …). They can never collide with the 18 presets, and VRM 1.0 forbids a custom name equal to a preset (`expressions.md`, "Custom Expressions"). Warudo's model guide says tracker matching is case-sensitive and uses these camelCase names; in VRM 0.x the same strings are `BlendShapeClip` names with `presetName: Unknown`.

**Baking is separate from export.** The exporter writes expressions that are already in the scene. Creating them from a procedurally painted face needs the face painter and the character, so it happens beforehand behind `ExpressionBaker` (implemented on the face side; `MockBaker` makes trivial morph targets). That keeps this crate free of rendering and lets the exporter be tested end to end.

**The baker is found through a registry.** The factory that builds it is registered in `omuzaki-face-api` (`register_face_baker!`, `face_baker()`, decision G13), the same self-registration pattern as importers and exporters. So `omuzaki-io-ops` calls `face_baker()` and never depends on the face implementation, and this crate knows only the `ExpressionBaker` trait.

**Defaults (decided on review):** `PerfectSync::Arkit52` is the default, so an exported avatar drives from any tracker out of the box; extras (`Arkit52WithExtras`) are opt-in. A scene without an avatar has no expressions to carry, so the default asks nothing of it.

The op layer's `io.export` runs: gather the character into a scene → `ExpressionBaker::bake` (when `perfect_sync` isn't `Off`) → `run_export`.

A channel is realised as one or a mix of:

| Realisation | Used for | VRM mechanism |
|---|---|---|
| morph targets | jaw, cheeks, mouth shapes on geometry | `morphTargetBinds` |
| texture-transform | painted eyes, brows, mouths via a face atlas | `textureTransformBinds` (UV offset + scale of every UV-addressed texture of a material) |
| colour | blush levels | `materialColorBinds` |

Binds interpolate linearly with the weight, so a stepped flipbook must also set `isBinary`. The baker chooses; `BakeReport` records what it chose per channel (`Morph`, `Flipbook`, `Colour`, `Mixed`, `Inert { reason }`), for the user and for agents.

**Completeness** (`PerfectSync::Arkit52`): all 52 names must exist (`perfect_sync.missing_channel`, error). A channel that exists but drives nothing is a warning (`perfect_sync.inert_channel`): it is legal, and better than a missing name (trackers complain about missing ones), so a face with no tongue still exports `tongueOut`.

**Extras** (FE-02: `^^` eyes, heart eyes, sweat drop…) export as custom expressions named `omuzaki_<id>` (`extra_expression("heart-eyes")`), so they can't collide with a tracker's names and other apps can ignore them. `PerfectSync::Arkit52WithExtras` asks for them.

**Deriving presets from channels** is the baker's job, so non-perfect-sync apps still work. Proposed starting mapping (art review decides the final values):

| Preset | From channels |
|---|---|
| `blink`, `blinkLeft`, `blinkRight` | `eyeBlinkLeft` + `eyeBlinkRight`, left, right |
| `aa` | `jawOpen` |
| `ih` | `mouthStretch*` + slight `jawOpen` |
| `ou` | `mouthPucker` + `mouthFunnel` |
| `ee` | `mouthSmile*` + `mouthStretch*` |
| `oh` | `mouthFunnel` + `jawOpen` |
| `lookUp/Down/Left/Right` | `eyeLookUp*`, `eyeLookDown*`, `eyeLookOut/In*` (when `lookAt.type` is `expression`) |
| `happy`, `angry`, `sad`, `surprised`, `relaxed` | the named expressions from FE-03 (blends of channels), by name |

Binds with an empty array are omitted by exporters: the VRM schema requires at least one entry when the array is present (`VRMC_vrm.expressions.expression.schema.json`).

### D7. Reports: stable rule ids, locations, hints

`Report { findings: Vec<Finding> }`, each `{ severity, rule, message, location, hint }`, like `OpError` says what to fix. Rule ids are `<area>.<rule>` snake case: `scene`, `avatar`, `rights`, `vrm`, `perfect_sync`, `gltf` here; implementation crates add their own areas (`vrm0.…`, `fbx.…`). Ids live next to the code that emits them (no central list) and are part of the contract. `strict` raises warnings to errors (CI, conformance). Reports are serialisable and schema'd because they go straight back to people and agents as op output.

Three layers:

1. **Import:** the importer's own findings (dropped extensions, decomposed matrices, folded joint sets) plus `IoScene::check` plus rights notes (by the importer's `LicenceEvidence`), in `ImportOutcome.report`.
2. **Export preflight:** `preflight` + `Exporter::check`, before anything is written; failure is `IoError::Rejected(report)` with *every* finding.
3. **Export notes:** what the exporter had to drop or approximate, in `ExportOutcome.report`, merged with the preflight warnings.

Rule catalogue shipped in the API crate:

| Area | Rules |
|---|---|
| `scene.*` (errors unless noted) | `index_out_of_range`, `node_multiple_parents`, `root_has_parent`, `node_cycle`, `non_finite`, `rotation_not_unit`, `attribute_length_mismatch`, `skin_attributes_incomplete`, `indices_invalid`, `mesh_invalid` (the shared mesh type's own validation), `morph_target_mismatch`, `skin_matrices`, `joint_out_of_range`; warnings: `skin_without_mesh`, `uv_set_dropped`, `weights_not_normalised` |
| `avatar.*` | `humanoid_duplicate_node`, `bind_weight_range`, `morph_bind_invalid`, `spring_chain_order`, `spring_joint_shared`, `spring_center`, `collider_invalid` (non-finite number, negative radius, plane normal not unit length), `limit_invalid` (negative or non-finite angle, rotation not a unit quaternion), `limit_on_tail` (warning: a limit on the last joint of a chain, which the spec ignores), `constraint_invalid`, `constraint_cycle` (a circular dependency between two or more constraints; VRMC_node_constraint 1.0) |
| `vrm.*` (`vrm_profile`) | `humanoid_missing`, `humanoid_required_bone`, `humanoid_parent`, `humanoid_missing_parent`, `humanoid_scale`, `meta_name_missing`, `meta_authors_missing`, `meta_license_url_missing`; warning `meta_license_url_unknown`; `tpose` (warning on export: a limb segment more than 10 degrees off its T-pose axis); `meta_unknown_keys` (info on import, warning on an export that cannot write them); warnings `collider_dropped`, `limit_dropped` (extended colliders and joint limits a 1.0 export cannot write yet) |
| `vrm0.*` | `material_name_duplicate` (0.x binds find materials by *name*); warnings `collider_unsupported`, `limit_unsupported` (0.x has no extended colliders or joint limits); lossy-meta warnings from `Vrm0Meta::from_meta`: `meta_licence`, `meta_commercial`, `meta_content_flags`, `meta_third_party_licenses` |
| `rights.*` | see D5: `no_license_info`, `license_not_imported`, `unrecognised_license_url`, `import_restricted`, `other_license`, `provenance_missing`, `gate_*`, `terms_exceed_provenance`, `private_test_export`, `licence_forbids_redistribution`, `licence_forbids_modification`, `private_export_restricted` |
| `perfect_sync.*` | `missing_channel`, `inert_channel` |
| `gltf.*` (info unless noted) | `passthrough` (what is carried unchanged). The real rules are the table in `omuzaki-gltf`'s crate documentation (shared by `omuzaki-io-gltf` and `omuzaki-io-vrm`), plus what `omuzaki-io-gltf` adds for plain glTF (`image_dropped`, `texture_dropped`, `avatar_dropped`, `toon_dropped`; its crate documentation). The mock's `images_dropped`, `vertex_colours_dropped` and error `layout_unsupported` are the mock's own and appear in no real file's report |

### D8. The humanoid bone table is a file-format fact and lives here

`HUMAN_BONES` is the 55-row table from `humanoid.md` (name, parent, required, needs-parent), generated from the spec text. `HumanBoneName` can only hold one of those names (checked at construction and when deserialising). `vrm_profile` checks required bones (15 in 1.0; 17 in 0.x, which also requires `chest` and `neck`), the parent rule (a bone's nearest humanoid ancestor must be its spec parent, skipping absent optional ones; `upperChest` and finger joints can't exist without their parent) and positive scale.

The one 0.x/1.0 difference is the thumb: 0.x `ThumbProximal / Intermediate / Distal` are 1.0 `ThumbMetacarpal / Proximal / Distal`. `BoneSpec::vrm0_name` and `HumanBoneName::from_vrm0` hold that rule once.

`omuzaki-anim-api` (being designed) owns the runtime skeleton and its bone enum. The contract between the two is the VRM 1.0 camelCase string: the anim bone type offers `as_vrm_str()` / `from_vrm_str()` and the op layer converts through it. See Q4.

### D9. Fidelity rules that came from real files

These are in the trait docs as "must", and checked where the scene makes it checkable:

- **Inverse bind matrices are data, not derived.** A model sculpted in an A-pose on a T-pose skeleton has bind matrices that differ from the rest pose. Exporters write them as given.
- **Bone transforms are written as given.** VRM 1.0 requires a T-pose rest (`tpose.md`). Exporters never "fix" a pose; `omuzaki-io-vrm` checks it on export with the `vrm.tpose` warning (arms against ±X, legs against −Y, 10 degrees; hands, fingers and spine are not checked).
- **Original image bytes are kept**, not decoded and re-encoded, so an unchanged texture round-trips bit-identically and keeps its `ContentHash`. VRM allows only PNG and JPEG thumbnails.
- **Tangents are not stored** and not exported (VRM 1.0: recompute with MikkTSpace). Morph target tangents are not stored either.
- **A file with no `NORMAL` gets flat normals** (glTF 2.0: clients "MUST calculate flat normals"), which un-shares vertices, so a re-export is larger than the source; the importer says so with `gltf.normals_generated`.
- **`extras.targetNames`** carries morph target names (the glTF convention VRM 1.0 recommends).
- **`POSITION` accessors need `min` and `max`**, including morph target ones (glTF 2.0, "Morph Targets"; the mock test checks it).
- **Primitives of one mesh share a morph target count** (glTF 2.0); a primitive with none may stay empty.
- **`KHR_texture_transform` rotation and `texCoord` are not represented** (VRM 1.0 recommends against them); importers drop them with a warning.
- **MToon:** vertex colours are ignored by the spec. The README gives `shadeColorFactor` default `[0,0,0]` but the JSON schema says `[1,1,1]`; we follow the README and always write explicit values, so a file we write depends on neither.
- **Unknown extensions and `extras` pass through** (decided on review). `Passthrough` holds raw JSON for the scene, each node and each material; importers capture optional extensions they don't model, exporters write them back and list them in `extensionsUsed`. It is best effort: the JSON is never interpreted or edited, indices inside it (a texture `{"index": 3}` in a material extension) are not remapped (the scene keeps arenas in file order, so an unmodified round trip stays valid; the one exception is an image or texture `omuzaki-io-gltf` drops as unusable, which shifts the later ones and is reported by `gltf.image_dropped` or `gltf.texture_dropped`), and an extension on an object without a bag (a mesh, a primitive, a texture, a spring joint) is reported as dropped. A *required* extension the importer doesn't know is refused, not passed through.
- **Spring bones:** 1.0 chains need each joint to descend from the one before; the last joint is only a tail. 0.x makes the tail implicit (7 cm); the 0.x importer makes it explicit. Extended colliders and joint limits are modelled (`VRMC_springBone_extended_collider-1.0`, `VRMC_springBone_limit-1.0`): `ColliderShape` also holds `SphereInside`, `CapsuleInside` and `Plane`, and `SpringJoint::limit` an optional `JointLimit` (cone, hinge, spherical; radians). Both enums are `#[non_exhaustive]` because the VRM family keeps adding shapes, so consumers match with a fallback arm. The 0.x flip turns positions and the plane normal as vectors and conjugates a limit's rotation. A 0.x export has no such shapes or limits and drops them (`vrm0.collider_unsupported`, `vrm0.limit_unsupported`). A 1.0 export (and the mock) writes the spec's fallback for a reader without the extension, an inert base sphere of radius 0 at `[0, -10000, 0]`, so collider indices in groups stay valid, until EX-11 (phase 2) writes the extensions and simulates them (`vrm.collider_dropped`, `vrm.limit_dropped`). `vrm_profile` raises these warnings, so `Exporter::check` reports them and strict mode refuses the export before anything is written (ADR 0055); importers still report them as dropped.
- **Expression binds:** 1.0 `weight` is 0..1, 0.x is 0..100 (importers divide). 0.x binds name a *mesh*, 1.0 a *node*; the 0.x importer resolves the node that uses the mesh.

### D10. A character's licence record: where it lives, how it is set, what it may carry

Rights are a legal surface, so an export writes only terms the project recorded, and the project can record no more than its parts and application records allow. The record is `LicenceRecord` (`omuzaki-io-ops`, `gather::avatar`), read by `io.check_export` and `io.export`; three ops in `omuzaki-io-ops` (`ops::licence`) set and explain it. Legal review's sign-off on the terms themselves is separate; this is the mechanics.

**Where.** `characters/<id>/licence`, a sibling sub-tree of the character node like `variants`, `colourways`, `rig` and `look` (not a separate `licences/<id>` tree, which would go stale when a character is renamed, duplicated or deleted). It carries `schema: 1` and holds `provenance` (the §7 record the gate reads), `meta` (the `VRMC_vrm.meta` keys the provenance has no field for) and, only when `licence.set` used the project-owned exception below, `owned { asserted_by }`.

**Where the parts' rights come from.** Each part has an optional **source row** at `characters/<id>/sources/<part>` (`schema: 1`, a `provenance`, optional `terms` in the same keys as `meta`, optional `owned { asserted_by }`), written by whatever brings the part in. A part cannot hold it (`PartDef` rejects unknown keys). A part without a row (and without a project-owned kind) counts as the most restrictive part possible. A row that does not read is an error naming `sources/<part>`.

| Op | Kind | Scope | Does |
|---|---|---|---|
| `licence.derive {character}` | query | `read` | the record the parts and application records allow, with each contribution and a report |
| `licence.describe {character, purpose}` | query | `read` | what the record holds and what is missing for `private` or `publish`, as a `Report` (rule id, location, hint) in the vocabulary of `io.check_export` |
| `licence.set {character, record}` | edit, undoable | `edit:character` | stores the whole record; refuses one that grants more than `derive` says, naming the field |
| `licence.set_source {character, part, source}` | edit, undoable | `edit:character` | records one part's source row; refuses a row the file format would refuse, naming the field |

**Who writes and moves the rows.** `row_patch` (`ops::licence::source`) is the one way a row is written: no nulls, and the `sources` table created in the same patch so undo removes it.

| Op | What it does to the rows |
|---|---|
| `licence.set_source {character, part, source}` | records the row by hand, replacing any; the same checks as the file (`PartSource::check`: schema, a licence that is not blank, a named asserter), and a refusal names the field (`source/provenance/licence`). A row that does not read can be repaired by recording it again. The summary lists notes: `terms` that `derive` will ignore (status not `ok`, or private test material), `owned` on private test material (never waived), a stored record that may now be stale |
| `part.duplicate` (`omuzaki-char`) | copies the original's row verbatim to the copy (the same asset under the same licence), or clears a leftover row under the new id when the original has none |
| `part.remove` (`omuzaki-char`) | removes the part's row with it; undo brings both back |
| `part.add` (`omuzaki-char`) | clears a row left under the id by a part that no longer exists, so a new part never inherits another part's rights; a new part has no row |
| `character.rename` | nothing to do: the rows are inside the character's node, and a rename changes only the display name |

`omuzaki-char` moves a row as an opaque node (the type is `omuzaki-io-ops`', and implementations may not depend on each other); only the place `characters/<id>/sources/<part>` is shared, and a test pins the key against `CharacterDef::SIBLING_KEYS`. **Not built yet, so nothing writes a row from them:** the library ops (LB-01, LB-03: a library item records its own provenance; an item saved to "my library" carries `owned`), `io.import` (the provenance the asset was stored with; an upload marked self-authored carries `owned`) and a part rename (no such op; it must move the row). Those ops must write the row with `row_patch` in the same patch that adds the part. Where `PartSource` lives so that the library crate can use it is open (an API-crate question).

**`derive`: the strictest join of the parts, never a guess.**

| Field | Join |
|---|---|
| `provenance.commercial` | the most restrictive: `no`, then `unknown`, then `ok` (a known restriction outranks one nobody has established yet) |
| `provenance.private_test` | true if any part is |
| the nine permission flags of `meta` | each the most restrictive value any part has (`avatarPermission`, `commercialUsage`, `modification`, `creditNotation` and the five booleans) |
| `provenance.licence` | every distinct licence joined with SPDX `AND` (all apply; an expression with its own `OR` is bracketed). A stored record must keep every conjunct, compared as flattened terms (ids case-insensitive, order and brackets ignored) |
| `provenance.author`, `meta.authors`, `provenance.attribution` | the union, in part-id order, without repeats |
| `provenance.consent` | the one consent every part and application record that has one agrees on; if they differ, none, with a warning (a record holds one) |
| `provenance.source` | the strictest source of the parts: any `Imported` wins, then `Generated`, `Library`, `Authored`. `preflight` applies the creator's own terms (VF-08) to `Imported` material, so a record may not call such a character authored (`set` refuses `record/provenance/source`) |
| `meta.name`, `meta.licenseUrl` | the character's display name; the VRM Public License 1.0 page, the document that defines the flags a VRM 1.0 file must name. Neither widens a right |

- **Kind-declared provenance.** `PartKind::provenance()` (`omuzaki-char-api`, `KindProvenance`: `ProjectOwned` | `External`, default `External`) says whose rights a generator's output carries. A part with no row whose registered kind is `ProjectOwned` is the project's own: `commercial: ok`, source `Authored`, licence `LicenseRef-Omuzaki-User`, owner "the project (built-in part kind)", and the most restrictive terms (a kind says whose rights, not what is granted). So a fully procedural character derives, sets and publishes. A row about a part wins over its kind. An `External` or unregistered kind with no row stays most restrictive.
- A part with **no source row** (and no project-owned kind) counts as the most restrictive part possible: `commercial: unknown`, the default (most restrictive) terms, licence `LicenseRef-Unknown`. A character with **no parts** joins to the same floor. So the answer only widens as sources are recorded. `complete` says whether every part had a row, and the report names each part that had none and where its row goes.
- **Every part is joined, enabled or not:** a variant can switch any part on, and one record serves every variant.
- **No permissive default.** `Commercial::Ok` does not default `commercialUsage` to `personalProfit`: a part's terms say what it grants, and what a row leaves out is the most restrictive value. Legal review can relax this later.
- A status that is not `ok`, and private test material, carry **only the most restrictive value of every flag** (the direction of the exporters' `rights.terms_exceed_provenance`), so the derived record always passes `preflight`.
- An attribution on any part makes credit required whatever its terms say.

**`set`: refuses a record more permissive than `derive`.** It checks, in this order, and the first problem's field is the error's `field` (a path into the arguments, `record/meta/commercialUsage`; the message lists every offending field and the parts that set each limit):
1. the record is well formed: `schema` is 1, the licence is not blank, a consent names a person and a scope (`record/schema`, `record/provenance/licence`, `record/provenance/consent/person`);
2. it does not contradict itself: terms grant nothing while the status is not `ok`, and private test material carries only the most restrictive terms;
3. it grants no more than the parts and application records allow on any permission: `provenance/private_test`, `provenance/commercial`, `provenance/licence` (the record's conjuncts must include every conjunct of the parts' licences), `provenance/attribution` (a credit the parts require may not be dropped; rewording around it is fine, but the credit must stand as whole tokens, not inside a longer word) and each flag of `meta`. Authors, contact, version and consent are not permissions and are free to add.

**The project-owned exception.** If **every** part has `owned.asserted_by` (one of the project's own library items, or the person's own upload marked as self-authored) **and every application record's rights carry `owned`**, rule 3 is waived and the op writes `owned { asserted_by }` (the sorted names from the parts and application records) into the record, replacing whatever the caller passed, and only when the record needed it. It is never waived for **private test material** (fan art stays private even when its drawer owns it, so a character with such a part has no exception at all), and a part with no row, and an application record whose rights lack `owned`, is not project-owned. Rule 2 is never waived.

**Application records (the library, `docs/design/library.md` D8).** `derive` also reads every record under `characters/<id>/applied/` (`omuzaki_library_api::ApplicationRecord`) and joins its `rights` exactly as it joins a source row, the strictest of each field, because an item can change things that are not parts (the face, the palette) and a non-commercial one must not slip past the gate. Each record is a named contribution (`applications`: the record's path, item and version) in the output. A record that does not read, or whose licence (or a consent's person or scope) is blank, is an error naming `applied/<app>`; so is a stored licence record or part source row with a blank licence or a consent with a blank `person` or `scope` (one shared check, `blank_rights_field`). A record counts as the project's own only when its rights carry `owned`. A character with no parts keeps the most restrictive floor (`LicenseRef-Unknown`, commercial `unknown`) whatever it has applied. For `publish`, `io.check_export` and `describe` raise `rights.item_not_cleared` (an error, located at the record) for an applied item that is `commercial: no` or `unknown`, or private test material; `private` passes.

**`describe`: what is missing for a purpose.** (`io.check_export` reports the record-against-parts findings below through the same code.) Errors block the purpose (`ok`); `ok_for_vrm` also needs authors and the licence URL (warnings, since a plain glTF does not). For `private`: a record exists and reads, does not contradict itself, and private test material is flagged. For anything else (today `publish`): the licence gate passes, every part has a recorded source, and the record grants no more than the parts and application records allow (for `private` the last two are warnings). **A private test part fails every purpose but `private`**, with or without a record and even if the record forgot the flag (`rights.gate_private_test`, located at the part's row); a record that forgot the flag is an error for `private` too.

**`io.check_export` and `io.export`: a stored record goes stale.** The record is a snapshot, so a part that changes after `licence.set` (a library item with tighter terms swapped in, a part with a non-commercial source switched on, a part added with no source) leaves a record that grants more than the parts do. `CheckExport::run` is the one path to a verdict: it derives what the parts and application records allow now and holds the stored record against it with the code `describe` uses (`ops::licence::findings`), so the two cannot disagree. `io.export` must call `run` before it writes anything. Per field that exceeds the parts there is one finding, located at that field of the record (`characters/<id>/licence/meta/commercialUsage`), whose message gives what the record says, what the parts and application records allow and which contributions limit it (a part with no source row is named as such), and whose hint is "run licence.derive and licence.set":

| Rule | Severity | When |
|---|---|---|
| `licence.exceeds_parts` | error for `publish`; warning for `private` | the record grants more than the parts and application records allow on any permission (`provenance/commercial`, `provenance/source`, `provenance/licence`, `provenance/attribution`, each flag of `meta`) |
| `licence.exceeds_parts` at `provenance/private_test` | error for every purpose | a part is private test material and the record lacks the flag; never waived |
| `rights.gate_private_test` | error for `publish` | a part is private test material, even if the record lacks the flag (as in `describe`) |
| `licence.exceeds_parts_owned` | note if the record carries `owned`, warning if it does not | every part and application record is the project's own: the exception stays honoured, and the answer does not block. The moment one is not owned (a swapped-in item with no `owned`), the findings are errors again, whatever the record says |

Nothing is said about the parts when there is no record: that is `rights.provenance_missing`. A project whose record was written by hand, before the parts had source rows, therefore fails `publish` until the rows are recorded or the built-in kinds declare their provenance, which is the point: rights are never guessed.

**Sibling key.** `sources` is a sibling of `licence` in the character node; `CharacterDef::from_node` must ignore it exactly as it ignores `licence`. Whichever of the two changes (the character reader and the licence rows) merges second adds `"sources"` to `SIBLING_KEYS`.

**Known limits.** The op decoder names the `meta` table, not the key, for a bad flag value, because the flags are flattened into `meta` (the message lists the valid values). A typo in a `meta` key never reaches a typed flag, which keeps its restrictive default.

### D11. Animation is its own type with its own exporter trait (EX-16)

**Decision.** A motion is an `IoAnimation`, a standalone type in a new module `omuzaki_io_api::animation`. It is **not** a field of `IoScene`. It is written by an `AnimationExporter`, a second trait with its own self-registration, run by `run_export_animation`. The change is **additive** (a new module, a new trait, a new macro; no existing type, field or rule changes), so it needs no ADR. The format mapping (`VRMC_vrm_animation`), the conversion from clips and emotes and the list of what cannot be exported are in `docs/design/vrma-export.md`.

**Why not a field on `IoScene`.** `IoScene` means "a model". Everything that reads one assumes that:
- `IoScene::check` and `summary` count meshes and skins;
- `preflight` demands `meta.terms`, runs `terms_exceed_provenance` and (by default) asks for the 52 perfect-sync channels;
- `Exporter::check` for VRM calls `vrm_profile`.

A `.vrma` has no mesh, no material, no `VRMC_vrm.meta` and no expressions-as-binds, so each of those would need an "unless it is an animation" exception, and every future exporter would inherit the question. A separate type also keeps N+N: a VRMA importer (RG-08) or a BVH importer yields an `IoAnimation` without inventing an empty model, and a model exporter that wants to carry clips (glTF for Unity or Godot, EX-03 onward) can later add `IoScene::animations: Vec<IoAnimation>` (the scene is `#[non_exhaustive]`), reusing this type unchanged. *Cost:* a second small registry and one more entry point. It is the price of not special-casing the model path; the registry code is a copy of `format.rs` (about 60 lines).

**The shapes** (docs on every item, `#![deny(missing_docs)]`; none of these derive serde, like `IoScene`, because they hold large float arrays; the op returns `AnimationSummary`, which does):

```rust
/// One humanoid animation, in VRM normalised space (see vrma-export.md V3). `#[non_exhaustive]`; build with `IoAnimation::new`.
pub struct IoAnimation {
    /// Names the glTF animation. Not unique, not an id.
    pub name: String,
    /// The T-pose node tree written with the animation: where each humanoid bone sits, rotations all identity.
    pub rest: RestHumanoid,
    /// The tracks, in any order. An exporter writes them in one canonical order (hips, bones in `HUMAN_BONES` order,
    /// expressions by name, look-at), so equal animations give equal bytes whatever order they were built in.
    pub tracks: Vec<Track>,
    /// Gaze origin relative to the head bone (`lookAt.offsetFromHeadBone`); `None` writes the VRM sample value.
    pub look_at_offset: Option<Vec3>,
    /// A hint that the last key meets the first (a loop); carried in `asset.extras`, not a VRMA feature.
    pub looped: bool,
    /// What Omuzaki records about the motion's rights (the clip's own record). `None` is refused by `run_export_animation`.
    pub provenance: Option<Provenance>,
}

/// The T-pose hierarchy, as local translations. Rotations are identity by definition: the animation is normalised.
pub struct RestHumanoid { pub bones: Vec<RestBone> }
pub struct RestBone {
    pub bone: HumanBoneName,
    /// Metres, relative to the nearest present humanoid ancestor (hips: relative to the file origin, so its y is the hips height).
    pub translation: Vec3,
}

pub enum Interpolation { Linear, Step }   // CUBICSPLINE is not offered: the spec's players interpolate linearly (V5)

pub struct Keys<T> {
    pub interpolation: Interpolation,
    /// Seconds from the start, strictly increasing, from 0.
    pub times: Vec<f32>,
    /// One value per time.
    pub values: Vec<T>,
}

/// A track's target and its value type are one thing, so a rotation on an expression cannot be built.
#[non_exhaustive]
pub enum Track {
    /// Parent-local rotation of a humanoid bone (never an eye: gaze is `LookAt`).
    Rotation { bone: HumanBoneName, keys: Keys<Quat> },
    /// The hips node's local translation, metres, absolute (rest hips plus the pose's offset).
    Hips(Keys<Vec3>),
    /// An expression weight, 0..=1. A preset (not the four `look*` ones) or a custom name.
    Expression { name: ExpressionName, keys: Keys<f32> },
    /// The gaze direction as a rotation (yaw about Y, pitch about X; vrma-export.md V7).
    LookAt(Keys<Quat>),
}

impl IoAnimation {
    pub fn new(name: impl Into<String>, rest: RestHumanoid) -> Self;
    /// The last key time over all tracks.
    pub fn duration(&self) -> f32;
    /// Every invariant below, as a `Report` (no side effects).
    pub fn check(&self) -> Report;
    pub fn summary(&self) -> AnimationSummary;   // tracks per kind, keys, duration; serde + JsonSchema
}

pub trait AnimationExporter: Send + Sync {
    fn info(&self) -> FormatInfo;
    /// Format-specific checks (the VRMA exporter: node and extension rules). Not the shared ones.
    fn check(&self, animation: &IoAnimation, options: &ExportOptions) -> Report;
    fn export(&self, animation: &IoAnimation, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError>;
}
pub struct AnimationExporterRegistration { pub create: fn() -> Box<dyn AnimationExporter> }
// register_animation_exporter!(T), animation_exporters(), find_animation_exporter(id), animation_formats() -> Vec<FormatInfo>
pub fn run_export_animation(e: &dyn AnimationExporter, a: &IoAnimation, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError>;
```

`FormatInfo`, `ExportOptions`, `ExportCtx`, `ExportOutcome`, `OutputFile`, `Purpose`, `Report` and `IoError` are reused as they are. `ExportOptions::layout` and `perfect_sync` do not apply to an animation and are ignored (a `.vrma` is always one GLB, like a `.vrm`); no field is added. `animation_formats()` is separate from `formats()` on purpose: `FormatSupport` is not `#[non_exhaustive]`, so adding a field would break its constructors. `io.list_formats` gains an additive `animations` array.

**Why these shapes.**
- *glTF space, glTF vocabulary, `f32` times:* the model is what the file says (D1). Times are `f32` seconds because that is the accessor type; the converter samples in `f64` and rounds once.
- *Keyed, not a sampled grid.* A track owns its times, so key reduction (a constant track is two keys) and a hips track that differs from the bone tracks need no special case. A grid is the special case where every track has the same times.
- *A target enum instead of `target + values: Vec<f32>`.* The legal combinations are small and fixed by the spec (rotation for bones and look-at, translation for hips only, a weight for expressions), so the type holds them and `check` has fewer failure modes.
- *`Hips` carries an absolute translation.* A glTF channel sets the node's translation. The conversion adds `HumanoidPose::hips_offset` to the rest hips; exporters write what they are given (D9).
- *Rest is in the animation,* because the players need it (V4): three-vrm reads the hips height from the file's rest tree to scale the hips motion, and warns when it is not above 1 mm.

**Validation** (`IoAnimation::check`; area `anim`, errors unless noted; ids are part of the contract like D7's):

| Rule | Fires when |
|---|---|
| `anim.no_tracks`, `anim.no_keys` | no tracks, or a track with no keys |
| `anim.keys_length_mismatch` | `times.len() != values.len()` |
| `anim.time_invalid`, `anim.time_order` | a time is negative or not finite; times are not strictly increasing |
| `anim.value_non_finite` | a NaN or infinity in a value (glTF 2.0 forbids both in animation data) |
| `anim.rotation_not_unit` | a quaternion is not unit length (tolerance as `scene.rotation_not_unit`) |
| `anim.duplicate_track` | two tracks with the same target (glTF 2.0: a node and path is used once per animation) |
| `anim.eye_bone` | a `Rotation` on `leftEye` or `rightEye` (VRMA forbids it; use `LookAt`) |
| `anim.expression_reserved` | an `Expression` named `lookUp`, `lookDown`, `lookLeft` or `lookRight` (VRMA forbids them; use `LookAt`) |
| `anim.rest_invalid` | rest has a duplicate or eye bone, lacks one of the 15 required bones, or breaks the nearest-ancestor parent rule of `HUMAN_BONES` |
| `anim.rest_hips_height` | the rest hips y is not above 1 mm (the players divide by it) |
| `anim.track_without_rest` | a `Rotation` track on a bone that has no rest node |
| `anim.quaternion_flip` (warning) | two adjacent keys of a rotation track have a negative dot product |
| `anim.weight_range` (warning) | an expression weight outside `0..=1` (players clamp) |
| `anim.custom_expression_limit` (warning) | more than 100 custom expression tracks (UniVRM reads 100, V4) |

**Rights.** `run_export_animation` runs, in order, `IoAnimation::check`, an animation preflight, `AnimationExporter::check`, strict handling, then the export, with the same all-findings-at-once rejection as `run_export`. The preflight is the model preflight's rights part without the terms:
- `rights.provenance_missing` when `provenance` is `None`;
- `Purpose::Publish`: `licence_gate` must pass (`rights.gate_non_commercial`, `_unknown`, `_private_test`, `_no_consent`), the same function and messages (the model preflight's gate code moves to a function both call);
- `rights.private_test_export` (info) for private test material, which only `Purpose::Private` allows.
- `rights.licence_forbids_derivatives` (error under `Purpose::Publish`; info `rights.nd_private_export` under `Private`) when the record's licence includes a no-derivatives (`ND`) identifier: `licence_forbids_derivatives(&Provenance)` in `omuzaki-io-api`, which library and share uploads of a motion call too. Details and message in `vrma-export.md` V12.

There is nothing to cap (`terms_exceed_provenance`, `licence_forbids_*`): a VRMA carries no permission flags, so the file says no more than the record does. What is written is a **credit line and a short record, from the clip's own provenance only** (never the character's, never a consent holder's name): `asset.copyright` is the record's attribution text, else its author; `asset.extras.omuzaki` holds `licence` (SPDX or `LicenseRef`), `commercial`, `source` kind, `author`, `looped`, and `private_test: true` when set. Other apps ignore `extras`; a reader of the file can see where the motion came from. The record for an exported emote is the strictest join (D10) of the records of the items it was made from.

## 4. Mock

`omuzaki-io-mock` is a real small glTF 2.0 implementation, not canned data: GLB container with bounds-checked chunks, accessors (dense, tightly packed, with checked offsets), nodes, meshes with morph targets, skins, plain PBR materials, and the `VRMC_vrm` 1.0 `meta`, `humanoid` and morph-bind `expressions`. It refuses what would come out wrong (sparse or interleaved accessors, matrix nodes, non-triangle modes, unknown required extensions) and reports what it drops (images, MToon, look-at, first-person, spring bones, constraints).

All fixtures are generated by the code: `tiny_glb()` assembles a one-triangle GLB byte by byte, independently of the exporter, and `tiny_avatar()` builds a 15-bone skeleton with a skinned triangle, a morph target, two expressions, meta and a provenance record. No third-party or private file is used.

Tests (all in the mock because they need the avatar fixture, and an API crate must not depend on its own mock):

| File | Covers |
|---|---|
| `tests/roundtrip.rs` (16) | hand-built GLB import (with generated flat normals); unindexed primitives; pass-through of unknown extensions and extras; GLB and VRM round trips; byte determinism and canonical form; the VRM file's spec shape; sniff ranking; registry; limits; malformed and unsupported files; an importer that breaks invariants is rejected; strict mode; cancellation; every problem reported at once |
| `tests/checks.rs` (7) | every `scene.*` / `avatar.*` rule fired by a one-line mutation of a valid scene; node cycles; spring chains and constraints; the VRM profile; 0.x requirements; every rights rule by purpose and provenance |
| `tests/perfect_sync.rs` (6) | on by default and extras off; an avatar-less scene isn't asked for channels; 52 missing channels reported; bake then export then import with all 52 intact; the morph budget making channels inert, and strict mode refusing that; `omuzaki_` extras; a bake needs a mesh |
| API crate unit tests (32) | flat-normal generation, humanoid table and the thumb rename, preset mapping, custom-name rules, toon defaults, meta and provenance mapping (1.0 and 0.x), `UseTerms::is_within`, reports |

## 5. Interfaces with the other contracts

The render, char, face and anim contracts are being designed in parallel. Nothing here depends on them; this is what each needs from, or gives to, `omuzaki-io-api`.

| With | Direction | What crosses | Notes |
|---|---|---|---|
| `omuzaki-store-api` (merged) | uses | `Provenance`, `Commercial`, `licence_gate`; the importer's bytes go to `Store::put` with the derived provenance | `omuzaki-io-api` depends on it. Original VRM bytes are stored as is, so the full meta is always recoverable; the record keeps only what the gate reads |
| `omuzaki-ops-api` (merged) | op layer calls | proposed ops below | `OpKind::Job`, scopes `fs` + `edit:character` (import), `fs` + `publish` (export). `IoError` maps to `OpError`: `Malformed`, `Limit`, `Rejected` → `Invalid`; `Unsupported` → `Unsupported`; `Resource` → `NotFound`; `Cancelled` → the job's cancelled state; `Internal` → `Internal` |
| `omuzaki-render-api` | shares `MeshData`; op layer converts the rest | `MeshData` (shared, in `omuzaki-core`), `Material`, textures, skins | Meshes move as is. `Toon` is MToon 1.0 field for field; render-api's `Toon` is smaller. Per its doc: `shadeColorFactor` → `shade`, `shadingShiftFactor` → `shift`, `parametricRim*` → `Rim`, outline → `Outline`; the rest (GI equalisation, matcap, UV animation, render-queue offset) is reported, not drawn. Textures: `EncodedImage` is decoded to `Image` + `TextureDesc` by `omuzaki-io` (render-api says PNG codec lives there). Linear factors → `Colour` at this boundary |
| `omuzaki-char-api` | op layer converts | character → `IoScene` (export); `IoScene` → part or reference (CB-44 import) | `CharId` is not used here. The bake step needs the head node. Import as a part needs auto-skinning, which is char/rig work |
| `omuzaki-face-api` | implements `ExpressionBaker` | channel set, face painter, atlas, the baker lookup (`face_baker()`, G13, see D6) | **Channel ids:** `slug_id!` ids are lowercase, ARKit names are camelCase. If face-api uses slugs, the 52-row slug ↔ ARKit table lives in `omuzaki-io-ops` (decided on review), and a `omuzaki-test` test asserts it covers `ARKIT_52` exactly. The ARKit names live in `omuzaki_io_api::ARKIT_52` because perfect-sync naming is a file convention; face-api may re-export or assert equality |
| `omuzaki-anim-api` | op layer converts | humanoid bones, skeleton rest pose, spring-bone parameters, expressions, look-at | Bones cross as VRM 1.0 strings (D8); the conversion lives in `omuzaki-io-ops`. Spring parameters: `SpringJoint` has the 1.0 fields; anim-api's solver parameters should be a superset or a lossless map. VRMA motion *export* is D11 (`IoAnimation`, built from a `HumanoidPose` clip by `omuzaki-io-ops`). VRMA motion import (RG-08) isn't here: it yields anim clips and belongs with anim-api; if it lands in `omuzaki-io`, it is a new `AnimationImporter` type against `IoAnimation` and a registry like D11's |
| `omuzaki-doc-api` | none | | Imported assets are referenced from the document by `ContentHash`; `omuzaki-io-api` never touches the document |

**Proposed ops** (built later in `omuzaki-io-ops`, listed so the contract can be judged against them):

| Op | Kind | Scope | Does |
|---|---|---|---|
| `io.list_formats` | query | `read` | `formats()` |
| `io.inspect` | query | `read`; a `path` outside the project also needs `fs` and a covering root; `asset` reads the store | sniff, import into memory, return `SceneSummary` and the report; touches nothing (the import dry-run) |
| `io.import` | job | `edit:character` + `fs` | `run_import`, `Store::put` the original and images with provenance, return summary and report; the document change is a patch that records the asset |
| `io.check_export` | query | `read` | `preflight` + `Exporter::check`: what would block an export, without writing |
| `io.export` | job | `publish` + `fs` | gather → bake → `run_export` → host writes the files; `Purpose` from the request |
| `licence.derive`, `licence.describe`, `licence.set`, `licence.set_source` | query, query, edit, edit | `read`, `read`, `edit:character`, `edit:character` | the character's licence record, the parts' source rows and the library's application records (D10) |

### The asset record

`io.import` records each original it stores at `assets/<content hash>`: the original's hash, its images' and sidecars'
hashes, the file's name and format, the characters it serves, and its own provenance. Two rules settle what two imports
that share bytes do (#1792, #1773):

- **One asset serves several characters.** `characters` lists every character the asset is a reference for (or, for an
  avatar import, the character it became), each once, in name order. Importing the same bytes for another character
  adds that character; importing without one changes nothing; undoing an import removes only what it added. A record
  written before this had a single `character`; it reads as a list of one and becomes `characters` when the record next
  changes, so no migration step is needed.
- **Rights belong to the reference, not the object.** The store keeps one record per object, the first it was given
  (`Store::put`), so an identical texture shipped by two models under different licences or credit lines would otherwise
  carry the first model's rights for both. The asset record therefore keeps its own model's provenance, and that record
  is what the model's images are used under: both imports succeed, each asset carries its own model's licence and
  credit, and the result is the same in either order. Readers of a reference's rights read the asset record, never the
  store's record of a shared object. The original file is the one exception: identical bytes derive an identical record,
  so an original already stored under another record is refused (fail closed).

A failed `put` part way through an import (an I/O error) leaves the objects already put whole and referenced by nothing,
since the record is written only when the job finishes; garbage collection removes them, as after a cancel.

## 6. Crate layout this implies

| Crate | Kind | Depends on |
|---|---|---|
| `omuzaki-io-api` | API | `omuzaki-core` (`MeshData`, ids, glam), `omuzaki-store-api` |
| `omuzaki-io-mock` | Mock | API crates |
| `omuzaki-gltf` | Lib | `omuzaki-io-api`, `omuzaki-core`: the glTF 2.0 document model, containers, accessors, writer and the document to `IoScene` mapping, with hooks for formats built on glTF (hand-rolled over `serde_json`, ADR 0011) |
| `omuzaki-io-gltf` | Implementation | `omuzaki-io-api`, `omuzaki-gltf` (registration and the plain-glTF policy) |
| `omuzaki-io-vrm` | Implementation | `omuzaki-io-api`, `omuzaki-gltf` (VRM 1.0 and 0.x, MToon, spring bones, constraints on top of the hooks) |
| `omuzaki-io-ops` | Implementation | `omuzaki-io-api` + char / render / anim / face / ops / store APIs: the ops and the conversions |

Spec §9 lists one `omuzaki-io` crate. **Decided on review (Q2): split it**, so format code depends only on this contract and the one crate that knows the runtime types stays small. The crates are created with their first tasks; `omuzaki-io-ops` also owns the bone and channel-name mapping to anim-api and face-api (Q4).

New dependencies in this PR: none (`inventory`, `schemars`, `serde`, `thiserror`, `serde_json` are already in the register).

## 7. Decisions taken on review

Decisions on the questions Q1 to Q10 as first raised:

| # | Question | Decision | Where it landed |
|---|---|---|---|
| Q1 | Separate `IoScene` vs shared types | Keep the separate `IoScene`. Revisit the material model when char-api merges. | D1 |
| Q2 | Crate split | **Yes.** `omuzaki-io-gltf`, `omuzaki-io-vrm`, `omuzaki-io-ops` (no crates needed yet). | §6 |
| Q3 | Commercial status of imported VRMs | Keep the mapping (`personalNonProfit` → `No`, `personalProfit` / `corporation` → `Ok`): the gate governs what the user publishes. The VRM terms still need legal review. | D5 |
| Q4 | Bone and channel identity | Strings at the boundary are fine; the mapping lives in `omuzaki-io-ops`. | D6, D8 |
| Q5 | `omuzaki_` prefix and defaults | Prefix OK. **Perfect sync is on by default** (`PerfectSync::Arkit52`); **extras are off** (`Arkit52WithExtras` is opt-in). A scene without an avatar isn't asked for face channels. | D6 |
| Q6 | `Progress` vs the job manager | Settled: the traits stay separate. An io op passes `omuzaki_io_ops::ops::progress::JobProgress` over its `JobCtx` (#875); noted on the `Progress` trait. | `io.rs` |
| Q7 | Web build registration | Keep. If `inventory` doesn't populate in wasm, `omuzaki-web` gets an explicit list generated by a build script. | open |
| Q8 | Unknown extensions | **Yes, pass them through.** Raw JSON for unknown extensions and `extras` is kept per node, per material and for the scene (`Passthrough`), written back verbatim, and listed in an info finding (`gltf.passthrough`) on import and in the export preflight. Small change, done here. Limits in D9. | D9 |
| Q9 | `Purpose` default | Confirmed: default `Private`; `Publish` comes from the op's `purpose` argument and needs the `publish` grant. | D5 |
| Q10 | `Colour` in the scene | Keep linear `f32`. | D1 |

**Still open elsewhere:** whether registration runs in the web build, and the legal review of the VRM terms and the gate.

## 8. After this merges

Natural first cuts (S10): `omuzaki-io-gltf` GLB import (EX-02), then export; `omuzaki-io-vrm` 1.0 meta + humanoid + expressions (EX-01), then MToon, spring bones, constraints; 0.x import (migrator behaviour above); `omuzaki-io-ops` `io.inspect` / `io.import`; the face-side `ExpressionBaker` (FE-09); then `io.export`.

## 9. Sources

Read 2026-10-11. Specifications:

- glTF 2.0: <https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html> (source `KhronosGroup/glTF`, `specification/2.0/Specification.adoc`): "Coordinate System and Units", "GLB-stored Buffer", "Morph Targets" (including the `targetNames` note), "Binary glTF Layout" (header, chunks, padding), "Specifying Extensions".
- VRM 1.0: <https://github.com/vrm-c/vrm-specification/tree/master/specification/VRMC_vrm-1.0>: `README.md` (model space, unused items, TANGENT, `targetNames`, application order), `meta.md`, `humanoid.md`, `expressions.md`, `firstPerson.md`, `lookAt.md`, `tpose.md`, and the JSON schemas under `schema/`.
- Companion extensions: `VRMC_springBone-1.0`, `VRMC_node_constraint-1.0`, `VRMC_materials_mtoon-1.0`, `VRMC_springBone_extended_collider-1.0`, `VRMC_springBone_limit-1.0`, `VRMC_vrm_animation-1.0` (same repository). `VRMC_vrm_animation-1.0` was read in full on 2026-10-12 at a pinned commit; see `docs/design/vrma-export.md` §8.
- VRM 0.x: <https://github.com/vrm-c/vrm-specification/blob/master/specification/0.0/README.md>.

Reference behaviour (read to learn what real files and the incumbent do; MIT-licensed; nothing copied): UniVRM `Packages/VRM10/Runtime/Migration/` (`MigrationVrmMeta.cs`, `MigrationVrmHumanoid.cs`, `MigrationVrmExpression.cs`, `RotateY180.cs`) and `Vrm10Exporter.cs` (licence URLs).

Perfect-sync naming: Warudo, "3D primer" <https://docs.warudo.app/docs/tutorials/3d-primer> (the 52 names, case-sensitive matching); Apple `ARFaceAnchor.BlendShapeLocation` (the names' origin). VSeeFace's own matching rules weren't found in a primary source; the exact camelCase form is the safe choice.

A real VRM 1.0 contains these parts, and a rig keeps them (`VRMC_vrm`, `VRMC_node_constraint`, expressions with morph, colour and texture-transform binds, `firstPerson`, spring bones). The contract can represent all of it.
