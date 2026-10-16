---
title: "Bring your own model: from an imported `IoScene` to a performing character"
summary: "from an imported `IoScene` to a performing character"
area: design
---

# Bring your own model: from an imported `IoScene` to a performing character

**Status:** D1 and D2 accepted with conditions (written into D1, D2 and D8); D4 to D6 and D8 accepted. **Feature:** EX-12. **ADR:** [0033](../decisions/0033-imported-avatars.md).
**Spec:** v3 §8 (frame order), §10 (budgets), §20 (RG-03), §21 (LK-07), §22 (PF-20, PF-24), §27 (EX-11, EX-12).
**Builds on:** [io-api.md](io-api.md) (D1, D5, D6, D9, D10), [render-api.md](render-api.md), [anim-api.md](anim-api.md)
§8, [char-face-api.md](char-face-api.md), [l10n-ops.md](l10n-ops.md) §5, ADR [0013](../decisions/0013-file-roots-per-surface.md),
[0014](../decisions/0014-rest-in-t-bind-in-a.md), [0016](../decisions/0016-user-supplied-models.md).
**Read on 2026-10-13:** the code on `main` and the issues cited.

## 1. In one minute

The file side exists: `omuzaki-io-gltf` and `omuzaki-io-vrm` read a VRM or glTF into an `IoScene`, `run_import` adds the
rights record, and `io.inspect` shows it. Nothing turns that scene into something the stage can perform. This design
adds one runtime type, **`ImportedAvatar`**, in a new API crate `omuzaki-avatar-api`. It is a performer body of its own,
not a part of a procedural `CharacterDef`, and it lives in the document at `characters/<id>/avatar`, so the cast, the
licence record and `io.export` keep addressing it by `CharId`.

```text
drop ─► io.inspect {as: avatar}  ─► prepare (io-ops): humanoid map (BY2) · T rest (D4) · cleanup (BY3) · ARKit
        (read-only, < 10 s on R)      synthesis (BY4) · materials and textures · springs, constraints, look-at
                                   ─► ImportedAvatar + one Report ─► the stage performs it while the report shows
confirm ─► io.import {as: avatar} ─► store the original, write characters/<id>/{name, avatar, sources/avatar}
each frame: channels ─► ExpressionEvaluator ─► morph weights (Scene) + colour and UV binds (update_material)
            HumanoidPose ─► HumanoidRig on the imported skeleton ─► look-at and IK ─► constraints ─► springs ─► per-mesh skinning
```

Morph targets become part of the render contract (sparse targets uploaded once, weights in the `Scene` beside the
joints). Measured below: CPU blending of a 20 000-vertex head with 52 sparse targets takes 0.3 ms with 16 targets
active and 0.8 ms with all 52, so the first implementation blends on the CPU and a GPU pass follows.

## 2. Where the existing documents disagree

| Where | Says | Resolution |
|---|---|---|
| `CharacterRig::skeleton` docs | humanoid bones rest with identity rotations | true of built characters only. An imported skeleton keeps its own bone axes; its invariant is "the rest is the VRM T-pose" (D4). `HumanoidRig` already converts any axes (`local = Wp⁻¹ · q · Wb`), so nothing in the rig changes |
| io-api D1 | "render-api has no morph targets in v1" | still true; D2 adds them |
| render-api "Not in v1" | morphs and dynamic vertex updates are one item, `update_vertices` | two items. `update_vertices` stays for meshes a simulation rewrites (hair, cloth). Expression morphs are a contract of their own, because their deltas are static and only the weights change (D2) |
| `omuzaki-io-ops` | `io.import … as: 'reference' \| 'part'` | adds `'avatar'` (D6); `'part'` stays CB-44 |
| ADR 0014 §2 | "imported models keep their own inverse bind matrices as given" | kept, and made precise: per mesh, because glTF skins may give one joint different matrices in different skins (D4) |

## 3. Decisions

### D1. The runtime type: a performer body of its own

An imported avatar is **not** a part kind of a `CharacterDef`. A `CharacterDef` names a body template that fixes the
skeleton, and parts contribute extra bones only; a VRM brings its whole skeleton, its own weights and its own inverse
bind matrices. A part would have to replace the template's humanoid, which the part contract forbids, and every
template assumption (`FaceBinding`, palette slots, `stature`, auto-weights) would need an "unless imported" branch.
CB-44 stays what it is: a mesh brought in *as a part*, auto-skinned to the template.

`ImportedAvatar` (outline in section 4) reuses the char-api types the stage already uploads, so the stage learns one
new source, not a new vocabulary:

