---
title: "0033: An imported avatar is a performer body of its own, with morph targets in the renderer (2026-10-13)"
summary: "An imported avatar is a performer body of its own, with morph targets in the renderer"
area: decisions
---

# 0033: An imported avatar is a performer body of its own, with morph targets in the renderer (2026-10-13)

**Status:** accepted (decisions 1 and 3 with conditions). **Context:** EX-12, EX-11, LK-07,
`docs/design/byom.md` (the full design), `docs/design/io-api.md` D1, D5 and D10, `docs/design/render-api.md`
("Not in v1"), ADR 0014 (rest in T, bind as data), ADR 0016 (user-supplied ML models; avatars are not ML models).

## Context

The file side of bring-your-own-model is built: a VRM or glTF reads into an `IoScene` with its rights record. The
runtime side has no answer. `BuiltCharacter` comes from a body template and parts with a painted face; the renderer
has no morph targets; `HumanoidRig` already solves normalised poses onto any skeleton. Four choices shape everything
after: what runtime type an imported model becomes, where morphs are blended, where the avatar lives in the document,
and what an export of it reads.

## Decision

1. **A performer body of its own.** An imported model becomes an `ImportedAvatar` (new API crate `omuzaki-avatar-api`),
   not a part of a `CharacterDef`. It reuses the char-api mesh, material and texture types and the anim-api skeleton,
   pose, spring and constraint types. Everything downstream (stage, recording, export, the tracking doctor, agents)
   sees either kind through one `Body` view; there is no second performance path. It publishes anchors (its humanoid
   bones plus named anchors), so accessories, outfits and hair attach to it through the existing attachment system.
2. **The rest is the VRM T-pose; the mesh stays as authored.** Import aligns limb rests with the T axes when a file
   rests elsewhere, keeps the file's inverse bind matrices per mesh, and records the original rest as `bind`
   (ADR 0014 applied to imports).
3. **Morph targets are part of the render contract.** Sparse targets are uploaded with the mesh; weights are per-frame
   `Scene` data beside the joints, in render-api's existing dynamic-vertex direction. The GPU compute pass ships in
   the same feature and is the default where compute exists (target ≤ 0.1 ms on the reference GPU), verified against
   one CPU reference in `omuzaki-render-api`, which is also the fallback (measured: 0.3 ms with 16 of 52 targets active
   on a 20 000-vertex head, 0.8 ms with all 52; 3.6 MiB of morph data).
4. **Faces are driven through the channel table.** Each expression is bound to a channel at import (ARKit names,
   BY4's synthesised ones, emotion presets to `customNN`); a separate evaluator applies the VRM overrides and the
   spec's update algorithm each frame. An inert channel does nothing on the model, and retarget, calibration and the
   life layer do not change.
5. **It lives at `characters/<id>/avatar`.** The cast, the licence record (`sources/avatar`, `licence`) and the export
   ops keep addressing it by `CharId`; `CharacterDef::from_node` refuses such a node with a typed error, and the
   licence ops join the avatar (part id `avatar`) and its attachments.
6. **Export gathers from the runtime type.** `io.export` of an imported character gathers from the `ImportedAvatar`
   and its attachments; what the runtime does not use (MToon extras, passthrough, meta, thumbnail) is retained from
   the import and written back, so an unedited avatar round-trips and the file's terms carry through.
7. **Rights warn at import and never block performing.** `avatarPermission` other than `everyone` needs an
   acknowledgement; non-profit, credit, content and licence terms are findings; export keeps applying io-api D5.
   `docs/licensing/policy.md` gains a sentence on user-supplied avatars.

## Consequences

- New crates: `omuzaki-avatar-api` (API) and `omuzaki-avatar` (the evaluator). Additive API changes: render-api (morphs,
  capabilities), char-api (the `avatar` sibling key and the `Imported` error), `omuzaki-rig`'s `Skinner` (inverse bind
  matrices per joint run). No hub crate changes.
- The task cut BY6 to BY22 in `docs/design/byom.md` section 5; BY2 to BY5 stand as filed.
- The import report gains the `avatar.*`, `look.mtoon_not_drawn` and new `rights.*` findings, rendered in four
  sections for people.
- Rejected:
  - **An imported model as a part kind:** a part cannot replace the template's humanoid, and every template
    assumption would need an imported branch.
  - **Blending in the stage through `update_vertices`:** the deltas would live outside the renderer and a GPU path
    would need a second contract; `update_vertices` stays for simulated meshes.
  - **Dense or quantised morph data:** dense is 6.6 times the memory and 11 times slower; quantised deltas were slower
    than f32 for 1.5 MiB saved.
  - **Exporting by re-importing the stored original:** it would miss attachments and edits made on the avatar.
  - **Re-posing the mesh into the T-pose:** lossy where weights blend (ADR 0014).
