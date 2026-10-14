# 0029: Likeness is measured from pixels and given landmarks, as a radial outline in eye distances (2026-10-13)

**Status:** accepted (2026-10-13; decisions folded in). **Context:** VF-01; spec v3 §15, §16 (ML-01, ML-02, ML-09,
ML-11, ML-12), §18 (CB-10 to CB-12, CB-90), CO-06, §33 lesson 2, §40; `docs/licensing/policy.md`; ADR 0003 (assets
outside git), ADR 0009 (determinism). Design: `docs/design/likeness-measures.md`.

## Context

Shape and likeness edits are not done until a measure says the face matches its reference (VF-01, VF-09). The spec asks
for an outline curve and feature spacing normalised by eye distance, computed by the same code on references (sheets,
drawings) and on renders. That needs: landmarks on stylised faces, a face segmentation that works on line art and on toon
renders, a representation of the outline, and tolerances. No stylised-face landmark model we know of clears the licence
policy (trained on real faces, or on illustrations with no licence for training and commercial use; models trained on
300-W or WIDER FACE are banned outright). A throwaway prototype on synthetic faces of our own measured the approach before
this record was written (design §9).

## Decision

1. **The input is an image plus landmarks, never a character.** References and renders run through identical code; a
   render's landmarks come from `render.preview` (`face_landmarks`), a reference's from a person or an agent.
2. **No learned landmark detector in v1.** Given landmarks are snapped to the drawn marks by a windowed mean shift,
   which made clicks with 2% of IOD noise repeatable to 0.16% of IOD in the prototype (3.1% without). A detector may
   later fill the model slot `face_landmarks_stylised@1` (ML-01) only with a licence-clean model: our own ML-09 model
   trained on our renders (ML-12), or, for realistic human-proportioned faces only, MediaPipe Face Landmarker (weights
   Apache-2.0 per its model cards; register row). The search is a P2 research task: no
   non-commercial weights or data.
3. **The outline is a radial profile about the eye midpoint**: 85 samples every 2.5° from 15° above the eye line through
   the chin, in eye distances, at the centre of the outline stroke, each with a state (`ok`, `occluded`, `excluded`,
   `cut`). A fill that leaks through a gap in the line art refuses the measurement; it never reports numbers.
4. **Likeness is a weighted distance in units of tolerance** over the outline and four spacing terms, with fixed
   weights and two tolerance presets (`same_look` for render against render, `cross_look` for a drawing against a
   render). Lower is better; the gate passes only when every term is within its tolerance. Tolerances may only tighten
   until people's ratings calibrate them.
5. **Measurement renders use the named render preset `measure`** (library data, not ad-hoc settings): orthographic
   front, one canonical flat light rig, the neutral expression, the outline forced dark and at least 1.5 px, IOD at
   least 160 px. The geometry is the character's own; shading stops being a confound.
6. **A likeness target is an immutable store asset** of inputs (the reference image's hash, landmarks, exclusions)
   with a cache of the numbers tagged by `measure_version`, **referenced by content hash from an optional
   `CharacterDef.likeness` field**, the pattern of `TracedHead::asset`; never inline. A private target lives in a private
   project's local store and stays out of git, as the traced head does; everything measured from it carries
   `private_test`.
7. **A new implementation crate, `omuzaki-measure`**, owns the image measures and their ops; no API crate until another
   crate needs the types in Rust.

## Consequences

- New crate `omuzaki-measure` (implementation; depends on `omuzaki-core`, `omuzaki-ops-api`, `omuzaki-store-api`,
  `omuzaki-l10n-api`, `image`). The `image` crate gains its `jpeg` feature, decoded by `zune-jpeg` 0.5 (MIT OR Apache-2.0
  OR Zlib, already in `Cargo.lock`); the register row for `image` is updated by the crate's first task.
- `render.preview` gains `face_landmarks` and named presets; a `measure-flat` light rig and the `measure` render
  preset are added to the library; `face.layout.measure` accepts a partial reference.
- `omuzaki-char-api` gains the optional `likeness` map (`LikenessRef { asset, view }`). The ops
  `measure.target.set` and `measure.target.remove` maintain it.
- The private acceptance check runs only where the private assets are available, reports Rust against JS under both presets, and gates
  on the sheet (a drawing) against the Rust render at `cross_look`.
- Eval `query` checks that measure a render need a nested op argument (the image from `render.preview`), as `render.diff`
  already has for regions; `measure.face_outline` reports `/aspect`, which the `face.more-oval` task reads.
- v1 measures front views only; yaw is flagged by asymmetry, pitch is controlled by the measurement camera. Three-quarter
  and side views are the P2 follow-up.
- **Rejected:** measuring characters instead of images (breaks "the same code"); an object-id mask for renders (a sheet
  has none, so the two sides would differ by half a line); a detector now (no licence-clean model); width-at-height,
  Fourier, chamfer or superellipse-fit representations of the outline (design D5); one tolerance for every comparison;
  a score without per-term gates; targets that store only numbers; target files found by path beside the image (the
  first draft); targets inline in the character file.
