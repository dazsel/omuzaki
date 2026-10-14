# VRMA export: animation in the io contract, the `VRMC_vrm_animation` mapping, and clip and emote conversion

**Status:** proposed. **Feature:** EX-16, spec v3 §27 (export and interop); builds on RG-06 (the pose library), SH-07 (emotes) and the hips-scale helper (`scale_hips`).
**Decision record:** the contract decision is `docs/design/io-api.md` D11 (additive, so no ADR). This page holds the format facts and the conversion.
**Builds on:** `omuzaki-io-api` (`Exporter`, `run_export`, `HUMAN_BONES`, `ExpressionName`), `omuzaki-anim-api` (`HumanoidPose`, `Skeleton`, `Score`, ADR 0014), `omuzaki-gltf` (the glTF document model, ADR 0011), `omuzaki-io-vrm`.
**Read on 2026-10-12** (specifications change; check dates): the sources in §8, at the commits and tags named there.

This page explains the decisions so that an implementer or a reviewer can disagree with a reason. The contract types are in `io-api.md` D11 and are not repeated; the converter's types are in §5.

## 1. What this covers

| Asked for | Where |
|---|---|
| The data: `IoAnimation`, a field on `IoScene` or a separate exporter trait | `io-api.md` D11 |
| The format facts: `VRMC_vrm_animation` 1.0, rest-pose node tree, bone mapping, expressions as node translations, look-at, `specVersion`, what other apps need | V1 to V7, §4 |
| Conversion from `PoseClip` and emotes: frame rate, key reduction, hips scaling across statures | V8 to V10 |
| What cannot be expressed, and what happens to it | V11 |
| Rights: provenance and licence an exported motion carries | V12, `io-api.md` D11 |
| The EX-16 tasks | §5 |

**Not covered, on purpose:**
- **VRMA import** (RG-08). It yields clips and belongs with `omuzaki-anim`; it would be an `AnimationImporter` against the same `IoAnimation` (D11). V10 says what it must do with the hips.
- **BVH and FBX motion**, glTF animations in model exports (Unity, Godot): `IoAnimation` is built so they can reuse it, nothing here builds them.
- **Score channel tracks and face tracking data** (V11).

## 2. What a `.vrma` looks like

One GLB holding one animation, no mesh. A clip of the wave, abridged (`...` is the other bones):

```json
{
  "asset": { "version": "2.0", "generator": "Omuzaki 1.0", "copyright": "Omuzaki contributors",
             "extras": { "omuzaki": { "licence": "CC0-1.0", "commercial": "ok", "source": "authored", "looped": false } } },
  "extensionsUsed": ["VRMC_vrm_animation"],
  "extensions": { "VRMC_vrm_animation": {
    "specVersion": "1.0",
    "humanoid": { "humanBones": { "hips": { "node": 0 }, "spine": { "node": 1 }, "...": {} } },
    "expressions": { "preset": { "happy": { "node": 53 } }, "custom": { "omuzaki_smug": { "node": 54 } } },
    "lookAt": { "node": 55, "offsetFromHeadBone": [0.0, 0.06, 0.0] } } },
  "scene": 0, "scenes": [{ "nodes": [0, 53, 54, 55] }],
  "nodes": [ { "name": "hips", "translation": [0, 0.94, 0], "children": [1, 7, 11] },
             { "name": "spine", "translation": [0, 0.1, 0], "children": [2] }, "...",
             { "name": "expression_happy" }, { "name": "expression_omuzaki_smug" }, { "name": "lookAt" } ],
  "animations": [ { "name": "wave",
    "channels": [ { "sampler": 0, "target": { "node": 0, "path": "translation" } },
                  { "sampler": 1, "target": { "node": 3, "path": "rotation" } },
                  { "sampler": 2, "target": { "node": 53, "path": "translation" } } ],
    "samplers": [ { "input": 0, "output": 1, "interpolation": "LINEAR" }, "..." ] } ],
  "accessors": [ "... FLOAT, SCALAR input with min and max; VEC3 / VEC4 outputs" ], "bufferViews": [ "..." ], "buffers": [ "..." ]
}
```

## 3. Decisions

### V1. One animation per file, one GLB, `extensionsUsed` but not `extensionsRequired`

