---
title: "Flat 2D puppets (EX-08): the model, the runtime, the renderer seam, the mapping and import"
summary: "Flat 2D puppets: one contract crate, one runtime, a CPU reference renderer, layered-art auto-rig, Inochi2D import, tracks and shorts"
area: design
---

# Flat 2D puppets (EX-08)

**Status:** accepted by its owner (spine phase, ADR 0067); the code is on `main` with this page.
**Features:** EX-08 (Inochi2D import), the 2D path of SH-03 (shorts), the reference loop's puppet-from-video.
**Decision record:** `docs/decisions/0068-puppet2d.md`.
**Spec:** v3 §27 (EX-08), §26 (SH-03, SH-05), §21 and §22 (PF-20, PF-24), §24 (ST-10, ST-12), §25 (BR-01 to BR-04), §10.
**Builds on:** `docs/research/2026-10-inochi2d-format.md` (the format facts and the full-scope checklist), ADRs 0009
(determinism), 0010 (Live2D and Inochi2D), 0011 (crate layering), 0054 (code structure).

A large share of VTubers perform as a flat, layered 2D puppet: art in layers, meshes over the layers, parameters that
bend the meshes, and pendulums for hair. The quality references for short-form video are puppets of this kind. This page
settles how Omuzaki holds, moves, draws, imports and authors them.

## 1. Crates and layering

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-puppet-api` | API | the data: `PuppetDef` (nodes, meshes, parameters, bindings, pendulums, textures, rights), `ParamMapping`, `DrawList2D`, the checks (`puppet.*` rule ids) |
| `omuzaki-puppet` | Implementation | the runtime: `Evaluator`, `Physics`, `Driver` (channels to parameters), the blend formulas, `Texture` and `CpuRenderer`, the auto-rig, bundles, tracks |
| `omuzaki-inochi2d` | Lib | the Inochi2D format: INP1 read and write, the INP2 container, the mapping into `PuppetDef`, the import report |

Reasons. The format is a lib (ADR 0011): it has no registrations and nothing but the importer needs it. The data the
stage, the renderer, the UI and the shorts see is an API crate, so they depend on a contract and not on the runtime;
`PuppetDef` is a plain serde tree with no behaviour beyond `check()`. Every other source of a puppet (layered art, a
puppet from a video, a puppet written by an agent) produces the same `PuppetDef`, so the runtime has one input.

The `puppet.*` ops (import, rig from layers, render, play a track) register next to their code in `omuzaki-puppet`'s
op module when that lands (ADR 0054); until then `short.render` reads puppets (§8) and the examples run the rest.

## 2. The model

Model space is canvas pixels, **y down**, origin at the canvas centre: Inochi2D's own axes, so an import passes
positions and UVs through unchanged. Angles are radians, clockwise on screen.

- **Nodes** are a flat list in tree order, parents before children, the root at index 0. Kinds: `Group`, `Part` (a
  mesh, a texture and its drawing state), `Mask` (a mesh that only masks), `Composite` (children drawn to one layer,
  then blended as one), `Lattice` (a bilinear grid deformer), `MeshDeformer` (a triangle-mesh deformer), `Unknown`
  (kept, not drawn). Every struct-like variant is `#[non_exhaustive]` with a constructor (#1514).
- **A node's transform** is translate, rotate, scale (`T*R*S`), and its world transform is the parent's times its own.
- **Parameters** have one or two axes, a range, a default and key points per axis. A **binding** ties a parameter to
  one node's target: `Deform` (an offset per point per key), or a scalar (`TranslateX/Y`, `Rotate`, `RotateX/Y`,
  `ScaleX/Y`, `ZSort`, `Opacity`, `TintR/G/B`). Values are stored x-major, `key_index(ix, iy) = ix * ny + iy`; keys a
  rig leaves unset are filled from their set neighbours along each axis. Interpolation is `Nearest`, `Linear` or
  `Cubic` (Catmull-Rom, which keeps keys exact).