| Field | Type it reuses | Read by |
|---|---|---|
| meshes | `PartMesh` (`MeshData`, `joint_bones` as `JointRef`, material name, shadow flag, render order) plus per-mesh inverse bind matrices and optional morphs | stage upload, `Skinner` |
| materials, textures | `MaterialSpec`, `TextureData` (overlays empty; no face atlas) | stage upload |
| skeleton, bind | `Skeleton` (rest = T), `HumanoidPose` (ADR 0014) | `HumanoidRig`, the base pose (`seed_base_pose`) |
| springs, constraints | `SpringSet` with EX-11's shapes and limits, `ConstraintSet` | the full rig |
| expressions, drivers, look-at | new, in `omuzaki-avatar-api` | `ExpressionEvaluator` (D3), the look-at stage |

**Conditions:**
1. **One body surface downstream.** A `Body` enum, `Built(BuiltCharacter) | Imported(ImportedAvatar)`, and a
   borrowed `BodyView` (meshes, materials, textures, skeleton, springs, constraints, bind, anchors, optional face
   binding, optional morphs, runtimes) are the only way the stage, recording, export, the tracking doctor and agents
   see a performer. There is no second performance path behind it: retarget, the life layer, springs and colliders,
   the look and the expressions all read the view. A built character has no morphs; an imported one has no face
   binding and no part runtimes of its own.
   `Body` and `BodyView` live in `omuzaki-avatar-api` (it already depends on char-api), not in the stage, because
   `omuzaki-io-ops` (export) and the tracking doctor read them too and may not depend on an App crate.
2. **Attachments work on both.** `ImportedAvatar::anchors()` publishes an anchor per humanoid bone plus named anchors
   (the head's top, the hands' grips, read from the skeleton the way the template places them), so the accessory
   framework and later outfits and hair attach to an imported model through the same `Attach` and anchor
   types, with no second attachment system. Attached parts are stored as `PartDef`s in `avatar.parts` (D5), built by
   the kit against those anchors, carry their own source rows, and join the `BodyView`'s meshes.
3. **Round trip from the runtime type** (D8): re-export gathers from the `ImportedAvatar`, its attachments included,
   and the import's licence terms carry through the source row.

**`omuzaki-rig`** needs one change: `Skinner` takes inverse bind matrices per joint run, not only per bone, for imported
meshes (D4).

**Why a new API crate, not char-api additions.** char-api's contract is the procedural kit (parts, kinds, templates,
the builder). The avatar contract is consumed by io-ops (which writes it), the stage and an evaluator crate, and it
changes with VRM extensions, not with the kit. A separate crate keeps char-api's churn and the avatar's apart; the
cost is one more API crate (`omuzaki-avatar-api`, depending on char-, anim-, render- and io-api).

### D2. Morph targets in the renderer

**Measured** (synthetic model, nothing third-party: a 20 000-vertex head of random positions and normals, 52 targets,
each a contiguous region with one vertex in eight skipped, 3 to 25 % of the head, 12.9 % mean, 2 578 vertices;
single thread, release build, 3 000 timed frames after 200 warm-up, two runs; class `reported` on a shared build machine,
an Intel Xeon at 2.8 GHz, never a tier number, spec §10):

| Layout | Morph data | 52 targets active, p50 / p95 | 16 active, p50 / p95 |
|---|---|---|---|
| dense, f32 position and normal deltas | 23.80 MiB | 8.86–9.01 / 14.2–14.9 ms | 2.68–2.78 / 4.5–4.9 ms |
| **sparse, u32 index + f32 deltas** | **3.58 MiB** | **0.78–0.83 / 1.30–1.60 ms** | **0.23–0.30 / 0.50 ms** |
| sparse, u32 index + i16 deltas | 2.05 MiB | 0.99–1.25 / 1.83–1.91 ms | 0.31 / 0.52–0.59 ms |

Each frame also copies the base (in the times above) and uploads 469 KiB of positions and normals; renormalising the
20 000 normals costs 0.03 ms. Quantised deltas save 1.5 MiB and cost time, so they are not worth it.

**Decision.**
1. **The contract** (render-api, additive, in the direction render-api already names for "morph targets and dynamic
   vertex updates": one `MeshUsage` family, no parallel path; `update_vertices` is its simulated-mesh sibling): `MorphTargets` (sparse: per target a sorted `u32` index list and f32
   position and optional normal deltas) is passed with the mesh at creation (`create_mesh_morphed`, or a field of the
   mesh description); `Scene.morph_weights: Vec<f32>` is filled each frame like `Scene.joints`, and
   `Instance.morphs: Option<Range<u32>>` points into it. `Capabilities` gains `max_morph_targets` (at least 128) and,
   for D7, the MToon fields the renderer draws. One CPU reference, `MorphTargets::blend_into(weights, base, out)`, is
   a pure function in the API crate: the mock, the CPU path of `omuzaki-render` and offline consumers (thumbnails, a
   posed export) share it, so conformance is exact on the mock.