Format id `vrma`, extension `.vrma`, media type `model/gltf-binary` (a VRMA is a GLB; no media type of its own is registered, so the container's is used).

- **One animation.** The specification says that as a general rule the first animation in `animations` is loaded and that implementations *may* support several (VRMA README, "Animations"). One animation is what every player reads, so a file holds one clip and an emote pack is several files.
- **`extensionsUsed`, not `extensionsRequired`.** three-vrm's loader only reads the extension when it is listed in `extensionsUsed` (`VRMAnimationLoaderPlugin.afterRoot`), so it must be there. The specification does not say whether it is required; we leave it out of `extensionsRequired` because the file is still a valid glTF animation over a node tree for a viewer that does not know the extension, and a required unknown extension would make such a viewer refuse it. This is our decision (a project decision), not the spec's. What the two reference implementations do: three-vrm only *reads* (it requires `extensionsUsed`, above) and writes nothing. UniVRM 0.131.0's `VrmAnimationExporter` serialises the extension into the root `extensions` and its source contains no `extensionsRequired`; whether its glTF layer lists the extension in `extensionsUsed` was not confirmed in the files read.
- **`specVersion` is `"1.0"`.** The spec says it must be (VRMA README, `specVersion`). three-vrm 3.5.5 accepts `"1.0"` and `"1.0-draft"` and warns on anything else (`POSSIBLE_SPEC_VERSIONS`).

### V2. Coordinates and units: glTF space, written as they are

Right-handed, +Y up, the model's front toward +Z, metres, unit quaternions as `x, y, z, w` (glTF 2.0, "Coordinate System and Units"; the VRM T-pose definition 1.1, "Standing straight toward the +Z axis"; `tpose.md`). This is the space `IoScene` is in (io-api D2), so no conversion exists in the exporter. The model's left (`leftUpperArm`) is +X, which is why a yaw toward the left is positive (V7).

VRM 0.x motion (which faces -Z and has other T-pose rules) is a different input: the 0.x model exporter rotates 180 degrees about Y (D2). A 0.x VRMA does not exist (the animation specification starts at 1.0), so nothing here depends on it.

### V3. `HumanoidPose` is already the VRMA space: rotations go through unchanged

`HumanoidPose` is "in the normalised space of VRM Animation: parent-local rotations where identity is the VRM T-pose" (`omuzaki-anim-api` `skeleton.rs`, `anim-api.md` §8). The VRMA specification's own pose-compatibility document defines the same thing: `NormalizedLocalRotation` is "pose data that looks the same ... when applied to a model with no rest rotation", and a player converts it to its model with `B.LocalRotation = L * W^-1 * Normalized * W` (`how_to_transform_human_pose.md`, non-normative). So the exporter:
- writes every rest node with **identity rotation**, which the specification allows ("the hierarchy of T-pose can have any rotation") and which makes `W` and `L` identity in every player (the formulas collapse to `B.LocalRotation = Normalized`, so there is nothing for a player to get subtly wrong);
- writes `HumanoidPose` rotations unchanged.

This holds for every Omuzaki character because decision 0014 fixes the skeleton's rest as the VRM T-pose and keeps the A-pose in the bind matrices, which are not in the file. An **imported** avatar with non-identity rest rotations is also exported correctly, because the clip is normalised: only the *positions* of its rest bones are read (V4), never its rotations.

### V4. The rest tree and `humanBones`

**What is written.** One node per humanoid bone the character has, **except the eyes**, in `HUMAN_BONES` order (parents before children), each a child of its nearest present humanoid ancestor, with `translation` = the bone's rest world position minus its ancestor's (the hips: its world position), identity rotation, no scale. The hips is the only humanoid scene root.

`humanBones` maps each of those nodes. The specification:
- requires `hips`, `spine`, `head`, both legs, both arms and hands ("if the bone is required in `VRMC_vrm`", README "humanoid.humanBones.(Humanoid bone name)"), and says the whole hierarchy of the humanoid is included "for future extensibility";
- forbids `leftEye` and `rightEye` ("cannot be defined"); the JSON schema's `humanBones` has 53 properties, the 55 bones minus the eyes. They are handled by look-at (V7).

**Why the rest tree matters (what a player needs).**
- three-vrm 3.5.5 reads the hips node's **world position in the file** and uses its `y` as `y_src`: `scale = humanoid.normalizedRestPose.hips.position[1] / vrmAnimation.restHipsPosition.y`, applied to every hips translation key (`createVRMAnimationHumanoidTracks`). It warns when that `y` is below 1 mm ("might violate the VRM T-pose"). So the file must carry the true standing hips height, and `anim.rest_hips_height` makes zero an error.
- The specification states the same idea: scaling by `y_dst / y_src`, "the height of the hips as the length of the leg" (`how_to_transform_human_pose.md`, "About translation scaling", non-normative).
- Nodes the animation never moves still need real translations, since a player may use the bone lengths to retarget (the README: "advanced animation retargeting like Unity's Humanoid ... if this is difficult, you can simply transfer it as FK animation").

**Bones the character lacks.** A character with no `upperChest` or finger bones simply has no such nodes. A clip track on a bone with no rest node is dropped at conversion with `anim_export.bone_not_in_rest` (a warning naming the bone), and `IoAnimation::check` refuses the case if an importer builds it (`anim.track_without_rest`). The specification's guidance for the receiving side (apply a missing bone's rotation to its children) is the player's job.

**What of the character is in the file.** Only bone *positions*: proportions and the hips height. No mesh, texture, material, name or licence record of the character. This is the "the character is not in the file" of the issue; the proportions are the one character-derived datum, listed under Q2.

### V5. Tracks: LINEAR, float32, the three legal paths

