---
title: "0014: Characters rest in the VRM T-pose and are bound in their sculpted pose (2026-10-12)"
summary: "Characters rest in the VRM T-pose and are bound in their sculpted pose"
area: decisions
---

# 0014: Characters rest in the VRM T-pose and are bound in their sculpted pose (2026-10-12)

**Status:** accepted. It corrects the first decision, which fixed only the hands. **Context:** the rule "bind the mesh in A, rest the skeleton in T", `docs/design/anim-api.md` §8 (`HumanoidPose` identity is the VRM T-pose), `docs/design/io-api.md` ("inverse bind matrices are data"; VRM 1.0 needs a T-pose rest).

## Context

The contracts say a pose means the same thing on every character because identity is the VRM T-pose. The template rig instead put identity rest rotations on **A-pose** joints, and derived the inverse bind matrices from that rest. As a result:
- every T-pose-authored arm pose, clip, retarget (RG-08) and AI intent is off by the arm hang (58.6° on the upper arm at default proportions);
- the hand shapes are off (a peace V of 5.5° instead of 20°);
- VRM 1.0 export would write an A-pose rest.

Journal 05 already solved this for the prototype. Converting the mesh to the T-pose is lossy wherever weights blend, so the mesh stays in its sculpted A-pose and the arm joints' inverse bind matrices carry the A-pose.

## Decision

1. **Rest = VRM T-pose, always.** The skeleton a character builds has identity rest rotations on T-pose joints:
   - arms along ±X;
   - fingers along ±X with the palm facing −Y;
   - the thumb at its natural rest, its azimuth read from the rig, never assumed.
2. **Bind = the sculpted pose, as data.** `CharacterRig` gains `bind: HumanoidPose` (normalised space, identity when the mesh is built in T). The inverse bind matrices are computed from it: `Skeleton::inverse_bind_for(&Pose)` in `omuzaki-anim-api`. `RigSkeleton` takes the inverse bind matrices as data (`RigSkeleton::with_inverse_bind`); `Skeleton::inverse_bind()` stays as "bind = rest". Imported models keep their own inverse bind matrices as given (io-api).
3. **Meshes and weights stay in the bind pose.** Parts build their meshes in the sculpted pose. Auto-weights weight against the bind-pose joint positions, which are the template layout's joints, unchanged.
4. **The neutral display pose is the bind pose.** With no animation, the stage shows the character as sculpted, not in a T. The life layer and clips layer onto `bind` for the bones they don't drive. A clip that drives the arms drives them in T space as the contract says.
5. **Exporters write the T rest and the bind inverse bind matrices.** VRM 1.0 conformance needs no special case.

## Consequences

- One small API change set (anim-api, omuzaki-rig, char-api), then the template assembly produces a T rest plus an A bind. Its test: posing the skeleton at `bind` reproduces the layout joints to 1e-6, and the rest arms are horizontal.
- The hand library can rely on the VRM T-pose finger axes. It still reads the thumb's rest frame from the rig, because imported avatars differ there.
- The gather for export takes the inverse bind matrices from the rig, never recomputed from rest.
- The stage and animation base pose use `bind` (a task for the stage frame loop).
- The animator layers onto the base pose; `PoseLayers::resolve` pins the meaning. `Animator::animate` receives `out` holding `bind` and layers onto it (`omuzaki-anim-api`, `anim-api.md` §8); the stage seeds `out = bind` each frame.
- Rejected:
  - **Hands-only frame reading** (the first ruling): it leaves arms, retargeting and export wrong.
  - **Re-posing the mesh to T:** lossy at blended weights.
  - **An A-pose identity convention:** breaks "a pose means the same on every character" and VRM 1.0.