2. **The GPU compute pass ships in the same feature and is the default where compute exists**: one
   dispatch per morphed mesh before `Opaque`, reading the sparse buffers uploaded once, verified against the CPU
   reference with the maximum vertex error stated, 0 allocations per frame. Target ≤ 0.1 ms on the reference GPU
   (3.6 MiB read per frame), checked on the maintainers' machine like the other real-GPU numbers.
3. **The CPU blend is the reference and the fallback** (WebGL2, any device without compute): only instances whose
   weights changed by more than 1/1024, only targets with weight above 1/512, then renormalise and write the vertex
   buffer. Its cost is the measured 0.3 ms at a typical 16 active targets (4 % of the 8 ms `cpu.total` frame, spec
   §10) and 0.8 ms with all 52.
4. **Who:** the render-api contract and the mock need no GPU; the `omuzaki-render` paths need a real GPU to check.

Rejected: blending in the stage and uploading the result through `update_vertices` each frame: the deltas would
live outside the renderer, the GPU path could not use them, and every consumer would re-implement the blend.

### D3. Driving the face

**Channel to expression.** The channel table (`omuzaki-anim-api::channel`) is the only thing producers write. Each
imported expression gets a driver at prepare time, stored in `ChannelDrivers` (an expression index per channel):

| Expression in the file | Channel |
|---|---|
| a custom expression named like an ARKit key (perfect sync), or one BY4 synthesised | that ARKit channel (`0..52`) |
| presets `blink`, `blinkLeft/Right`, `aa`…`oh` with no perfect-sync equivalent | the ARKit channels of BY4's rule table, through the synthesised expressions |
| `lookUp/Down/Left/Right` presets and `eyeLook*` expressions | by the one gaze route chosen at prepare time (below) |
| emotion presets (`happy`, `angry`, `sad`, `relaxed`, `surprised`, `neutral`) and other custom expressions | `custom00..15` in file order, named in the report, so hotkeys and scores reach them; beyond 16, listed as undriven |

**One gaze route, chosen once at prepare time** (`AvatarGaze`, stored in the avatar). The eight `eyeLook*` channels
reach the model through exactly one of these, first match wins:
1. **Bone:** the file's `lookAt` is bone type (or absent) and the model has both eye bones. The look-at stage turns
   the channels into eye rotations; no gaze expression is driven.
2. **Native ARKit:** the model has its own `eyeLook*` expressions (BY4 outcome `Present` or `FromMorph`, never
   `FromPreset`). Each channel drives its own expression directly, like any ARKit channel; look-at drives nothing,
   and the look presets are not driven.