- **Bones:** `rotation` only on humanoid nodes. The specification says a humanoid bone's animation "must not include scales" and "must not include translations for bones other than the Hips" (README, "Humanoid"). The `Track` type cannot express either.
- **Hips:** `translation`, in metres, the node's absolute local translation (`rest_hips + HumanoidPose::hips_offset`). A glTF channel *sets* a node property, it does not add to it; and three-vrm applies the hips parent's world matrix to the values, which is identity here because the hips is a scene root.
- **Interpolation:** `LINEAR` is the default. The README says that "considering that the animation is interpolated linearly in the implementation, 30 FPS should be sufficient". For a rotation the glTF specification's `LINEAR` means spherical interpolation with the sign of the dot product taken into account (glTF 2.0 Appendix C, "Spherical Linear Interpolation"), so the shortest arc is always taken. `Interpolation::Step` exists in the model, but **the VRMA exporter never writes `STEP`**: three-vrm 3.5.5 rebuilds humanoid tracks as `new THREE.QuaternionKeyframeTrack(name, times, values)` with no interpolation argument (`createVRMAnimationClip.ts` line 27) and expression tracks as `new THREE.NumberKeyframeTrack(..., times, values)` (`VRMAnimationLoaderPlugin.ts` line 270), so a `STEP` sampler plays as a linear ramp. The exporter therefore writes every track `LINEAR` and encodes a hold as **two keys an epsilon apart**: for each key `i > 0` of a `Step` track whose value differs from the previous one, it inserts `(t_i - e, v_{i-1})` before `(t_i, v_i)`, with `e = min(1e-4 s, (t_i - t_{i-1}) / 2)` computed in `f64`. The hold time is then rounded to `f32` **once**, and the rounding can land on a neighbour: `t_i - e` rounds to `t_i` when `e` is under half an `f32` ulp. So the exporter checks `f32(t_{i-1}) < h_i < f32(t_i)`, and when that fails it takes `h_i` as the largest `f32` below `t_i`. If that is still not above `f32(t_{i-1})`, no `f32` lies strictly between the two keys and the hold cannot be encoded: the exporter **refuses the track** with `anim_export.step_unrepresentable` (an error, not a warning, since a clip with the hold missing plays wrong). Example: keys `[0, 0.9999999403953552, 1]` with weights `[0, 0, 1]` is refused, because the midpoint rounds to `1`. The converter's own keys are at least 1/120 s apart, which is many `f32` ulps even at 600 s, so this refusal is reachable only from an importer's `IoAnimation`. The hold width is `w = t_i - h_i`, and the exporter checks `w <= B` with `B = 1e-4 s + ½ ulp_f32(t_i)`, the most that nearest rounding can give. A track that fails the check is refused with the same `anim_export.step_unrepresentable`. The check is needed because the fallback to the next `f32` down is a whole ulp wide: `[0, 3599, 3600]` with weights `[0, 0, 1]` falls back to `h = 3599.999755859375`, a 0.244 ms hold against a 0.222 ms bound, so it is refused. The 600 s cap is a limit on converter input, not on an imported `IoAnimation`, so the width check is what keeps the bound true for any duration. A `Step` track that passes is accepted, and every player shows a hold of width at most `B` (about 0.108 ms at 200 s, 0.13 ms at 600 s). plays a held expression in three-vrm and asserts the held value is reached within 0.2 ms of the key and unchanged until the next. `CUBICSPLINE` is not offered: nothing in the specification or the players' loaders promises it plays as authored, and a sampled linear clip is what the spec recommends.
- **Component type:** `FLOAT` for all outputs. glTF also allows normalised `short` quaternions, which halve the size and cost about 3e-5 of precision; the conformance bound is 1e-4, and a 60 KB file is not a size problem.
- **Quaternion sign continuity** is kept anyway (V8). The glTF formula tolerates a sign flip, but players and tools that lerp componentwise do not, and the export op's tests expect it.

### V6. Expressions: a node per expression, weight in `translation.x`

The specification: "among the animations for the nodes specified as expressions, the X component of the translation is treated as the animation data for the weight"; weights lie in `[0, 1]` and a player clamps outside values (README, "Expressions").

- A preset is under `expressions.preset` by its VRM name; anything else under `expressions.custom` by its name, and a custom name must not equal a preset (README). `ExpressionName` (io-api) already guarantees both.
- The four presets `lookUp`, `lookDown`, `lookLeft`, `lookRight` "cannot have animation data" (README). They are look-at (V7). `anim.expression_reserved` refuses them.
- The output is `VEC3` `(weight, 0, 0)`; three-vrm reads index `3 * i` and ignores the rest. A node that is an expression target carries **only** `translation` channels: three-vrm throws `Invalid path` on any other path for an expression node.
- **Naming a Omuzaki expression: one shared function.** `named_expression(id: &ExpressionId) -> ExpressionName` in `omuzaki-io-ops::convert::channels` (next to `expression_name(Channel)`, which names channels and is unchanged) is used by **both** the VRM and the VRMA exporter, so a name in a VRMA is always the name in the VRM. It returns a preset when the id equals a preset name (`happy`, `angry`, `sad`, `relaxed`, `surprised`,...), else the custom name `omuzaki_<id>` (`extra_expression`, io-api D6). Today's VRM export keeps its names exactly (presets, the 52 ARKit names, the extras table), but it does **not** yet write a character's own named expressions as custom expressions (`gather/avatar.rs` gathers the 18 presets with empty binds, and the baker adds channels); task adds the function and that writing, and blocks the emote conversion. If the function returns a `look*` preset the VRMA converter drops the track with `anim_export.expression_reserved` (warning), since a VRMA cannot animate those. A preset plays on every VRM; a `omuzaki_` name only on a model that defines it, and other apps ignore it. The converter says so (`anim_export.expression_custom`, info).
- **What the weight is.** The cue's envelope (fade in, hold, fade out, with the cue's `Ease`), sampled. The Omuzaki expression's `IntensityCurve` and its channel deltas are *not* in the file: they define how a weight looks on Omuzaki's face, while the receiving model's own binds decide that on its face (VRM binds interpolate linearly, io-api D6).
- **Players' limits.** UniVRM 0.131.0 reads custom expression tracks into fixed slots `custom_00` to `custom_99`, so it plays at most 100; `anim.custom_expression_limit` warns above that. A Omuzaki emote has one or two.

