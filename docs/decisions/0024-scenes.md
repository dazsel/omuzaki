# 0024: A scene is a folder of materialised values in subject units, resolved off the frame path (2026-10-12)

**Status:** accepted (2026-10-12). **Context:** ST-01, LB-12; spec v3 §7, §8, §21 (LK-04,
LK-05, LK-08), §24, §26, §28; ADR 0014 (bind pose), ADR 0019 (library items, the reserved `scene` target), ADR 0020
(agent sessions); real stature, one record per gesture, shadows. Design:
`docs/design/scenes.md`.

## Context

LB-12's light rigs apply to a scene under ADR 0019, and no scene contract existed: the spec has one line
(`scenes/<id>.toml: stage, props, lights, cameras`), the stage has a light-rig library and golden-manifest recipes but
no scene node, and the library refuses the `scene` target until one exists. Five facts shape the answer:
- Characters are now built at real stature, from a 0.97 m soft-sd to a 1.71 m adult. The seven rigs and the
  prototype's camera views were tuned in metres on one 0.966 m figure.
- ADR 0019 materialises items: building never reads the library, so a project changes only when it is updated.
- The render contract is immediate: the stage fills a `Scene` every frame, and `Scene::clear()` keeps capacity so the
  fill can be allocation-free (render-api D1). It allows eight lights and one shadow-casting directional light.
- The project store decides files by content: an object holding a plain value is one file, an object of objects is a
  folder (`omuzaki-doc` rules 1 to 3).
- Several people and agents edit one project (ADR 0020), and the CRDT of phase 5 mirrors the document tree.

## Decision

1. **A scene is the folder `scenes/<id>/`:** `scene.toml` (name, subject, opening camera, backdrop, ground, post),
   `lighting.toml` (the light rig as values), `cameras/<id>.toml`, `cast/<member>.toml`, `props/<id>.toml` and
   `applied/<app>.toml` (library records, the only sibling key). Ids are stable; `project.default_scene` names the
   scene a project opens in. Spec §7 shows this layout.
2. **Lighting is materialised.** `stage.set_light_rig` copies a rig's values into `lighting`, keeping the rig's name
   only as information. A light-rig file is the same `Lighting` type plus metadata, so the seven existing rig files are
   converted once and applying a rig is a copy.
3. **Lighting and framing are in subject units.** Lights and free cameras have an anchor (`stage`, `subject`, or
   `member:<id>`). With a member anchor, lengths are given for that member scaled to 1 m tall, and point and spot
   intensities scale with its height squared, so a rig lights every stature the same. Cast positions and props stay in
   metres. Directional lights are stored as azimuth and elevation.
4. **Cameras are framing presets measured on the subject** (`face`, `bust`, `full`, `hands`) from landmarks of the
   built character in its bind pose, or free cameras. One framing function serves scene cameras, `render.preview`,
   library previews and the sheet.
5. **The types live in a new API crate, `omuzaki-stage-api`** (`SceneDef`, paths, keys, `check`, limits, the pure light
   resolve). The ops, the rig library, landmarks, framing, resolve and apply live in `omuzaki-stage`.
6. **One op per intent, one patch per op.** The `stage.*` ops write only under the scene they name, carry their
   consequences in the same patch (the shadow moving, anchors re-pointed, the default moving), touch only the changed
   leaves, and continuous gestures are one transaction. The viewport's own navigation is view state.
7. **Library items target `scenes/<id>`.** `$scene` defaults to the default scene; slot `light-rig` owns `lighting`;
   slot `stage` owns the backdrop, the ground and what it creates, never the lighting.
8. **Render input is resolved on change and filled every frame.** Resolve (may allocate) produces fixed-capacity
   render-api values; apply on the render thread touches the renderer only for changed resources and allocates nothing
   for a value-only change; the per-frame fill copies values into the reused `Scene` and never allocates or locks.
9. **Every project has a default scene,** a starting point whose look is judged on renders: `main`, the
   studio rig, a shadow-catcher ground, the `full`, `bust` and `face` cameras (opening on a three-quarter full body)
   and the default character. New-project paths create it; a project without a scene renders it unsaved.
10. **The skin floor stays with the material** (per skin tone); a scene may override it, and no seed rig does.
11. **Spot lights, fog, image-based light, props and image or video backdrops are additive phase 2 fields**, defined
    here so LK-04 rigs and LB-14 presets can rely on their shape.

## Consequences

- One new API crate and its phase 1 tasks; the light-rig and scene-target tasks are re-cut onto it. The library ships five rigs in phase 1 and the
  coloured-spot "stage" rig waits for LK-08's spot lights. The task cut is `docs/design/scenes.md` §6.
- The light-rig files change format once; `LightRig` and its prototype-shaped structs are removed, and the
  golden-manifest recipes resolve at the 0.966 m reference height so the JS-parity goldens keep their numbers.
- `omuzaki-render-api` grows `Light::Spot`, a fog and an environment on `Scene` (LK-08), and gradient and texture
  backgrounds, in their own tasks.
- The framing and light-range questions are answered by decisions 3 and 4.
- A render's export gate must join scene records, scene assets and the cast's licences (a phase 2 task).
- The store test for this layout surfaced that characters save as one file; scenes don't depend on its fix.
- **Rejected:**
  - **A rig by name, resolved at render time:** the library would become a build dependency, a retuned rig file would
    change saved scenes, and a light couldn't be edited per scene (ADR 0019's live-link rejection).
  - **Metres everywhere:** every rig and camera preset would fit one stature; a template switch or a second character
    would need re-lighting by hand.
  - **One file per scene** (the spec's line): a rig change, a camera edit and a cast change would all rewrite one file,
    and library records couldn't be files of their own.
  - **A nested scene graph:** anchors give the one level of parenting staging needs (a light or prop on a member or a
    bone), patch and merge per entity, and map onto the CRDT's movable tree later.
  - **The live camera and live scene in the document:** switching on air is not an undoable edit (ST-04).
  - **The rig's skin floor replacing the material's:** it would undo the per-tone tuning of skin shadows.