3. **Presets:** the model has look presets. The look-at stage turns the channels into the four preset weights (the
   expression type, with the file's range maps or the defaults); the `eyeLook*` copies BY4 synthesised from them are
   not driven.
4. **Inert:** none of these; reported (`arkit.gaze_inert`).

So a symmetric gaze can never accumulate the same preset twice, or drive a preset through both look-at and a
synthesised copy. BY4's synthesised `eyeLook*` expressions still exist for export (perfect sync in other apps);
there, their gain from a symmetric preset is 0.5 per side, like smile and stretch (a re-cut of the rule table).
BY11's acceptance has one avatar per route: bone only; native gaze morphs only (no eye bones, no presets: gaze moves);
presets only (both `eyeLookUp*` at 1 give `lookUp` 1, not 2); and none (inert, reported).

Several channels may drive one expression (the largest weight wins); one channel drives at most one expression.
People and agents change a driver with `avatar.map_channel` (D6).

**Evaluation** runs in a new implementation crate, `omuzaki-avatar`, as `ExpressionEvaluator::evaluate(&ChannelBuf,
&mut MorphWeights, &mut BindDeltas)`, after the anim blend and before the renderer, on the frame thread. Hot path: no
allocation, tables built at load. Per frame, in this order (VRM 1.0 "Procedural override" and "Interaction between
override and isBinary"):
1. input: each expression's weight is its channel's value clamped to `0..1`; a binary expression steps (above 0.5 is
   1, else 0) **before** it overrides anything, so it overrides with its stepped value;
2. override factor per class: `block` gives 0 while any blocking expression is above 0; `blend` gives
   `1 − saturate(Σ weights of the blending expressions)`;
3. apply: a non-binary target is multiplied by the factor; a **binary target receiving any effect (factor below 1)
   is suppressed completely (0)**, never attenuated, so it only ever shows 0 or 1 (the spec's `happy` and `blink`
   example: a binary `blink` at 1 under `happy = 0.2` with `blend` is 0, not 0.8). The VRM classes are the presets (`blink*`; `look*`; `aa`…`oh`);
we extend them to the ARKit channels by `Region` (`eyeBlink*` with blink, `Gaze` with look-at, `Mouth` with lip sync),
which the spec leaves to the application for custom expressions. Then, as the spec's update algorithm says, morph
weights accumulate per target, colour binds add `(target − base) · w` per material colour and UV binds add to scale and
offset; a material is re-sent with `update_material` only when a value moved by more than 1/1024. The look-at's
expression mode (D4) writes the four look presets' weights into step 1, so they take part in the overrides.

**UV binds need a material field.** Neither `MaterialSpec` nor render-api's `Material` has a texture transform today,
so `update_material` could not apply a UV bind. BY7 adds `Material::uv_transform: UvTransform { scale: Vec2, offset:
Vec2 }` (identity default; applied to the UV of every UV-addressed texture of the material: base, overlays mapped by
UV, as VRM's texture-transform bind says), with a conformance check (an atlas quad shows the expected cell at offsets 0
and 0.5); BY6 adds the same field to `MaterialSpec` and to its conversion into the renderer's `Material`, both in
`omuzaki-char-api/src/geometry.rs` (BY6's fence includes it, with a conversion test); BY8 implements it in the shader.

**What a missing channel does.** An inert channel drives nothing on this model, and that is all: its value still
flows to recordings, scores and outputs (VMC out), so a take replays on a model that has the channel.
`ImportedAvatar::driven()` returns a `ChannelMask` of the channels that reach the model: expression drivers, the gaze
channels the chosen route can actually show, per direction, and the head and body channels through the skeleton.
By route:
- **bone:** both eye bones exist, and the direction's range map has an `outputScale` above 0 (the file's, or the
  default when the file has no `lookAt`);
- **native ARKit:** the channel has its own expression, and nothing else: this route bypasses look-at, so range maps
  play no part (a zero-scale range map in the file is unused, not a gap);
- **presets:** the direction's range map has an `outputScale` above 0, and the preset of that direction exists (VRM presets are optional): `lookUp` gives both `eyeLookUp*`,
  `lookDown` both `eyeLookDown*`, `lookLeft` gives `eyeLookOutLeft` and `eyeLookInRight`, `lookRight` gives
  `eyeLookInLeft` and `eyeLookOutRight`;
- **inert:** none.

A direction the route cannot show is reported (`arkit.gaze_partial`, naming the directions), never counted as driven,
so liveliness measures do not count it. BY6 tests the mask for each route and for partial sets: `lookUp` only (two
channels); `lookUp` and `lookLeft` (four); a bone route whose vertical range map has `outputScale` 0 (the four
horizontal channels); a native ARKit avatar with no eye bones, an `eyeLookUpLeft` expression and `verticalUp.outputScale`
0 (`eyeLookUpLeft` counted, nothing reported). BY11 checks that this native avatar moves for an upward gaze, and that the `lookUp`-only avatar moves for an upward gaze and stays still for a
downward or sideways one.
- **Retarget and calibration (PF-20)** do not change: calibration measures the *person* and is stored per person and
  device, so a model's gaps neither add nor skip prompts, and switching models never asks to calibrate again.
- **The life layer (PF-24)** does not change either: it writes channels, and the inert ones fall away. The liveliness
  measure and the report read `driven()`, so a behaviour that cannot show (a smile's cheek push without `cheekSquint`)
  is named, not silently counted.
- **The report** lists each inert channel once, by region, with BY4's outcome (`Missing`) and the fix: generate the
  shape (D9, task BY12) or map another expression to it.

### D4. Skeleton, humanoid and physics

**The rest becomes the T-pose; the mesh stays as authored** (ADR 0014 applied to imports). VRoid and UniVRM files
already rest in T (VRM 1.0 `tpose.md`). An FBX or plain glTF often rests in A. Prepare checks each limb (upper and
lower arms, hands, the four fingers, upper and lower legs) against its T axis (`tpose.md`: arms, hands and fingers
along ±X, legs along −Y); a bone more than 1° off has its rest rotated by the swing that aligns it, parents first. The inverse bind matrices are untouched, so the mesh keeps its
sculpted pose, and `bind` is the original rest read back through `HumanoidRig::to_humanoid`; the stage then shows the
model as authored and every pose, clip and retarget means the same as on a built character. The thumb keeps its authored
rest (ADR 0014: its azimuth is read from the rig, never assumed), with an info finding when it is more than 10° off
the spec's 45° between ±X and +Z. A correction above 10° is an info finding (`avatar.rest_normalised`).

**Inverse bind matrices are per mesh.** The converter keeps each skin's matrices with the mesh that uses it, aligned
with its `joint_bones`; `Skinner` multiplies by them per joint run.

**Humanoid map and its override.** BY2 proposes the map; overrides win. The override table is stored at
`characters/<id>/avatar/humanoid` as `{ "<humanBoneName>": { node = <index>, name = "<node name>" } }`: the index is
authoritative for the stored source hash, the name lets a re-import of an updated file re-find the node and say so
when it cannot. Bone names: imported nodes become `JointRef::Named` bones with sanitised, de-duplicated names, and the
converter keeps the node-to-bone table for export.

**At run time** the imported skeleton goes through the same stages as a built one (spec §8 order): `HumanoidRig`
(D1), look-at and IK (below), node constraints (`FullRig::with_constraints`, the VRM 1.0 order), springs with the extended colliders and limits, then skinning.
- **Look-at:** `eyeLook*` channels become yaw and pitch, the file's range maps (`horizontalInner/Outer`,
  `verticalDown/Up`) turn them into eye-bone rotations (bone route) or `lookUp/Down/Left/Right` weights (presets
  route), in the IK stage. On the native ARKit route the look-at stage does nothing (D3 chooses the route).
- **First person:** carried and round-tripped, not used: no camera views from inside the head yet. When one exists,
  it hides `thirdPersonOnly` meshes and splits `auto` meshes by head weight; filed then, not now.
- **Physics budget:** imported chains count against `physics.step` (0.5 ms) like built ones; the scheduler's existing
  step-down (fewer sub-steps) applies.

### D5. The document and the rights

An imported avatar is a character: `characters/<id>` holds `name` and `avatar` (an `AvatarDef`), and the usual
siblings (`licence`, `sources`, `variants` later, `poses`, `emotes`). `avatar` joins `CharacterDef::SIBLING_KEYS`, and
`CharacterDef::from_node` on a node with `avatar` and no `template` returns a typed `Imported` error, so procedural
ops (`part.add`, the builder) refuse it with a sentence instead of "missing field".

```toml
# characters/<id>/avatar (schema 1)
schema = 1
source = { hash = "blake3:…", format = "vrm1", file = "Ava.vrm" }   # the original, in the store
humanoid = { leftHand = { node = 41, name = "J_Bip_L_Hand" } }       # overrides only (D4)
channels = { cheekPuff = "Puff", custom00 = "happy" }                # driver overrides only (D3)
materials = { Body_00 = { shade = [0.8, 0.6, 0.6] } }                 # material edits only, over the retained source (D8)
acknowledged = { perform = "2026-10-13", by = "owner" }              # the confirm step's rights acknowledgement
[parts.hat]                                                         # attachments (D1 condition 2), PartDefs
kind = "hat"; attach = { anchor = "head-top" }                      # with source rows in sources/<part>
```

**Assets.** `io.import` stores the original bytes unchanged with the provenance D5 of io-api derives, and each image. The avatar is rebuilt from the original plus `AvatarDef` on load (import is deterministic, io-api D3); the
prepared avatar may be cached in the store, keyed by source hash, importer version and `AvatarDef`, to keep
`startup.live` (2 s).

**Licence record and source row.** `io.import` writes `characters/<id>/sources/avatar` with `row_patch` in the same
patch (io-api D10): the provenance, and as `terms` the VRM's own permissions. **`licence.derive` needs a
change:** it enumerates `CharacterDef` parts, so on an avatar node it would find none and would treat `sources/avatar`
as an orphaned row. It learns the node kind: for an imported character, the parts it joins are the avatar itself
(under the reserved part id `avatar`, kind `External`, its row at `sources/avatar`) plus each attachment in
`avatar.parts`; `set`, `describe`, `io.check_export` and `io.export` follow through the same code
(`ops::licence::findings`). Task BY22 builds it and proves that the avatar's provenance and permissions reach the
export checks.

**What warns at import** (rights findings in the report, each with a sentence and a fix; nothing blocks performing,
because performing on your own machine is not publishing):

| File says | Finding | Severity |
|---|---|---|
| `avatarPermission` is `onlyAuthor` or `onlySeparatelyLicensedPerson` | `rights.avatar_permission`: only its author (or people licensed separately) may perform as it; confirm that is you | warning, needs the acknowledgement |
| `commercialUsage` is `personalNonProfit` (0.x `Disallow`) | `rights.perform_non_profit`: do not use it on monetised streams | warning |
| licence `Other` or unknown | `rights.unrecognised_license_url` (exists): read the licence at the URL | warning |
| `modification` prohibited, or `allowRedistribution` false | `rights.import_restricted` (exists): perform as is; exports with edits stay private | info |
| `creditNotation` required | `rights.credit_required`: the credit line to put in stream titles, ready to copy | info |
| a content flag false (violent, sexual, political, antisocial) | `rights.content_restricted`, naming each | info |

Company use (`corporation` against `personalProfit`) needs to know whether the person performs as a company; that is
open question, so the report shows the term and does not judge it.

**Policy.** `docs/licensing/policy.md` needs one sentence beside "User-supplied models": an avatar a person imports is
theirs, outside the register; we never bundle or fetch one; its terms are recorded, shown at import and applied at
export; tests and fixtures never use a third-party avatar (task BY15).

### D6. The import experience

**Ops** (`omuzaki-io-ops` and a new `avatar.*` group; scopes as io-api §5):

| Op | Kind | Does |
|---|---|---|
| `io.inspect {path \| asset, as: 'avatar'}` | query | runs the whole prepare pipeline in memory, returns the `AvatarSummary`, the report and a `confirm` token; touches nothing. The confirm screen and agents read this |
| `io.import {path \| asset, as: 'avatar', id?, confirm}` | job | the same pipeline, then stores the original and images and writes the character in one patch. `confirm` must equal the token `io.inspect` returned, so a person confirms what they saw |
| `avatar.remap {character, bone, node \| null}` | edit, undoable | sets or clears one humanoid override; refuses one that breaks the parent rule (BY2's `humanoid.override_invalid`) |
| `avatar.map_channel {character, channel, expression \| null}` | edit, undoable | sets or clears one driver override |
| `avatar.describe {character}` | query | the current report against the stored source, with `driven()` |

**One report**, the io-api `Report` with every finding from every step (BY2 `humanoid.*`, BY3 cleanup counts, BY4
`arkit.*`, `avatar.*`, `look.*`, `rights.*`, the importers' `vrm.*` and `gltf.*`), grouped for people into four
sections by rule id, never by parsing text: **Mapped** (humanoid, channels present, physics kept), **Made for you**
(synthesised channels, a normalised rest, welded vertices), **Not used** (dropped data, inert channels, MToon fields
not drawn) and **Rights**. Each finding has its `text` and `hint_text` as message ids in `omuzaki-io-ops`'s
`messages!` table (l10n-ops §5, `io-ops-<rule>` and `io-ops-<rule>-hint`); the English stays in `message`. Example:
`arkit.missing` "This model has no shape for puffing the cheeks, so cheekPuff won't move it." with the hint "Generate
the shape, or map another expression to it."

**The confirm token** is a blake3 hash over the source file's content hash, the hashes of every dependency the
import read (sidecar `.bin`s and images through the `Resolver`), the `AvatarDef` and preparation options, the importer
and preparation versions, and the canonical report. A file replaced between inspect and import, or a changed option,
gives a different token and the import is refused (`io.confirm_stale`), even when the findings would read the same.

**The confirm step** is the report screen, and the avatar is already performing behind it from the live tracker:
`io.inspect` returns the prepared avatar to the stage as a preview (not in the document). Confirm writes it; cancel
drops it. A warning that needs an acknowledgement (`rights.avatar_permission`) adds one checkbox to the confirm.

**Drag and drop** is a pick (ADR 0013): the drop adds a root for that file for the session, so `io.inspect` and
`io.import` may read it with no other grant.

**Time budget, "file drop to performing in under 60 s."** Machine time on `R` ≤ 10 s for a 40 MB VRM with 20
textures, the rest left for the person to read the report:

| Stage | Budget on `R` | Measured by |
|---|---|---|
| sniff, read the file, `run_import` | 1.5 s | job progress events, timed per stage into `AvatarSummary.timings` |
| prepare: humanoid map, T rest, cleanup, ARKit synthesis, driver table | 1.0 s | as above |
| decode textures (job pool, in parallel) | 3.0 s | as above |
| store the original and images (hash and write) | 1.0 s | as above |
| upload to the renderer, first performing frame | 1.5 s | the stage's first-frame report |
| **machine total** | **≤ 8 s (2 s spare)** | the `byom_drop_to_perform` test on a generated VRM (task BY14), scaled per §10 |
| the person reads and confirms | ≤ 45 s | the BY5 acceptance run, stopwatch |

Calibration is not on this path: it is per person and device and already stored (D3); without one the default ranges
apply and the studio offers calibration later.

### D7. Fidelity gaps until LK-07 lands

The converter maps MToon onto render-api's `Toon` as io-api's interface table says, and asks the renderer what it
draws (`Capabilities`, D2). For each material and each MToon group the renderer cannot draw, one info finding
`look.mtoon_not_drawn` under **Not used**, naming the group: matcap, GI equalisation, UV animation, render-queue
offset, outline width mode (screen coordinates), the rim-lighting mix, the outline-lighting mix. Today all seven
appear when the file uses them. Each LK-07 task that draws a group sets its capability bit, and that finding disappears
by itself on that renderer; it stays on renderers without it (the mock, a WebGL fallback). Nothing in the file is
dropped: the avatar retains them for export (D8), so they round-trip whatever the renderer draws.

### D8. The end-to-end EX-11 check

`io.export` of an imported character gathers from the `ImportedAvatar` through the `BodyView` (condition
3), attachments included, into an `IoScene`, then `run_export`. What the avatar does not model at run time (the MToon
fields beyond `Toon`, passthrough extensions and extras, the meta, the thumbnail, node names and order) is kept in
`ImportedAvatar::retained`, carried from the import, and the gather writes it back, so an unedited avatar re-exports
with every field of the original. That includes a mesh no node draws: it has no runtime mesh, so its primitives keep
their geometry and morph targets themselves (`RetainedPrimitive::geometry`), exactly, and `validate` insists on it.

**Materials are retained exactly.** The runtime `MaterialSpec` holds an 8-bit sRGB `Colour`, so converting back would
nudge values (linear 0.5 comes back as about 0.502886). `retained` therefore keeps every source material as the file
had it (the io-api `Material`, linear f32 and the whole MToon block), and the gather writes that, never a value
converted back from the runtime. A person's edit to a material is recorded as an explicit override in
`AvatarDef.materials` (`{ "<material>": { "<field>": value } }`), and the gather applies those fields on top of the
retained material; an edit is never detected by comparing converted values. BY20 tests off-grid colours (0.5, 0.123,
0.9999) round-tripping bit-exact, and one edited field replacing only itself. The source row carries the file's terms, so io-api D5's export rules (VF-08)
apply. The check (task BY13), on a VRM 1.0 the test writes with `omuzaki-io-vrm` (spring chains, an inside sphere, a
plane, a cone and a hinge limit, roll, aim and rotation constraints; no third-party file):
1. import as avatar; every collider, limit and constraint appears in the runtime sets, and the report's **Not used**
   section has no physics entry;
2. 120 frames of a scripted head shake: a joint inside an inside-sphere stays inside, a hinge-limited joint stays in
   its plane, the aim constraint's target bone tracks;
3. export, re-import: the springs, colliders, limits and constraints equal the original (floats to 1e-6) and
   `io-api`'s round-trip checks pass. The UniVRM-made file of BY5 repeats steps 1 and 3 on a maintainer's machine (QG-10).

### D9. The geometry-based ARKit shape generator

BY4 reuses what the model has. The rest (cheek puff, mouth roll and shrug, squint and wide, look in and out, and
left/right halves of symmetric shapes) is made from geometry: split a symmetric morph by the sagittal plane with a
smooth falloff, and build new shapes from landmarks on the face mesh. It needs renders to judge, so it is a visual
task that needs renders to judge (BY12); its shapes arrive as synthesised expressions with `arkit.generated` findings.

## 4. The API outline (`omuzaki-avatar-api`)

```rust
pub struct ImportedAvatar {
    pub source: ContentHash,                                   // the original in the store
    pub skeleton: Skeleton, pub bind: HumanoidPose,            // rest = VRM T-pose (D4); ADR 0014
    pub meshes: Vec<AvatarMesh>,
    pub materials: BTreeMap<MaterialName, MaterialSpec>, pub textures: BTreeMap<TextureName, TextureData>,
    pub expressions: Vec<AvatarExpression>,                    // binds, is_binary, overrides; names kept
    pub drivers: ChannelDrivers,                               // [Option<ExprIx>; CHANNEL_CAPACITY]
    pub gaze: AvatarGaze,                                      // Bone | NativeArkit | Presets | Inert (D3)
    pub springs: SpringSet, pub constraints: ConstraintSet,    // omuzaki-anim-api, EX-11
    pub look_at: Option<AvatarLookAt>, pub first_person: Vec<FirstPersonFlag>,
    pub anchors: BTreeMap<AnchorId, Anchor>,                   // humanoid bones + named anchors (condition 2)
    pub retained: Retained,                                    // what export needs and the runtime does not use (D8)
}
pub struct AvatarMesh { pub mesh: PartMesh, pub inverse_bind: Vec<Mat4>, pub morphs: Option<MorphTargets> }
pub struct AvatarExpression { pub name: String, pub morph: Vec<(u16, u16, f32)>, pub colour: Vec<ColourBind>,
                              pub uv: Vec<UvBind>, pub binary: bool, pub overrides: Overrides }
pub struct AvatarDef { /* the document node of D5, serde + JsonSchema */ }
pub struct AvatarSummary { pub id_suggestion: CharId, pub driven: ChannelMask, pub timings: Vec<(Stage, Duration)> }
impl ImportedAvatar { pub fn driven(&self) -> ChannelMask; pub fn validate(&self) -> Result<(), AvatarError>; }
```

The issue's sketch held `report` inside the avatar and a `MorphRig` with per-frame weights. The report goes in the op
output instead (the runtime type is rebuilt on every load, the report belongs to the import), and the weights are
per-frame data owned by the stage and the `Scene`, not by the asset. `MorphTargets` lives in `omuzaki-render-api`
(D2) rather than `omuzaki-core`, which would make it a hub-crate change.

## 5. Task cut

Full blocks (fence, contract, acceptance, blocked by) are filed with the tasks. BY1 is this design;
BY2, BY3 and BY4 stand as written; BY5 is the maintainers' acceptance run.

| Id | Task | Blocked by |
|---|---|---|
| BY6 | `omuzaki-avatar-api`: `ImportedAvatar` (with `anchors`, `retained`), `Body`, `BodyView`, `AvatarDef`, drivers, look-at, validation; `avatar` sibling key and the `Imported` error in char-api (`character.rs`); `MaterialSpec::uv_transform` and its conversion (`geometry.rs`) | this design |
| BY7 | render-api: `MorphTargets`, `Scene.morph_weights`, `Instance.morphs`, capabilities; CPU reference; mock and conformance | this design |
| BY8 | `omuzaki-render`: the GPU compute pass (default) and the CPU fallback, checked against the reference | BY7 |
| BY9 | `omuzaki-io-ops` `prepare::tpose` (D4) | BY2 |
| BY10 | `omuzaki-io-ops` `prepare::avatar`: IoScene → `ImportedAvatar` (meshes, per-mesh matrices, sparse morphs, materials with `look.*` findings, textures, drivers, springs, constraints, look-at, `retained`) | BY6, BY7, BY2, BY9 |
| BY11 | `omuzaki-avatar`: `ExpressionEvaluator` with bench and allocation test | BY6 |
| BY12 | geometry-based ARKit shape generator (D9) | BY4 |
| BY13 | EX-11 end-to-end check (D8) | BY10, BY16, BY20 |
| BY14 | `byom_drop_to_perform` timing test on a generated VRM | BY16, BY17 |
| BY15 | `docs/licensing/policy.md`: the user-supplied avatars sentence | none |
| BY16 | stage: performs through `Body`/`BodyView`, morph weights, evaluator, look-at, per-run inverse binds in `Skinner` | BY6, BY7, BY11 |
| BY17 | ops: `io.inspect`/`io.import as: 'avatar'`, `avatar.remap`, `avatar.map_channel`, `avatar.describe`, source row | BY10 |
| BY18 | the plain-language report: sections by rule id, `messages!` entries with en text for every finding of the pipeline | BY17 |
| BY19 | drop-a-file UI: drop, live preview behind the report, confirm, acknowledgement | BY17, BY18 |
| BY20 | `io.export` of an imported avatar: gather from `ImportedAvatar` and `retained` through `BodyView` (D8) | BY10, BY16 |
| BY21 | attachments on imported avatars: `anchors()`, `avatar.parts` built by the kit, source rows (condition 2) | BY6, BY16 |
| BY22 | licence integration for imported avatars: `licence.derive`/`set`/`describe` and the export checks join the avatar and its attachments (D5) | BY6, BY17 |

Rejected for this cut, each with its reason: first-person at run time (no camera views from inside the head; D4);
a node-compacting cleanup (BY3's own note); quantised morph deltas (measured slower, D2).

## 6. Sources

Read 2026-10-13 (`master` of <https://github.com/vrm-c/vrm-specification>, `specification/VRMC_vrm-1.0`):
`tpose.md` (definitions 1.4, 1.6 to 1.8: arms, hands and four fingers along X, the thumb at 45° between X and +Z),
`expressions.md` ("Procedural override", "Interaction between override and isBinary", "Expression update algorithm":
inputs clamped to 0..1, morph binds accumulate, colours add the difference from the base), `lookAt.md` and
`firstPerson.md` ("MeshAnnotation.Auto Algorithm"). How UniVRM and three-vrm apply the overrides to custom expressions
was not checked (the spec leaves it to the application). The measurement program uses no asset
and no dependency.