### V7. Look-at: one rotation node, yaw about Y and pitch about X

The specification: `lookAt.node` is a node whose **local rotation** is the gaze direction; the player converts it to yaw and pitch "extrinsic ZXY", Y the yaw and X the pitch; the initial gaze is toward +Z; `offsetFromHeadBone` is the gaze origin in the head's rest frame (README, "LookAt"). The JSON schema lists `lookAt.node` as required (the README table says "No"; the schema is the stricter, so we always write it, and `lookAt` as a whole is only written when there is a look-at track).

**Sign convention** (from `VRMC_vrm-1.0/lookAt.md`: "Yaw: Z->X direction => Left; Pitch: Y->Z direction => Down", right-handed, +Z forward). A rotation about +Y by `+a` carries Z toward X, so a positive yaw looks toward the model's left (+X). A rotation about +X by `+a` carries Y toward Z and so carries the forward vector Z toward -Y, so a positive pitch looks down. Therefore:

```
q = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0)        // glam: Ry(yaw) * Rx(pitch) * Rz(0) = extrinsic ZXY
yaw   = atan2(d.x, d.z),  pitch = atan2(-d.y, hypot(d.x, d.z)),  d = gaze direction in the head's normalised frame
```

This follows the specification's `CalcYawPitch` and is derived by us from the two texts; **the conformance task (§5) checks it against three-vrm** (a +0.3 yaw and +0.2 pitch fixture) before anyone relies on it.

**Where it comes from.** Omuzaki's face is drawn and its gaze is channels (`eyeLook*`), not eye bones, so most clips carry none. `HumanoidPose` has `leftEye` and `rightEye`, and an imported motion (RG-08) can drive them. When either eye is not identity at some sample, the converter writes a look-at track from the **mean of the two gaze directions** (`q_eye * +Z`, normalised), not a mean of quaternions; if the eyes differ by more than 2 degrees it warns (`anim_export.eye_asymmetry`), because VRM look-at is one direction for both eyes (`lookAt.md`: both eyes look the same way, cross-eye is not expressible). No eye rotation track is ever written.

`offsetFromHeadBone` is the character's `lookAt.offsetFromHeadBone` when the converter is given one, else `[0, 0.06, 0]`, the value the VRM specification uses in its `lookAt` example. A player that ignores the offset loses nothing (README implementation note).

### V8. Converting a clip: 30 fps, continuous, reduced without loss

`clip_to_animation` (§5) turns a `PoseClip` (`sample(t) -> HumanoidPose`) into an `IoAnimation`.

1. **Sampling.** `t_k = k / fps` in `f64` for `k = 0..=floor(duration * fps)`; then the final sample at exactly `duration` is added **only if** `f32(duration)` is greater than the `f32` of the last grid time, and otherwise *replaces* it (so `duration = 0.1` at 30 fps gives keys at 0, 1/30, 2/30 and 0.1, never two keys at `f32(0.1)`). Times are rounded to `f32` once, **then coalesced**: any sample whose rounded time is not strictly greater than the previous one is dropped, the later one winning at the end, and key reduction runs on the coalesced times, so `anim.time_order` cannot fire from the converter. A regression for it (0.1 s, 1/30 s and 1/120 s clips at 30 and 120 fps) is. **Default `fps = 30`**, allowed 1 to 120. Reason: the specification's own guideline (README, "Animations": 60 and 30 are common in games, 24 in video, and 30 is "not too high or too low"), and players interpolate linearly, so doubling the rate buys little. The reduction below removes what is redundant, so a higher rate costs file size only where the motion is fast. The converter measures the cost of linear playback and reports it: `anim_export.sampling_error` is the largest angle between the exact clip and the linear reconstruction at 4x oversampled times (a wave's overshoot is where it shows).
2. **Which bones.** `BoneSelection::Driven` (default): a bone gets a track when its rotation differs from identity by more than the tolerance at any sample. A pose in the library is sparse (only bones that turn are listed, `skeleton.rs`), and a track for an unturned bone would force it into the T-pose in an app whose own idle should keep it. `BoneSelection::All` writes every bone of the rest tree (a full-body clip such as a walk wants it). The hips track is written when `hips_offset` is non-zero at any sample.
3. **Continuity.** First key: the first non-zero component of `(w, x, y, z)` positive; then every later key is flipped if its dot with the previous is negative. Players that do componentwise lerp (and the 1e-5 test) need it; it changes no rotation.
4. **Reduction: lossless to the tolerance.** Ramer-Douglas-Peucker per track on (time, value): keep the first and last key; keep the interior key with the largest error against the interpolation between the kept neighbours if that error exceeds the tolerance, and recurse; ties pick the lowest index. The error is the angle between quaternions for a rotation, the Euclidean distance for the hips (metres), the absolute difference for a weight. **Tolerances default to 1e-6** (rotation in radians), so reading the file back at any sample time gives the clip's value to far better than the 1e-5 that asserts, and the reduction only removes keys that a straight line (or slerp) already reproduces: holds, linear ramps, constant bones. `Tolerance` is a parameter for later "smaller file" modes; the op does not expose it yet. **Every track keeps a key at 0 and at `duration`**, so the animation's length does not depend on which bone moves last, and a constant bone is two keys.
5. **Loops.** `PoseClip::looped` sets `IoAnimation::looped`, carried as `asset.extras.omuzaki.looped`. glTF and VRMA have no loop flag; the player decides. The loop seam is continuous because the pose library requires it (C1), so the last key meets the first.
6. **Limits.** Duration at most 600 s and `fps` 1 to 120, so a track has at most 72,000 keys; `Limit` errors before any allocation.

