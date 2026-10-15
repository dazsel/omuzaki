---
title: "0011: Shared format libraries as a layer (2026-10-12)"
summary: "Shared format libraries as a layer"
area: decisions
---

# 0011: Shared format libraries as a layer (2026-10-12)

**Status:** accepted. **Context:** sharing the glTF reading between `omuzaki-io-gltf` and `omuzaki-io-vrm`; spec v3 §27 and §39 rule 10; `docs/design/io-api.md` §6; `crates/README.md`.

## Context

A VRM file *is* a glTF file with `VRMC_*` extensions on top. `omuzaki-io-gltf` and `omuzaki-io-vrm` were built in parallel, and each needed the same few thousand lines: the GLB container, the typed glTF JSON, accessor decoding (strides, sparse, normalised and quantised integers, limits before allocation), `data:` URIs, and the mapping between a glTF document and the `IoScene` (meshes with morph targets and skins, materials with `KHR_texture_transform`, `KHR_materials_unlit` and emissive strength, images, samplers, nodes, matrix decomposition) and back (a deterministic writer). Measured on the VRM side: about 2,300 lines with tests, and the subtle rules in them (a second skinning set shorter than the first, rotations that must not be renormalised, sparse morph targets, 16-bit indices) are exactly the ones that drift when written twice.

The layering rule (`cargo xtask lint-deps`, §39 rule 10) let an implementation crate depend only on API crates. So `omuzaki-io-vrm` could not call `omuzaki-io-gltf`, and the duplication was forced by the rule, not by the design.

## Decision

1. **A new layer: format libraries ("lib").** A lib crate holds code that several implementations share and that is a *format*, not behaviour. It declares itself with `[package.metadata.omuzaki] layer = "lib"` in its manifest (no crate name says it, and a name list in `xtask` would be a shared file every format PR edits).
2. **The rule.** Implementations, apps and other libs may depend on a lib. A lib may depend on API crates and other libs, never on an implementation or an app; mocks only as dev-dependencies, like everyone. **Contracts and mocks may never depend on a lib, not even as a dev-dependency**: a contract must not pull real code in, and a mock is the independent oracle the real code is checked against (`omuzaki-io-gltf` is tested against the mock's own GLB reader and writer in both directions). `lint-deps` enforces all of it, and rejects an unknown layer or `lib` on a contract, mock or app, so a mock cannot exempt itself.
3. **`crates/omuzaki-gltf` is the first lib, and it owns both halves of "glTF":** the document layer (typed model with raw `extensions` and `extras` on every object and on the root, GLB and `.gltf` containers, `data:` URIs and sidecars through the host's `Resolver`, accessor decode, packer and writer) and the scene layer (document to `IoScene` and back). It depends on `omuzaki-io-api` and `omuzaki-core`, hand-rolled over `serde_json` as `omuzaki-io-gltf` already was (full control of pass-through, determinism, explicit defaults and allocation limits; the `gltf` crate would have added `gltf-json`, `gltf-derive` and checked wrappers we would have to wrap anyway). No new external dependency.
4. **Formats plug in through hooks, not forks.** `ImportHooks` and `ExportHooks` say which extensions the format interprets (kept out of the pass-through bags, no finding), which to drop and why, which extra extensions a file may *require*, whether an unusable image or texture is dropped or refuses the file (VRM refers to images and textures by file index, so it must never renumber them), and they hand each node, mesh and material to the format with the file's raw JSON object and index to finish (`VRMC_materials_mtoon` becomes `Shading::Toon` there). The writer returns a `Written` the format adds to before it is serialised, and computes `extensionsUsed` from what is in the document, nested bodies included. `omuzaki-io-gltf` shrinks to the `glb` registration plus the plain-glTF policy (VRM data is dropped with a warning that names the fix, an avatar or `MToon` material is reported on export).
5. **The shared reader takes the stricter rule where the two crates disagreed**, and each is a tested behaviour of `omuzaki-gltf`: triangle strips and fans drop their degenerate triangles; an index past the vertex count is `Malformed` whether or not normals have to be generated; skin weights further than 1e-3 from a sum of 1 are renormalised (8-bit weights), sums within it are left bit for bit; a rotation is normalised only when it is further than 1e-6 from unit length; morph targets are stored sparsely when smaller; a base64 `data:` URI may use either alphabet and may be wrapped.

## Rejected options

- **Compose through the registry at run time** (`omuzaki-io-vrm` calls `find_importer("glb")` and layers `VRMC_*` on top). No new edge, but it relies on invariants no type states (the plain importer keeps arenas in file order, never fires its `VRMC_*` warning for VRM, keeps images and textures aligned), and it loses glTF-level detail VRM needs: `MToon` texture references, which are glTF texture infos with their own `KHR_texture_transform`, would be rebuilt from raw JSON; the VRM 0.x thumbnail is a texture *index*; morph target names live on the mesh for 1.0 and on the primitives for 0.x. Export would need the plain exporter to accept the VRM extensions as pass-through, then patch the result.
- **An exception in `lint-deps`: `omuzaki-io-vrm` may depend on `omuzaki-io-gltf`.** Cheap, but it makes one implementation crate a dependency of another, which is the coupling the rule exists to prevent (their tasks stop being parallel, and `omuzaki-io-gltf` would carry VRM's needs in its public API), and every future exception is a name in a shared file.
- **Accept the duplication.** Two validated copies of the same rules drift, and the bugs fuzzing finds in one (an index panic on a short second skinning set) have to be found again in the other.
- **A lib that is only the document layer** (model, container, accessors). The subtle rules are in the scene mapping, which both crates need; leaving it out leaves most of the duplication.

## Consequences

- Formats built on glTF (VRM now; VRM Animation and other `KHR`-based formats later) share one validated implementation, one fuzz suite and one writer. A fix lands once.
- `omuzaki-gltf` is a public surface two crates depend on: changes to its hook traits and `schema` types are reviewed like a contract change (maintainer review), though it is not an API crate. The hook contexts are structs and the traits have defaults for every method, so adding a hook or a field is not a breaking change for a format that ignores it.
- `crates/README.md` gains the layer and its rule, `xtask` gains the `Lib` kind (read from manifest metadata, tested in unit tests and against fixture workspaces), and spec §39 rule 10 names format libraries.
- Evidence moved with the code: the robustness suite (about 95,700 hostile inputs per run) and the round-trip suite now live in `omuzaki-gltf`; `omuzaki-io-gltf` keeps the registration and policy tests.
- Future shared format code (an FBX or USD helper, say) can follow the same pattern. A lib that needs to register something with `inventory`, hold global state or implement a contract is not a lib: it is an implementation, and belongs under the implementation rule.