- **Combining**: offsets, translation, rotation and zsort add across parameters; scale, opacity and tint multiply.
- **Draw order**: a larger total zsort (the node's own plus its ancestors') draws further back; ties keep tree order.
- **Pendulums** (Inochi2D's SimplePhysics): rigid or spring, anchored at a node, writing one parameter through a map
  (`AngleLength`, `LengthAngle`, `Xy`, `Yx`) times an output scale.

## 3. The runtime

`Evaluator::new(def)` checks the puppet once and precomputes every buffer: the binding table, the bins of each mesh
deformer's triangles (16 x 16), the draw order and the draw list's capacity. Each frame:

1. `set(param, value)` (clamped to the range) for whatever drives it: the mapping (§6), a track (§7), physics;
2. `Physics::step(&mut eval, dt)`: RK4 at a fixed 1/240 s with the leftover time carried, so the same inputs at any
   frame rate give the same swing (ADR 0009); a gap over 0.25 s is dropped, not integrated;
3. `evaluate()`: bindings into per-node offsets and transforms, deformers applied parents first (a point outside every
   triangle of a mesh deformer follows the nearest triangle, extrapolated), then the draw list.

`evaluate()` does not allocate (a counting-allocator test holds it to zero), and the same parameters give the same
draw list bit for bit.

**Budgets** (`cargo run --release -p omuzaki-puppet --example budget`, a reference puppet of 20 parts and 8,000
vertices with a two-axis deform on every part and two pendulums; a 4-core cloud VM with another job running, so these
are ceilings):

| Measure | Number | Spec §10 row it sits in | Gate |
|---|---|---|---|
| `evaluate()` | 0.15 ms mean, 0.31 ms p95 | `cpu.total` (8 ms of CPU a frame at 30 fps) | the allocation test; a nightly timing row when the stage draws puppets |
| `Physics::step`, one 60 Hz frame | 0.0014 ms mean | `physics.step` (0.5 ms) | as above |
| CPU draw, 1080 x 1920 | 58 ms mean | none: the CPU renderer is the reference and the offline path, not the live one | the render tests' exact pixels |

The live path draws on the GPU (§5); the CPU renderer's 58 ms is fine for shorts and tracks, which render offline.

## 4. Worked example

`crates/omuzaki-puppet/tests/worked_example.rs` builds it and asserts these numbers. Two parts on a 40 x 40 canvas:
`face` (a 20 x 20 quad at the centre, zsort 0.1, texture 0) and `blush` (16 x 4, texture 1, multiply at opacity 0.8).
One parameter, `squash` (0 to 1, keys 0 and 1), with one deform binding on `face`: at key 1 the top edge moves down 4
and the bottom edge up 2.

| `squash` | face's points (top left, top right, bottom left, bottom right) |
|---|---|
| 0 | (-10, -10), (10, -10), (-10, 10), (10, 10) |
| 0.5 | (-10, -8), (10, -8), (-10, 9), (10, 9) |
| 1 | (-10, -6), (10, -6), (-10, 8), (10, 8) |

The draw list holds 8 vertices and two meshes: the face first (zsort 0.1 is further back), indices 0 to 6, texture 0,
`Normal`, opacity 1; then the blush, indices 6 to 12, texture 1, `Multiply`, opacity 0.8.

## 5. The renderer seam

`DrawList2D` is the seam: vertices (model-space position and UV), indices, and commands in draw order: `Mesh` (an
index range, a texture, a `DrawState` of blend, opacity, tint, screen tint and whether it is masked), `MaskBegin`,
`MaskShape` (with `Mask` or `Dodge`), `CompositeBegin` and `CompositeEnd`. A renderer needs nothing of the puppet
beyond it, and a view affine (model to picture).

- **Blending** is the W3C separable formula on premultiplied, sRGB-encoded values (what 2D artists see in their paint
  program, and what Inochi2D does), plus `ClipToLower` (source-atop) and `SliceFromLower` (destination-out). The formulas
  are pure functions in `omuzaki-puppet::blend`, the reference every renderer is checked against.
- **Masks** are coverage: any `Mask` source times one minus any `Dodge` source, at the part's threshold.
- **Composites** draw their children to a cleared layer and blend the layer once, so overlapping children never double
  their opacity.
- **The CPU renderer** (`CpuRenderer`) is the reference and the offline path: band-split threads that draw the same
  pixels for any thread count, a top-left fill rule (no seam pixel drawn twice), trilinear mips with the level of detail
  per triangle from the UV Jacobian. Render tests check it against pixels computed by hand.
- **The GPU pass** in `omuzaki-render` draws the same list into the stage's target: one vertex buffer, one draw per
  `Mesh`, the blend formulas in the fragment shader against a copy of the target for the non-trivial modes, masks
  through the stencil, composites to an offscreen target. Its acceptance is the CPU renderer's pixels within 1/255 on
  lavapipe. A puppet then draws into the same target as 3D characters, before the outline pass (a puppet has no
  outline of its own beyond its art), so outputs, recording and the virtual camera (BR-01 to BR-04) need nothing new.
- **Premultiplied alpha** throughout: textures are premultiplied on load (Inochi2D's are stored premultiplied already
  and kept as they are), and the canvas converts to straight RGBA8 only at the output.

## 6. The channel mapping

`ParamMapping` is a list of entries: a parameter and axis, a channel (or a channel minus another, for the paired
ones), an input range and an output range, linear and clamped. A parameter several entries drive gets their sum.
`Driver::new(def, mapping)` resolves names once and lists what it couldn't resolve (`unresolved`), the way the BYOM
report lists what a model lacks; an unmapped parameter keeps its default, or what physics writes.

The default preset maps the shared channels (ARKit-52, head pose, the life layer's `breath` and `bodyTurn`, and the
anime extras) onto the parameter names common rigs use (`ParamAngleX/Y/Z`, `ParamBodyAngleX/Z`, `ParamBreath`,
`ParamEyeL/ROpen`, `ParamEyeBallX/Y`, `ParamBrowL/RY`, `ParamMouthOpenY`, `ParamMouthForm`, and `Expr <channel>` for
expression layers). Picture left is the character's right, as in every rig. The mapping is stored beside the puppet
(`mapping.json` in a bundle; in the document when the stage holds puppets) and edited as data: by ops, and as a table
in the UI.

Of the performer's layers, the life layer (PF-24) applies unchanged (it writes channels); calibration (PF-20) applies to
the tracker before the channels, so it is unchanged too. Lighting does not apply to a flat puppet.

## 7. Authoring: layered art, video and tracks

Inochi2D and Live2D rigs take a rigger days. Omuzaki also makes a puppet from **layered art** in one step:
`autorig(&LayeredArt)` reads a manifest (`*.layers.toml`: the canvas, then each layer's file, offset and optional role,
side, depth, blend, `show_when`, `hide_when` and `replaces`) and builds a rigged puppet: each layer cropped and meshed
on a grid that drops empty cells, roles from the layer names (`hair back`, `eye white L`, `mouth open`, `blush`, ...),
a body and a head pivot, a fake-3D head turn per vertex (depth by role, on a rounded head), blinks, gaze, brows, the
mouth, breathing, a body turn, pendulums on the hair, and expression layers shown by their channel and hiding what they
replace. `tools/puppet-fixture/make_layers.py` writes the project's own test character, Pip, in layers.

A **track** (`PuppetTrack`, plain JSON) is a performance frame by frame: per frame, a view and the parameters that
change. `track.play` renders it; the `play` example streams the frames raw to `ffmpeg` or a script. It is how a
tracker's log, an agent's keyframes or a solved video plays back exactly.

**A puppet from a video** is the reference loop's way to a pixel score against a flat-2D reference: solve each frame's
camera (a similarity transform from matched features), map the frames onto one canvas, cut the canvas into overlapping
tiles, cluster each tile's appearances into states, and write the states as parts whose opacity a parameter sets. The
camera becomes the track's views and the states its parameters, and `tools/reference-loop/parity.py` scores the result
frame by frame (PSNR, SSIM, the worst frames). A puppet made from another creator's video stays out of the repository;
the tooling is generic.

## 8. Shorts

A short file's `puppet` line (a bundle folder or a `*.layers.toml`, relative to the project) plays the script on a
puppet instead of the 3D body: the same timeline, offline performer, voice and captions; the performer's channels
through the puppet's mapping; its pendulums at their fixed step. Faces come from the built-in expression library. The
camera is 2D: a shot frames the face part (the parts under the `head pivot` named for the face, else the head without
its hair, else the top of the puppet) at 36% of the frame height for a face shot and 20% for a bust; zoom, shift, punch
and push scale and move the picture, roll turns it, and the camera's yaw and pitch turn the puppet 0.6 of the way the
other way, as an orbit would show it.

## 9. Import (Inochi2D)

`omuzaki_inochi2d::import(bytes, &Limits)` reads INP1 (and checks an INP2 container; its payload waits on 0.9's own
writer) into a checked `PuppetDef`, its textures and an `ImportReport` of findings (synthesised, dropped, approximated,
unsupported, notes), each with a rule id and a path. Every declared length and count is checked against `Limits` and
the bytes left before anything is allocated; 10,000 seeded mutations neither panic nor allocate a megabyte for a lie.
The rights block is kept verbatim in `PuppetMeta::rights`; the licence gate reads it with a conservative table (an
unknown licence is not redistributable). `write_inp1` writes our own fixtures, so no third-party puppet is in the tree.

What `PuppetDef` can't hold yet is reported, not guessed: screen-tint and emission bindings, `lockToRoot`, the
Inverse, DestinationIn and AddGlow blend modes, and how far a MeshGroup's deformation reaches through plain nodes.
Each is a leaf on the EX-08 epic.

## 10. The web build and outputs

The runtime is plain Rust with no platform code, so the web player can draw puppets with the GPU pass; the CPU renderer
is the fallback where WebGPU is missing. Outputs and recording take the stage's target and need nothing for puppets.