A clip's easing (`easing`) is applied by the clip's own `sample`, so the exported keys already follow it.

### V9. Converting an emote: a pose hold and an expression envelope

An emote is a record `{ expression, pose?, framing }` (library D1): a still, for stickers. EX-16 needs it as motion, so the converter builds the short animation that the intent `emote` (AP-04) would play, from `EmoteMotionInput` (§5):

- total length `fade_s + hold_s + fade_s` = 0.2 + 1.6 + 0.2 = **2.0 s**, the cue defaults (`CueKind::Expression`, `fade_s` 0.2) and a 1.6 s hold that reads on a sticker-length loop;
- the pose, when there is one: keys `(0, REST), (fade_s, pose), (fade_s + hold_s, pose), (total, REST)` with `Ease::OutCubic`, run through the clip path (V8), so bone tracks exist only for the bones the pose turns;
- the expression: one weight track `0 -> intensity -> 0` with the same fades and ease, named by `named_expression` (V6);
- `framing` (`face`, `bust`, `full`) is a camera instruction; it is dropped with `anim_export.framing_dropped` (info).

The emote starts and ends at REST, which is the T-pose for the bones it turns. A player blends that in and out with its own idle (three-vrm's `AnimationMixer` cross-fades), the same way Omuzaki's clip fade does with weight (V11). `looped` is false.

### V10. Stature: write the character's real rest, do not pre-scale the hips

A clip recorded on a tall character and played on a short one makes the short one float, unless the hips translation is scaled by `y_dst / y_src` (the specification's non-normative advice). The players do that **themselves from the rest tree in the file** (V4; three-vrm 3.5.5 does exactly this). So the exporter:
- writes the source character's real rest hips height and real bone offsets;
- writes the hips translation in that character's metres, **unscaled**;
- never invents a reference stature.

*Why not normalise to a canonical stature* (a fixed 1.0 m rest tree and hips values pre-multiplied): it is equivalent for a player that scales (`(y_dst / 1.0) * (1.0 / h_src) = y_dst / h_src`), but a player that does not scale then gets motion in the wrong metres, the file lies about the source, and `y` of a canonical rest still has to be above 1 mm for the warning. Writing the truth is simpler and needs no constant.

**The import side (RG-08) must do the same in reverse:** read `y_src` from the VRMA's rest tree and call the `scale_hips(&mut HumanoidPose, from_height, to_height)` helper that the hips-scale work provides. That work stays the home of the helper; this design adds no code to it and reads no value from it.

### V11. What cannot be exported, and what happens to it

A VRMA is absolute poses of 53 bones, one hips translation, weights on named nodes and one gaze direction, on a glTF timeline. Everything else that Omuzaki's clips, emotes and scores carry has no place in it. Nothing the op can be given is dropped silently: each row says its finding. Rows for score input have none today, because the op takes a clip or an emote, never a score; they name the finding a later score export must add.

| Omuzaki feature | In the file? | What happens | Finding |
|---|---|---|---|
| Rotations of the 53 bones; hips translation | yes | exact to the tolerance | |
| Eye bone rotations | as look-at | mean gaze direction; both-eyes-the-same only | `anim_export.eyes_to_look_at` (info), `anim_export.eye_asymmetry` (warning) |
| Clip cue `weight` and `fade_s` (a clip blended over the idle or the bind pose, ADR 0014 §4) | no | VRMA has no per-track weight or base pose; the clip is exported at full weight with no fade, and the player's own cross-fade replaces it | none today (the op takes a clip, never a cue); a future score export must report `anim_export.cue_weight_dropped` (info) |
| `PoseMode::Add` layers (the life layer's deltas) | no | clips are `Override` poses by definition; an additive layer is not a clip, so it cannot be named by the op | none |
| Bones the clip turns that the character's rest tree lacks | no | dropped | `anim_export.bone_not_in_rest` (warning) |
| Non-humanoid skeletons (quadruped, blob) | no | refused before anything is written | `anim_export.not_humanoid` (error) |
| Score channel tracks: the 52 ARKit channels, `headYaw`/`headPitch`/`headRoll`, `lean`, `breath`, `bodyTurn`, `headShift*` | no | not a clip or emote; these are face and performance data. An opt-in export of the ARKit channels as custom expressions is possible later and additive | none today (the op takes a clip or an emote, never a score); a future score export reports `anim_export.channel_tracks_dropped` (info, with the count) |
| Cues of kind `Marker` (sound, scene, prop) | no | VRMA has no events | none today; a future score export reports `anim_export.marker_dropped` (info) |
| Expression `IntensityCurve`, channel deltas, painted-face flipbooks | no | only the weight envelope is exported; the player's model decides how the weight looks | `anim_export.expression_custom` (info) when the name is a `omuzaki_` one |
| Custom expressions beyond 100 | partly | players such as UniVRM read 100 | `anim.custom_expression_limit` (warning) |
| Reserved presets (`lookUp`, ...) named by an emote | no | the track is dropped | `anim_export.expression_reserved` (warning) |
| Hair, cloth, spring bones, IK and foot planting, props | no | run-time simulation or rig state, not animation data; the receiving model's own springs run | none (not clip data) |
| Root motion and world position of a walk | no | the hips offset carries bob and sway; a walking clip is an in-place cycle, as the pose library authors it; PF-30's locomotion root motion stays in Omuzaki | none |
| Camera moves, shot timeline | no | not channels (anim-api §10) | none |
| Scale tracks | no | `HumanoidPose` has none; the spec forbids humanoid scale | none |
| Several animations per file; a loop flag | no | one clip per file; `looped` is a hint in `asset.extras` | |
| Framing of an emote | no | camera instruction | `anim_export.framing_dropped` (info) |
| The character, its mesh, textures, materials, licence record | no | by design | |

### V12. Rights

The decision, the rule ids and what is written are in `io-api.md` D11, "Rights". In short: the file carries the **clip's own record** (for an emote, the strictest join of its pose and clip items), as `asset.copyright` and `asset.extras.omuzaki`, never the character's record and never a consent holder; `Purpose::Publish` runs `licence_gate`; private test material exports only privately and the file says `private_test: true`. A VRMA has no permission flags, so nothing can exceed the record. Two additions (project decisions):

**No-derivatives licences.** Local export is allowed; publishing is refused. Retargeting, resampling and editing make an exported `.vrma` a derivative of the clip.
- *The check* is `licence_forbids_derivatives(&Provenance) -> Option<Finding>` in `omuzaki-io-api`, next to the gate. It splits `provenance.licence` (an SPDX expression or `LicenseRef`) into identifiers on whitespace, parentheses and `AND`/`OR`/`WITH`, and fires when **any** identifier has an `ND` segment (`CC-BY-ND-4.0`, `CC-BY-NC-ND-4.0`, a `LicenseRef-...-ND-...`; matched as a whole `-`-separated segment, so `ND` inside another word never matches). "Any" is deliberate: a joined record (D10) of `A AND B` includes `B`'s terms, and for an `OR` we cannot know which the user took, so the conservative reading stands until the record is corrected.
- *Publish* (`Purpose::Publish`, and **every library or share upload** of a `.vrma`, which must call the same function): error `rights.licence_forbids_derivatives` at `provenance/licence`. Message: "This motion's licence (CC-BY-ND-4.0) does not allow derivatives. Resampling, retargeting and editing make an exported VRMA a derivative, so it cannot be published or uploaded. Export it for your own use, or use a clip whose licence allows adaptation."
- *Private*: allowed, with an info finding `rights.nd_private_export` ("local use only; the file carries the licence and the attribution").
- *Dialog.* The op result carries `notices` which the export dialog shows: the licence id and the attribution text from the record, and for an ND licence the sentence "For your own use only: the licence does not allow sharing a derivative."

**Proportions.** The file contains the skeleton's rest proportions (V4). The dialog says so: "This file includes your character's skeleton proportions (bone lengths and hips height). VRM Animation needs them to scale hips motion between characters; it contains no mesh, texture or other character data." `notices: Vec<String>` always starts with this line, then the licence lines.

## 4. What other apps need to play it

Verified in primary sources (versions in §8) unless marked.

| Player | Needs | Source |
|---|---|---|
| three-vrm (`@pixiv/three-vrm-animation` 3.5.5) | `VRMC_vrm_animation` in `extensionsUsed` and the root `extensions`; `specVersion` `1.0` (or `1.0-draft`); humanoid `humanBones` with the nodes; a rest hips world `y` above 1 mm; hips translation only on `hips`; expression nodes with `translation` only; `lookAt.node` with `rotation` only; the app adds a `VRMLookAtQuaternionProxy` to the VRM (the library creates one with a warning if missing) | `VRMAnimationLoaderPlugin.ts`, `createVRMAnimationClip.ts` |
| UniVRM 0.131.0 (`VrmAnimationImporter`) | humanoid bones and expression nodes read from the extension; presets read by name; custom expressions in at most 100 slots; reads `translation` x of the expression node | `VrmAnimationImporter.cs`, `Vrm10AnimationInstance.cs` |
| Other VRM apps (VSeeFace, VRChat-side tools, Warudo, VTube Studio's VRM mode) | **not verified:** no primary source was found that says which of them play a VRMA. The file follows the specification, so the claim to make is "conforms to `VRMC_vrm_animation` 1.0 and plays in three-vrm", which the conformance task tests. | |

**Look-at in UniVRM** was not verified (its importer's look-at handling was not found in the files read); the sign convention of V7 is checked only against three-vrm.

## 5. The tasks

The work splits into the tasks below; every `Blocked by` line names this design until it is accepted. The conversion is two tasks (clip, emote) apart from the op, because it holds the algorithm (sampling, continuity, reduction, the rest tree) that deserves its own tests, while the op is wiring.

| Task | Scope | Blocked by |
|---|---|---|
| io-api: `IoAnimation`, `AnimationExporter`, registration, rights preflight, mock | the exact shapes and rules of D11; the model preflight's gate code is shared, not copied; no serde on the big types | this design |
| `omuzaki-gltf`: write and read animations | input accessors with `min`/`max`, shared identical inputs, `extensionsUsed` hook, `CUBICSPLINE` read-only | this design |
| `omuzaki-io-vrm`: the VRMA exporter | node order, extension JSON and one canonical track order spelled out (the exporter sorts; input order is irrelevant), shuffled-input test; **regression for STEP holds (V5):** `[0, 0.9999999403953552, 1]` with weights `[0, 0, 1]` is refused with `anim_export.step_unrepresentable`; `[0, 3599, 3600]` with weights `[0, 0, 1]` is refused with `anim_export.step_unrepresentable` (the fallback hold is 0.244 ms, over the 0.222 ms bound); a track whose hold has an interior `f32` at 600 s exports strictly increasing times with the hold width at most `B` as V5 states; spec JSON schemas **not** vendored (no licence file) and not fetched; a structural checker is written from the spec text instead | io-api animation types, `omuzaki-gltf` animations |
| `omuzaki-io-ops`: clip to `IoAnimation` | new: sampling, continuity, reduction, rest tree, hips, eye gaze, sampling error | io-api animation types, the pose library (RG-06) |
| `omuzaki-io-ops`: shared `named_expression` and VRM-side custom expressions | new: one function for both exporters | FE-09 (bake for export) |
| `omuzaki-io-ops`: emote to `IoAnimation` | new: pose hold plus expression envelope, calls `named_expression`; plain input struct so it does not wait for the emote record | clip to `IoAnimation`, shared `named_expression` |
| `io.export_animation` op | wiring only: gather, provenance, `notices` for the dialog, `list_formats` additive `animations`, grants | VRMA exporter, clip to `IoAnimation`, emote to `IoAnimation`, the emote set (SH-07), the pose library (RG-06) |
| conformance in three-vrm | adds the look-at sign check, hips scaling with a shorter target, a register row for `@pixiv/three-vrm-animation` | VRMA exporter, `io.export_animation` op |

The order of work: the io-api types and the `omuzaki-gltf` animations in parallel, then the VRMA exporter and the clip conversion, then the emote conversion, the op and the conformance task. The mock round trip is the first proof the contract is right; the conformance task is the proof the format is.

### The converter's Rust shapes (`omuzaki-io-ops::convert::animation`)

```rust
pub struct ClipExportOptions { pub fps: f32 /* 30 */, pub bones: BoneSelection /* Driven */, pub tolerance: Tolerance }
pub enum BoneSelection { Driven, All }
pub struct Tolerance { pub rotation_rad: f32, pub translation_m: f32, pub weight: f32 } // each 1e-6
pub struct EmoteTiming { pub fade_s: f32 /* 0.2 */, pub hold_s: f32 /* 1.6 */, pub intensity: f32 /* 1.0 */ }
pub struct EmoteMotionInput { pub name: String, pub expression: ExpressionId, pub pose: Option<HumanoidPose>, pub timing: EmoteTiming }

pub fn rest_humanoid(skeleton: &Skeleton) -> Result<RestHumanoid, NotHumanoid>;
pub fn named_expression(id: &ExpressionId) -> ExpressionName; // in convert::channels, shared with the VRM exporter
pub fn clip_to_animation(clip: &PoseClip, skeleton: &Skeleton, name: &str, provenance: Provenance,
                         opts: &ClipExportOptions) -> Result<(IoAnimation, Report), ConvertError>;
pub fn emote_to_animation(input: &EmoteMotionInput, skeleton: &Skeleton, provenance: Provenance,
                          opts: &ClipExportOptions) -> Result<(IoAnimation, Report), ConvertError>;
```

The op: `io.export_animation { character, clip | emote, format = "vrma", fps = 30, purpose = "private", path }`, kind job, scopes `fs` and `publish` (like `io.export`), output `{ path, summary: AnimationSummary, report, notices }`.

## 6. How the glTF is written (for `omuzaki-gltf`)

- All accessors `FLOAT`; times `SCALAR`, translations and weights `VEC3`, rotations `VEC4`. glTF 2.0 requires every animation sampler's **input accessor to have `min` and `max`**, and forbids NaN and infinity in animation data; the writer sets the first and refuses the second.
- A node and path is a target once per animation (glTF 2.0); `anim.duplicate_track` guards it.
- Samplers with bit-identical time arrays share an input accessor (first wins). Accessors, buffer views, samplers and channels are numbered in the order given, so equal input gives equal bytes (io-api D3).
- One buffer, the GLB BIN chunk, padded as glTF requires; no images, meshes, skins or materials.
- `asset.generator` comes from the options; the writer adds no time or version.

## 7. Questions and answers

- **Q1 (answered; see V12).** The record of an imported motion with a no-derivatives licence. `licence_gate` checks commercial status, private test and consent, which is all a VRMA can be held to today. A CC BY-ND motion (commercial use allowed, derivatives not) retimed and re-expressed as a sampled VRMA is arguably a derivative. Answer: local export allowed, publishing and uploads refused (V12). Legal review still owns the legal reading.
- **Q2 (answered).** Proportions in the file are acceptable; the dialog says so (V12).
- **Q3 (answered).** One shared function names expressions for both exporters; the shared `named_expression` task adds it and the VRM-side writing, and blocks the emote task (V6).
- **Q4 (answered).** The extension stays out of `extensionsRequired` (V1).

## 8. Sources

Read on 2026-10-12.

- **VRM Animation 1.0**, `vrm-c/vrm-specification`, directory `specification/VRMC_vrm_animation-1.0`, at commit `bb18e502634ea9374d2203ed7df2b4771cfa3921` (last change to that directory 2025-06-09, "Update specification/VRMC_vrm_animation-1.0/how_to_transform_human_pose.md"): <https://github.com/vrm-c/vrm-specification/tree/bb18e502634ea9374d2203ed7df2b4771cfa3921/specification/VRMC_vrm_animation-1.0>. Read in full: `README.md` (status "Complete"; the extension, humanoid, expressions, look-at, the 30 fps guideline), `how_to_transform_human_pose.md` (non-normative), and the schemas under `schema/` (`VRMC_vrm_animation.schema.json`, `.humanoid.humanBones.schema.json` with 53 bones and the 15 required, `.expressions.schema.json` with the 14 presets (no `look*`), `.lookAt.schema.json` with `node` required). Noted inconsistencies in the text, none of which changes what we write: the README overview spells the extension `VRMC_VRM_animation`; it names a preset `leftBlink` where the schema has `blinkLeft`; its table lists `lookAt.node` as optional while the schema requires it.
- **Licence of that text and schema:** the repository has **no `LICENSE` file at that commit** (the root holds `README.md`, `samples`, `specification`) and GitHub reports no licence. By `docs/licensing/policy.md` it is `unverified` and treated as not cleared: we read it for the format's facts, quote no passage beyond short phrases, copy none of the README or the schemas into the repository and **vendor no schema**. The format itself (extension and property names) is what interoperability needs. Legal review holds the real answer; it is the same repository the existing io-api D9 already cites for the VRM 1.0 text.
- **VRM 1.0** (same repository and commit, `specification/VRMC_vrm-1.0/`): `lookAt.md` (yaw and pitch directions, LookAt space), `tpose.md` (the T-pose definition), `humanoid.md`, `expressions.md`.
- **glTF 2.0**, <https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html> (source `KhronosGroup/glTF`, `specification/2.0/Specification.adoc`, read from `main`): "Coordinate System and Units", "Animations" (the input accessor `min`/`max` rule, one target once per animation, no NaN or infinity), Appendix C "Interpolation" (`LINEAR`, spherical linear interpolation with the dot product's sign, `STEP`).
- **three-vrm**, `pixiv/three-vrm` tag `v3.5.5` (commit `ff42fae4fcee1fcbca2cd262c7f5f8cbddeaf5ab`), MIT (GitHub licence API): `packages/three-vrm-animation/src/VRMAnimationLoaderPlugin.ts` and `createVRMAnimationClip.ts`. Read, nothing copied. `@pixiv/three-vrm` 3.5.5 is already in `docs/licensing/register.md`; `@pixiv/three-vrm-animation` is the same repository and licence and gets its own row.
- **UniVRM**, `vrm-c/UniVRM` tag `v0.131.0`, MIT (GitHub licence API): `Packages/VRM10/Runtime/IO/VrmAnimationImporter.cs`, `Components/VrmAnimationInstance/Vrm10AnimationInstance.cs`. Read, nothing copied.
- **Internal:** `docs/decisions/0014-rest-in-t-bind-in-a.md`, `docs/design/anim-api.md` §8 and §10, `docs/design/io-api.md` D2, D3, D5, D6, D8, D9, D10, `docs/design/library.md` D1 (emotes), spec v3 §27 (EX-16, row at `docs/product/02-studio-spec-v3.md` line 944), RG-06, RG-08.

New dependencies in this design: none. (The conformance task adds `@pixiv/three-vrm-animation` 3.5.5, MIT, test only.)
