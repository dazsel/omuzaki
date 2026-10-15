---
title: "Likeness measures: `measure.face_outline`, `measure.feature_spacing`, `measure.likeness` (VF-01)"
summary: "`measure.face_outline`, `measure.feature_spacing`, `measure.likeness` (VF-01)"
area: design
---

# Likeness measures: `measure.face_outline`, `measure.feature_spacing`, `measure.likeness` (VF-01)

**Status:** the maintainers' decisions of 2026-10-13 are folded in (§14); awaiting acceptance. **Feature:** VF-01. **Decision record:**
`docs/decisions/0029-likeness-measures.md`.
**Spec:** v3 §15 (VF-01, VF-09), §18 (CB-10, CB-11, CB-12, CB-90), §16 (ML-01, ML-02, ML-09, ML-11, ML-12), CO-06,
§33 lesson 2 ("made the face wider when the user meant oval; fixed only after measuring the outline"), §40.
**Builds on:** `docs/design/ops-api.md` (services, `fs_access`, rule 7), `docs/design/evals.md` (query checks, the
private list, the human-rated review set), `docs/design/char-face-api.md` (conventions, `HeadOutline`, `FaceLayout`),
`face.layout.measure` and `LayoutRatios`, the render ops, `docs/design/scenes.md` (framing), ADR
0003 (assets outside git), ADR 0009 (determinism), `docs/licensing/policy.md`.

This page says what each measure is, exactly enough that two implementations give the same numbers, and why. The
numbers in section 9 come from a throwaway prototype (Python, not committed) run on two synthetic faces
and one drawing of our own; nothing private or third-party was used.

## 1. In one minute

A likeness check compares a **reference** (a character sheet, a concept drawing) with a **candidate** (a render of
our character) at the same view. Both are images. Each image goes through the same code:

1. **Landmarks in.** Two eye centres (and the mouth) come with the image: clicked by a person, given by an agent that
   can see, or reported by the renderer. They are then **snapped** to the drawn marks, so a click 2% off lands on the
   same point every time.
2. **A face frame.** Origin at the eye midpoint, x along the eye line, y down the face, and the **eye distance (IOD)
   as the unit**. Position, size and roll drop out.
3. **The outline as a radial profile.** From the eye midpoint, 85 rays every 2.5° from 15° above the eye line on one
   side, down through the chin, to 15° above it on the other. Each ray stops at the centre of the face's outline stroke.
   The result is 85 distances in IOD, each with a state (`ok`, `occluded` by hair, `excluded`, ...).
4. **Feature spacing.** Eye line to chin, eye line to mouth, mouth to chin, the face width at the eye and mouth lines,
   the widest width and the aspect, all in IOD.
5. **Likeness.** A weighted distance over the profile and the spacing, each term divided by its tolerance. **Lower is
   better: 0 is identical, 1 is at the edge of tolerance.** The gate passes when every term is within its own
   tolerance.

```text
 reference image ─┐                                                      ┌─ OutlineProfile (85 radii, states)
 render image ────┼─► decode ─► landmarks ─► snap ─► face frame ─► fill ─┤
 (+ landmarks)    │   (PNG,     (person,     (mean    (eye mid,    (skin, ├─ FeatureSpacing (IOD ratios)
                  │    JPEG)     agent,       shift)   IOD unit)    holes) │
                  │              render)                                 └─► measure.likeness(ref, cand)
                  └── the same code for both: nothing branches on where the image came from     score, terms, worst sector
```

![Three synthetic faces of our own, measured: green dots are the outline samples, magenta crosses the snapped eyes,
the magenta square the mouth line, red rays the samples a hair lock covers.](img/likeness-measures-faces.png)

*A and B: render-like (thin outline in the skin palette's line colour). C: a drawing style with thick tapered lines,
hatched blush, an ear, an open mouth, a side lock over one cheek and JPEG compression. The side lock's samples are
marked occluded and left out of every comparison.*

## 2. What this covers

| Asked for | Where |
|---|---|
| The home (crate) and the input (image, face rectangle, landmarks) | D1, D2, D9; §5 |
| The outline: segmentation on sheets and renders, the representation, argued | D4, D5; §4.2 |
| Feature spacing; the likeness score as a documented weighted distance | D6, D7; §4.3, §4.4 |
| Robustness to pose, lighting and style; units, frames, what "lower is better" means | §4 |
| The reference format, privately when the reference is private | D8; §6 |
| Determinism, performance budgets, failure modes | §7 |
| The prototype on two synthetic faces and one drawing, its repeatability | §9 |
| The tolerance of the private acceptance check | §10 |
| A validation plan with perturbations that move the measure monotonically | §11 |
| The implementation tasks under VF-01 | §13 |

**Not covered, on purpose:** `render.compare` (the side-by-side and heatmap); three-quarter and side views (the
profile line; P2 follow-up, once the front view is proven); the ML-11 likeness critic (it learns from the human
ratings of §11.3 and uses these measures as its features); a learned landmark detector (D2).

## 3. Decisions

### D1. The input is an image and landmarks, never a character

Every measure op takes pixels plus landmarks. A render of our character goes through exactly the code a sheet goes
through, which is what VF-01 asks ("the same code on references and on renders") and what makes a reference-to-render
comparison fair: whatever bias the segmentation has (half a line width, a lash that moves the eye centroid), both sides
carry it.

- **Why not `measure.face_outline {character, view}`**, which `docs/design/evals.md` sketched? The measure crate would
  need the stage to render, an implementation crate cannot depend on the stage, and a service trait for "render this
  character" would duplicate `render.preview`. Agents call `render.preview` and pass its image and its landmarks on
  (D10); eval checks do the same through a nested op argument (§8).
- **Images:** PNG (8 or 16 bit, grey, RGB, RGBA) and JPEG, from the asset store by hash or from a file the actor may
  read (`ctx.fs_access(path, Read)`). Transparent pixels (alpha < 128) are background.

### D2. Landmarks are given and snapped; no learned detector in v1

The measure needs the two eye centres; the mouth is optional (without it the mouth terms are absent). They come from:

| Source | When | Accuracy before the snap |
|---|---|---|
| `render` | `render.preview` reports where the face layout's eyes, mouth and chin land in its image (D10) | exact |
| `person` | clicked on the sheet once, stored in the target file (§6) | about 1 to 3% of IOD |
| `agent` | an agent with vision reads the sheet (CN-09) and passes coordinates | a few % of IOD |
| `detector` | reserved: a model in the slot `face_landmarks_stylised@1` (ML-01), later | |

**The snap** moves each given point to the centroid of the drawn mark around it (§4.1). In the prototype, clicks with
2% of IOD noise gave a profile spread of 3.1% of IOD unsnapped and 0.16% snapped (§9.2). The snap runs on every source,
including `render`, so the two sides of a comparison define "eye centre" the same way.

**No detector model now.** Every stylised-face landmark model we know of is either trained on real human faces or on
illustrations without a licence that allows training and commercial use. The policy bans models trained on
non-commercial data (`docs/licensing/policy.md`: 300-W, WIDER FACE; this rules out dlib's 68-point predictor, which is
trained on 300-W). The commercially usable options, named for later:

- **Our own stylised-face model (ML-09)**, trained on our own renders through the ML-12 pipeline, licence-clean by
  construction. It fills `face_landmarks_stylised@1` when it exists and is gated by the same snap and the same
  validation (§11).
- **MediaPipe Face Landmarker** (`face_landmarker.task`; weights Apache-2.0 per its model cards; register row
  "MediaPipe face_landmarker.task", `conditions`). It is trained on real faces, so it is only a candidate for
  realistic, human-proportioned characters. It was not tried on stylised faces here.

The decision is recorded in ADR 0029 (accepted). The search for a licence-clean detector is a P2 research item:
it must use no non-commercial weights or data.

### D3. The face frame: eye midpoint, eye line, IOD

With the character's right eye `R` and left eye `L` (char-face-api's convention: the character's own left, which is the
image's right in a front view), in image pixels (origin top-left, x right, y down, pixel centres at `.5`):

- `E = (R + L) / 2`, `d = |L - R|` (the **IOD**, pixels), `u = (L - R) / d`, `v = (-u.y, u.x)`.
- A point `p` has face coordinates `x = (p - E)·u / d`, `y = (p - E)·v / d`. So **+x is along the eye line toward the
  character's left, +y is down the face, and 1.0 is one eye distance.**
- `v` is the image's "down" rotated with the eye line, so roll is removed. If a mouth is given and `y(mouth) <= 0`, the
  eyes are swapped (or the face is upside down) and the measure refuses (§7.3).
- Angles `φ` are measured from +x toward +y: `φ = 0°` points along the eye line toward the character's left, `90°`
  down the face's midline to the chin, `180°` toward the character's right.

The same unit and the same eye line are what `FaceLayout::ratios` and `LayoutRatios` use (IOD = `2 * eye_spacing`,
heights measured down from the eye line), so image measures and layout ratios can be compared directly (§4.3).

### D4. The face region: the lit skin, bounded by the line art, with holes filled

The outline is found from the **face region**: the skin pixels connected to the cheeks, with every enclosed mark (eyes,
brows inside the face, nose, mouth, blush, hatching) filled in. In order (all thresholds are in §4.1):

1. **Lit skin colour** `S`: the CIELAB median of the lighter half of three small patches below each eye and between
   them. A `skin` argument overrides it.
2. **Line lightness** `L_line`: along nine rays down the chin sector (70° to 110°), the first local minimum darker than
   `S.L - 10`; the median over the rays. This adapts the skin window to the image's line art (renders' skin-coloured
   outlines are much lighter than a drawing's ink).
3. **Skin-like pixels:** `S.L - Δ < L* < S.L + 10` and `|Δa*b*| < 14`, where `Δ = min(20, (S.L - L_line) / 2)`: the
   lower bound sits halfway between the lit skin and the line, never deeper than 20.
4. **Fill:** 4-connected flood fill of skin-like pixels from the three patch centres.
5. **Holes:** every pixel not reachable from the region of interest's border through non-region pixels joins the
   region (an eye is a hole; a gap in the jaw line is not).

The fill is what can go wrong. A gap in the jaw line (an outline thinner than a pixel, a sketchy drawing) lets the fill
escape into the neck. The **leak guard** in §4.2 catches it, and a leaked measurement is refused, never reported.

### D5. The outline is a radial profile about the eye midpoint, at the outline stroke's centre

`r(φ_k)` for `φ_k = -15° + 2.5°·k`, `k = 0..=84` (85 samples), in IOD. Each ray starts at `0.30·d` (outside the eyes)
and finds where it leaves the filled region, then measures the outline stroke there (§4.2). **The value is the centre of
the stroke**: its inner edge plus half the image's median stroke width. Where no stroke exists (a silhouette straight
against the background), it is the region's edge.

Why the stroke's centre: an artist draws the contour as the line's centre, and a sheet's lines are two or three times
thicker than a render's. Taking the inner edge on both would move a thick-lined sheet's face inward by half its line
(about 1.3% of IOD for a typical drawing). The centre makes the two agree within the measured noise (§9.3). A toon
render's hull outline lies outside the mesh silhouette, so a render's measured outline is half a line width outside its
mesh. It is the same for every render, so before and after comparisons are unaffected.

Why a radial profile, among the alternatives:

| Representation | Verdict |
|---|---|
| **Radial profile about the eye midpoint** (chosen) | Anchored to the eyes, which the spec's normalisation already uses; one number per direction, so two faces correspond sample by sample with no matching step; partial occlusion just drops samples; reads as "the jaw is 3% wider at 40°". Faces are star-shaped from the eye midpoint (every ray from it crosses the lower outline once), which holds for every plausible head (`docs/design/char-face-api.md`'s plausible space) |
| Half-width at fixed heights (fractions of eye-to-chin) | Needs the chin first, so a chin error moves every sample; degenerate near the chin, where the height changes fastest. Its useful numbers (width at the eye and mouth lines) are reported as spacing (D6) |
| Elliptic Fourier descriptors, turning angle | Need a closed contour (hair hides the forehead); invariant to the very things the eye anchor should keep; hard to explain to a person or an agent |
| Chamfer or Hausdorff distance between contours | No correspondence, so no "where"; one outlier dominates Hausdorff |
| Fitting `HeadOutline` (superellipse) parameters | Model-bound: a heart-shaped face or a pointed chin fits badly and the error hides in the fit. Fitting the builder's parameters *to* a profile is CO-06's and CB-90's job, using this measure |

The range stops 15° above the eye line because above it, in front views, the forehead is almost always under bangs: in
the prototype, samples more than about 15° above the eye line landed on the bangs' edge on both sides.

### D6. Feature spacing reports IOD ratios that match the layout's

From the profile and the landmarks, in IOD (§4.3): `eye_to_chin`, `eye_to_mouth`, `mouth_to_chin`, `mouth` (the
mouth's height as a fraction of eye-to-chin), `nose` (the same, when a nose point is given), `width_at_eyes`,
`width_at_mouth`, `width_max`, `aspect` (`eye_to_chin / width_max`). `eye_to_chin` is `LayoutRatios::drop_to_iod`, and
`mouth` and `nose` are its `mouth` and `nose`, so the output carries a `layout_ratios` object that
`face.layout.measure {reference}` can take once that op accepts a partial reference (task T14).

### D7. Likeness is a weighted distance in units of tolerance; lower is better

Terms, each a non-negative difference between reference and candidate:

| Term | Value | Weight | Tolerance, `same_look` | Tolerance, `cross_look` |
|---|---|---|---|---|
| `outline` | weighted RMS of `r_ref(φ) - r_cand(φ)` over samples `ok` in both, jaw and chin sector (30° to 150°) counted twice | 2 | 0.010 | 0.025 |
| `eye_to_chin` | absolute difference | 1 | 0.010 | 0.030 |
| `eye_to_mouth` | absolute difference (both mouths given) | 1 | 0.010 | 0.030 |
| `width_at_mouth` | absolute difference (both mouths given, both sides `ok`) | 1 | 0.020 | 0.035 |
| `width_at_eyes` | absolute difference (both sides `ok` in both) | 1 | 0.020 | 0.035 |

- **Score** `L = sqrt( Σ wᵢ (Δᵢ / τᵢ)² / Σ wᵢ )` over the terms present. **0 is identical; 1 is the weighted average
  difference at the tolerance; lower is better.** It is for ranking and trends (the eval history, an agent choosing
  between two edits).
- **Gate** `within = every |Δᵢ| <= τᵢ`. A single term far out fails the gate even if the others pull `L` down.
- **`compare`:** `same_look` when both images come from the same kind of picture (render against render: goldens, the
  Rust render against the prototype's), `cross_look` when a drawing is compared with a render (the likeness gate of
  CB-90). It defaults to `cross_look`.
- The **weights are fixed** and the tolerances are only overridable per call (`tolerances`), so scores stay comparable
  across runs and tasks. Where the tolerances come from is §10.
- **Coverage:** the outline term needs at least 60% of the weighted samples `ok` in both, including every sample from
  60° to 120°. Otherwise the comparison refuses (`measure-coverage`) and names the missing sectors.

### D8. A likeness target is a store asset, referenced by hash from the character; never inline

A reference is stored as **what was measured and how** (the reference image's hash, the landmarks, the exclusions),
plus a cache of the numbers tagged with `measure_version`. The target is a small immutable asset in the project's
store; the character document names it by content hash in an optional field (`CharacterDef.likeness`), the same pattern
as `TracedHead::asset`. Nothing about the reference is inline in the character file, and a private reference stays in a
private project's store in the assets folder, never in git, as the traced head (CB-11) does. Format, ops and privacy rules in §6.

### D9. A new implementation crate, `omuzaki-measure`, with no API crate yet

`omuzaki-measure` holds the image maths (`face::{frame, segment, snap, outline, spacing, likeness}`) and registers the
ops. It depends on `omuzaki-core`, `omuzaki-ops-api`, `omuzaki-store-api`, `omuzaki-l10n-api` and `image` (PNG and JPEG). The
output types are the ops' JSON Schemas; no other crate needs them in Rust today (the stage passes landmarks *in* as JSON,
CB-90 chains ops). If a second crate ever needs to call the maths directly, its types move to a `omuzaki-measure-api`
then, by a reviewed API change. The crate is the home for the other image measures of §15 too (`measure.readability`,
`measure.silhouette`, `measure.palette_contrast`, VF-06), which reuse its image input.

### D10. Renders report their face landmarks, and measurement renders use the `measure` render preset

- `render.preview` adds `face_landmarks` to its output for each character in frame: the image positions of the
  face layout's eye centres, mouth and chin, projected through the head's pose and the camera. Agents pass them to the
  measure ops as `source = "render"`.
- **The `measure` render preset** (`render.preview {preset: "measure"}`): a named preset, stored as library data next
  to the light rigs (`crates/omuzaki-stage/library/render-presets/measure.toml`), never ad-hoc settings. It fixes an
  orthographic front camera at the face framing, the `measure-flat` light rig (one canonical even frontal light, no rim,
  no post effects), the expression `neutral`, and the outline forced to a dark colour (`#1a1a1a`) at no less than
  1.5 px. The geometry is the character's own; only the lighting and the line colour are standardised, so shading is
  not a confound. Why: a toon render's outline is the palette's `skin_line`, which by the palette rule
  can be as little as about 19 L* below the lit skin, and a key light adds shade of the same depth. In the prototype, an
  outline that faint made the fill leak (refused) with the fixed skin window, and measured with up to 4% of IOD error
  with the adaptive one (§9.5). A dark line and flat light take the palette out of the measurement.
- **Size:** at least 160 px of IOD (a 768 px face framing gives about 250). The prototype refused render-like faces
  below 96 px of IOD (the outline became thinner than a pixel and the fill leaked) and measured them from 96 px.

### D11. Front views only, with a pose check

v1 measures front views. Roll is removed by the frame. **Yaw** shows as left-right asymmetry. The angle table is symmetric
about the vertical: ray `k` mirrors ray `84 - k`. `asymmetry` is the mean of `|r(φ_k) - r(φ_{84-k})|` over the 24 pairs
with `k` from 18 to 41 (φ from 30° to 87.5°, so the mirror covers 92.5° to 150°), each pair counted once and only when
both rays are `ok`, divided by the mean of those pairs' radii. **Fewer than 8 of the 24 pairs** having both rays `ok` (a
third; a provisional number that T15 confirms) leaves the yaw unmeasured: `asymmetry` is `null` and the output carries a
`yaw-unchecked` warning. The measure never reports a value from a set that small, including an empty one, and never refuses
on this alone, since asymmetry is not a likeness term. The difference is absolute because a signed one flips sign
between the two directions of a pair, so a signed mean cancels exactly. A left-right mirrored face therefore has the same
`asymmetry` and the same warning whichever way it is turned, and a test checks that (T5). Above 0.06 the output carries a
`yaw` warning ("the face looks turned; measure a front view"). **Pitch** cannot be told from one front image:
measurement renders use an orthographic camera at zero pitch, and a reference is assumed to be a front view. The real
effect of yaw and pitch on renders is measured by task T15, which then sets the warning threshold and decides whether
a turned face is refused. Side and three-quarter views come later (P2 follow-up).

## 4. The measures, exactly

### 4.1 Common steps

**Decode** with `image` 0.25 (`png`, `jpeg`) under limits checked before allocation: at most 8192 × 8192 px and 64 MiB
decoded. Colour: sRGB to linear by a 256-entry table, to XYZ (D65) and CIELAB (`L*`, `a*`, `b*`) computed in `f64` and
stored as `f32` in the ROI (§7.2). Every geometric value (landmarks, rays, radii, ratios, scores) is `f64`.

**Region of interest (ROI):** the square of half-side `2.6·d` around `E`, clipped to the image. If `d > 256 px`, the ROI
is box-downscaled by the integer factor `ceil(d / 256)` and the landmarks with it. Everything after works on the ROI,
so memory and time are bounded whatever the image size (at most about 1.8 Mpx).

**Refusals before any work:** `d < 64 px` (`measure-too-small`); a landmark outside the image (`measure-landmark-outside`);
`y(mouth) <= 0` (`measure-eyes-swapped`).

**Skin, line and fill:** D4. Patch centres at face coordinates `(-0.5, 0.42)`, `(0.5, 0.42)`, `(0, 0.22)`, radius
`0.07·d`. If `S.L - L_line < 15` the line art is too faint to bound the face (`measure-low-contrast`).

**Snap** (on by default, `refine = false` turns it off), on the mask `M` of non-skin pixels inside the filled region:

| Point | Window radius | Rule | Fails when |
|---|---|---|---|
| eyes | `0.25·d0` | mean shift: the centroid of `M` pixels in the disc, re-centred, six times | fewer than 5% of the disc's area is `M` |
| mouth | `0.15·d0` | mean shift as above, then the mark's **top**: walking along `+v` on the line through the centroid, from `-0.15·d` to `+0.15·d`, the first `M` pixel | fewer than 0.5% of the disc |
| nose | `0.10·d0` | mean shift (the mark's centroid) | fewer than 0.5% |

`d0` is the IOD of the given eyes; the frame is rebuilt from the snapped eyes. A failed snap keeps the given point and
reports `snapped: false` (and a `snap_failed` warning); it does not refuse.

- *Why a window, not the mark's connected shape:* in the prototype a thick lash that touched the brow, which touched the
  bangs, stopped the eye from being an enclosed mark in some images and not in others, and the eye centre jumped by up
  to 5% of IOD. The window ignores what the mark touches outside it.
- *Why the top of the mouth:* the top edge of a mouth is the lip line whether the mouth is open or closed; the centroid
  of an open mouth sits lower. It still differs between an open and a closed mouth by 2 to 3% of IOD (§9.3), so
  likeness compares neutral faces (D10) and warns when the two mouths' heights differ by more than 2× (`expression`).

### 4.2 `measure.face_outline`: the radial profile

For each `φ_k`, with direction `w = cos φ·u + sin φ·v`, march from `E + 0.30·d·w` in 0.25 px steps (nearest pixel) to
the first sample outside the filled region (`j`). No exit before the ray leaves the ROI: state `cut`.

Along the ray, `L*` is sampled bilinearly from 3 px before `j` to `0.10·d + 3 px` after it:

1. **The stroke:** the first sample darker than `S.L - 15` at or after `j - 1 px`, then down to its local minimum
   `L_min`. If `S.L - L_min <= 15` there is no stroke: the edge is `j - 0.125 px` and the width is 0.
2. **Inner edge** `t_in`: where `L*` crosses `S.L - (S.L - L_min) / 2` going back from the minimum (linear
   interpolation between samples).
3. **Outer edge** `t_out`, when visible: if the maximum after the minimum is at least 15 above `L_min`, where `L*`
   crosses halfway between them. If `t_out - t_in > 0.10·d` the run is not a line (dark hair against the face).
4. **Width** `w_line` = the median of `t_out - t_in` over the samples where it was visible (0 if none).
5. `r(φ_k) = (t_in + w_line / 2) / d`.

**What lies beyond** (`beyond`), sampled at `t_in + width + 0.03·d + 1 px`: `background` (alpha < 128, or within ΔE 6
of the ROI border's mode colour when at least half the border has it), `skin` (skin-like: the neck, an ear), `hair`
(within ΔE 20 of the hair colour: the median colour just outside the region in the sector -150° to -30°, above the eyes),
or `other` (clothes).

**States**, in this order:

| State | Rule |
|---|---|
| `excluded` | inside a caller's `exclude` sector |
| `cut` | the ray left the ROI first |
| `leak` | between two jumps (`r` changing by more than 0.08 between neighbouring samples), the first upward: the fill escaped through a gap. **Any `leak` refuses the measurement** (`measure-leak`, with the sector) |
| `occluded` | `beyond = hair` and `r(φ) < r(180° - φ) - 0.04` with the mirror sample `ok`: a lock of hair in front of one cheek |
| `ok` | otherwise |

Hair in front of both cheeks symmetrically cannot be told from a narrow face; the caller excludes those sectors (an
agent that can see does it from the overlay, task T9).

**Output scalars:** `chin` = `r(90°)` (where the midline meets the outline; the lowest point is ill-conditioned on a
flat-bottomed face: it wandered from 65° to 115° in the prototype) and its image point; `width_at_eyes`, `width_max`,
`aspect` (§4.3); `asymmetry` (D11); `coverage` (the weighted fraction `ok`); `line_width` (`w_line / d`); `line_contrast`
(`S.L - L_line`). `r(90°)` not `ok` refuses (`measure-no-chin`).

### 4.3 `measure.feature_spacing`

The `ok` samples as points `(x_k, y_k) = r_k (cos φ_k, sin φ_k)`; the right side is `φ < 90°`, the left `φ > 90°`.
`x_side(y)` is the first crossing of height `y` on that side's polyline (consecutive `ok` samples only).

| Field | Definition (IOD) | Needs |
|---|---|---|
| `eye_to_chin` | `r(90°)` | |
| `eye_to_mouth` | `y(mouth)` | mouth |
| `mouth_to_chin` | `eye_to_chin - eye_to_mouth` | mouth |
| `mouth` | `eye_to_mouth / eye_to_chin` (fraction, 0 = eye line, 1 = chin) | mouth |
| `nose` | `y(nose) / eye_to_chin` | nose |
| `width_at_eyes` | `x_right(0) - x_left(0)` | both sides `ok` at the eye line |
| `width_at_mouth` | `x_right(y_m) - x_left(y_m)` | mouth, both sides |
| `width_max` | the largest width over 41 heights from 0 to `eye_to_chin` | |
| `aspect` | `eye_to_chin / width_max` (larger = longer, narrower: "more oval") | |
| `layout_ratios` | `{drop_to_iod: eye_to_chin, mouth, nose}` (the fields measured) | |

A field that cannot be measured is `null` with a reason, never a guess.

### 4.4 `measure.likeness`

D7, over two subjects (`reference`, `candidate`), each measured by §4.1 to §4.3. Besides the terms and the score it
returns the **per-angle difference** (`r_cand - r_ref`, `null` where either is not `ok`), the **worst sector** (the
contiguous run of samples whose absolute difference exceeds half the outline tolerance, with the largest summed excess,
as `from_deg`, `to_deg`, mean difference), its image points on both images, the signed mean (`+` = the candidate's face
is larger around the eyes' midpoint), and short notes ("the lower face is 3.1% of the eye distance longer than the
reference") built from declared messages (ADR 0023), one per term outside tolerance.

## 5. The ops

All three are `OpKind::Query`, `Scope::Read`, `Undo::None`: they read an image and change nothing, so an eval's
read-only checker may call them (`docs/design/evals.md` §3.3). They read the store or a file inside `plan`, as
`io.inspect` does; their worst case (§7.2) is well under what rule 7 of `docs/design/ops-api.md` is about. The evidence
image is a separate op with `Scope::Render` because it writes to the store.

```rust
/// Where the pixels come from. JSON: `{"asset": "b3:…"}` or `{"path": "refs/front.png"}`.
#[serde(untagged)]
pub enum ImageRef {
    /// A PNG or JPEG in the asset store (what `render.preview` returns).
    Asset { asset: ContentHash },
    /// A PNG or JPEG file the actor may read (`OpCtx::fs_access`, read mode), such as a sheet outside the project.
    File { path: String },
}

/// Image points in pixels: origin top-left, x right, y down, pixel centres at .5.
pub struct FaceLandmarks {
    /// The character's right eye (on the image's left in a front view).
    pub eye_right: [f64; 2],
    /// The character's left eye.
    pub eye_left: [f64; 2],
    /// Anywhere on the mouth; the snap finds its top edge. Absent: no mouth terms.
    pub mouth: Option<[f64; 2]>,
    /// The nose mark. Absent: no `nose` ratio.
    pub nose: Option<[f64; 2]>,
    /// Who placed them: `person`, `agent`, `render` or `detector`. Recorded, never used to branch.
    pub source: LandmarkSource,
}

/// A range of outline angles to leave out, degrees, in the face frame (§D3), `from <= to`.
pub struct Sector { pub from_deg: f64, pub to_deg: f64 }

/// One face to measure: shared by the three ops.
pub struct FaceSubject {
    pub image: ImageRef,
    pub landmarks: FaceLandmarks,
    /// Snap the landmarks to the drawn marks (§4.1). Default true.
    pub refine: bool,
    /// Sectors to leave out (hair the occlusion rule cannot see, a hand in front of the face).
    pub exclude: Vec<Sector>,
    /// `#rrggbb`: the lit skin colour, when the patches below the eyes are not plain skin.
    pub skin: Option<String>,
}

/// `measure.face_outline`, `measure.feature_spacing`: one face.
pub struct MeasureFace { #[serde(flatten)] pub face: FaceSubject }

/// `measure.likeness`: a reference and a candidate.
pub struct MeasureLikeness {
    pub reference: Subject,          // FaceSubject, or { target: { asset } } or { target: { character, name } } (§6)
    pub candidate: Subject,
    pub compare: Compare,            // same_look | cross_look (default)
    pub tolerances: Option<Tolerances>,  // overrides, per term; absent terms keep the preset's value
}

/// `measure.face_overlay` (Scope::Render): the evidence image, written to the store.
pub struct FaceOverlay { pub face: Subject, pub reference: Option<Subject>, pub compare: Compare }
```

Outputs (abridged; the JSON Schemas are generated from the result types):

```jsonc
// measure.face_outline
{ "measure_version": 1, "private": false,
  "image": { "hash": "b3:…", "width": 768, "height": 768 },
  "frame": { "origin": [384.2, 301.7], "x_axis": [0.9998, 0.0175], "iod_px": 251.3, "roll_deg": 1.0, "downscale": 1 },
  "landmarks": { "eye_right": { "given": [258.0, 300.1], "used": [258.6, 299.4], "snapped": true }, "...": {} },
  "outline": { "from_deg": -15.0, "step_deg": 2.5,
               "r": [0.93, 0.92, null, "…"], "state": ["ok", "ok", "occluded", "…"], "beyond": ["hair", "…"] },
  "chin": { "drop": 0.712, "point": [386.1, 480.6] },
  "width_at_eyes": 1.74, "width_max": 1.75, "aspect": 0.407,
  "asymmetry": 0.008, "coverage": 0.93, "line_width": 0.021, "line_contrast": 58.2,
  "warnings": [] }

// measure.feature_spacing
{ "measure_version": 1, "private": false, "frame": { "…": "as above" }, "landmarks": { "…": "as above" },
  "spacing": { "eye_to_chin": 0.712, "eye_to_mouth": 0.401, "mouth_to_chin": 0.311, "mouth": 0.563, "nose": null,
               "width_at_eyes": 1.74, "width_at_mouth": 1.40, "width_max": 1.75, "aspect": 0.407 },
  "unmeasured": { "nose": "no nose point given" },
  "layout_ratios": { "drop_to_iod": 0.712, "mouth": 0.563 },
  "warnings": [] }

// measure.likeness
{ "measure_version": 1, "private": false, "compare": "cross_look",
  "score": 0.62, "within": true,
  "terms": [ { "id": "outline", "value": 0.012, "tolerance": 0.025, "weight": 2, "within": true },
             { "id": "eye_to_chin", "reference": 0.712, "candidate": 0.731, "difference": 0.019, "tolerance": 0.030,
               "weight": 1, "within": true } ],
  "outline": { "coverage": 0.88, "mean_signed": 0.004, "per_angle": [0.001, "…"],
               "worst": { "from_deg": 62.5, "to_deg": 80.0, "mean": 0.021,
                          "reference_points": [[…]], "candidate_points": [[…]] } },
  "notes": ["The lower face is 1.9% of the eye distance longer than the reference."],
  "warnings": [] }

// measure.face_overlay
{ "asset": "b3:…", "private": false }
```

- `private` is true when either input is private (§6.2). Every output and evidence image made from private input says
  so, and an evidence asset is stored with `Provenance::private_test = true`, which the licence gate already refuses to
  publish (§40).
- Errors: invalid arguments and every refusal of §7.3 are `ErrorCode::Invalid` with the field (`landmarks.eye_left`,
  `reference.image`) and a hint; a missing asset or file is `NotFound`; a missing store is `Unavailable`. Messages are
  declared (`messages!`, ADR 0023).
- **What tool descriptions say** (VF-09): when to use each op ("after a shape or likeness edit, compare a front render
  with the reference"), what to pass (landmarks from `render.preview`, the snap on), and what to check after (`within`,
  the worst sector, the overlay).

## 6. The reference format

### 6.1 The target asset

A likeness target is a small TOML document stored in the project's asset store (content-addressed by BLAKE3, with a
provenance record, immutable). Its media type is `application/vnd.omuzaki.likeness-target+toml`.

```toml
format = 1
view = "front"
compare = "cross_look"            # the preset this target is meant for
refine = true                     # the FaceSubject options, stored with the inputs (§6.1)
exclude = [{ from_deg = 182.5, to_deg = 195.0 }]   # a side lock the occlusion rule missed
# skin = "#f0d2bd"                # optional: only when the patches below the eyes are not plain skin

[image]
asset = "b3:…"                    # the reference image, itself a store asset (imported with its own provenance)

[landmarks]                       # pixels in that image, as placed; the snap runs at every use
eye_right = [412.0, 388.5]
eye_left  = [598.0, 386.0]
mouth     = [505.0, 541.0]
source    = "person"

[measured]                        # a cache, for machines whose store lacks the image object
measure_version = 1
outline = [0.93, 0.92, "…"]       # 85 values, the string "-" where not ok
state = ["ok", "ok", "…"]
spacing = { eye_to_chin = 0.712, eye_to_mouth = 0.401, width_at_mouth = 1.40, width_at_eyes = 1.74 }
```

- **The image is referenced by hash, never by path**, so a target is self-contained and can never point at a file that
  changed: a different image is a different target.
- **Inputs are the truth; `[measured]` is a cache.** When the image object is in the store, the measure runs again with
  the current code. When it is not (the target arrived without its image), the cache is used if its `measure_version`
  is current, and the output says `"from_cache": true`; otherwise the comparison refuses (`measure-stale-target`).
- **Every input that changes the measurement is stored:** the image, the landmarks, `refine`, `exclude` and `skin` (absent
  `skin`: the adaptive estimate of D4). A target made with the snap off or a skin override therefore reloads with the same
  options. The T10 tests reload such targets and check that the outline bytes match the original measurement on
  the same machine.
- **Provenance:** `Source::Authored`; licence, commercial status and `private_test` are inherited from the image asset.

### 6.2 The character's link

`CharacterDef` gains an optional field (T17, an API change):

```rust
/// The character's likeness targets (VF-01), by name. Absent or empty: none.
#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
pub likeness: BTreeMap<LikenessName, LikenessRef>,

/// A reference to a likeness target in the asset store; the data is never inline.
pub struct LikenessRef { pub asset: ContentHash, pub view: LikenessView /* Front in v1 */ }
```

```toml
# characters/mira/character.toml (excerpt)
[likeness.front]
asset = "b3:…"
view = "front"
```

Two edit ops in `omuzaki-measure` maintain it, and every measure op accepts a target wherever it accepts a face:

| Op | Kind, scope, undo | Does |
|---|---|---|
| `measure.target.set {character, name, face, view, compare}` | `Edit`, `edit:character`, `Auto` | measures `face` (refusing as the measures do), puts the target asset in the store, and patches `characters/<id>/likeness/<name> = {asset, view}`. A dry run measures and returns the would-be hash without writing anything. Undo removes the link; the store object stays until `gc` finds it unreferenced |
| `measure.target.remove {character, name}` | `Edit`, `edit:character`, `Auto` | removes the link |
| subjects | | `{"target": {"asset": "b3:…"}}`, or `{"target": {"character": "mira", "name": "front"}}` resolved through the document (`NotFound` if the character has no such target) |

### 6.3 Private references

A private reference (anything `private-test` in the licence policy, such as a test character's sheet) is handled so that neither
it nor anything measured from it can travel:

- **It lives in a private project's store in the assets folder** (for a test character, its project under
  `$OMUZAKI_ASSETS/characters/<name>/project/`), with its image imported into the same store. Store
  objects are never committed (ADR 0003), so neither the image nor the target reaches git; the character file holds only
  hashes, as for the traced head (CB-11).
- **Its numbers are private data.** The profile of a private reference is a tracing of someone else's design, which the
  policy forbids in shipped design data. They never go into git, PR text, fixtures, goldens, eval sets or library
  items. The private acceptance run (§10.2) reports only pass or fail and the distances (scalars in IOD), which reveal
  nothing about the design.
- **The flag propagates:** the image's `private_test = true` is inherited by the target asset, by every output (marked
  `private`) and by every evidence asset, so the licence gate refuses to publish any of them, and the eval loader
  (`docs/design/evals.md` §11.2), which accepts only items cleared for commercial use, refuses them too.

## 7. Determinism, performance, failure modes

### 7.1 Determinism (ADR 0009)

- Same machine and toolchain: **identical output bytes** (no threads in a measurement, no hash maps, fixed iteration
  order, integer masks, `f64` for geometry and `f32` for the ROI's CIELAB (§4.1), a fixed angle table).
- Across platforms: `r`, spacing and scores equal **within 1e-4 IOD**; states equal. The only path to a bigger
  difference is a value within about 1e-9 of a threshold (a pixel exactly on the skin window's edge), which the tests
  avoid by construction and the doc admits.
- The snap, the fill and the leak guard are pure functions of the decoded pixels and the landmarks.

### 7.2 Performance

Authoring time, not real time (no §10 budget applies), but the ops run inside `plan`:

| Case (release, one thread, the reference machine) | Budget |
|---|---|
| `measure.face_outline` or `feature_spacing`, a 1024² PNG, IOD ≤ 256 px, decode excluded | ≤ 30 ms |
| the same on a 4096² JPEG, decode included (the ROI is downscaled) | ≤ 150 ms |
| `measure.likeness`, two subjects, decode excluded | ≤ 70 ms |
| memory per measurement | ≤ 40 MB (the ROI at ≤ 1.8 Mpx: CIELAB `f32`, two masks, fill labels) |

`benches/face.rs` covers the first and third rows. The Python prototype took about 0.7 s per measurement, all of it in
interpreted loops; the work is O(ROI pixels) for the fill and O(85 × ray length) for the profile.

### 7.3 Failure modes

Refusals (an error, nothing measured) and warnings (measured, with a note):

| Case | Code | What the caller sees / does |
|---|---|---|
| IOD below 64 px | `measure-too-small` (refuse) | give a larger image (renders: at least 160 px, D10) |
| a landmark outside the image | `measure-landmark-outside` (refuse) | |
| mouth above the eye line | `measure-eyes-swapped` (refuse) | eye_left and eye_right swapped, or the face is upside down |
| line art too faint (`S.L - L_line < 15`) | `measure-low-contrast` (refuse) | use the `measure` render preset, or a reference with clearer lines |
| the fill escaped through a gap | `measure-leak` (refuse, with the sector) | render larger; for a sketchy sheet, close the gap or exclude the sector |
| no `ok` sample at 90° | `measure-no-chin` (refuse) | the chin is hidden or outside the image |
| outline coverage below 60%, or a gap in 60° to 120° | `measure-coverage` (likeness refuses) | names the missing sectors; exclude less, or use a clearer view |
| target cache from another `measure_version`, image object not in the store | `measure-stale-target` (refuse) | import the reference image into the store |
| a target name the character does not have | `measure-target-not-found` (`NotFound`) | `measure.target.set` it first |
| a landmark could not be snapped | `snap_failed` (warning) | the given point is used; check it on the overlay |
| asymmetry > 0.06 | `yaw` (warning) | measure a front view |
| fewer than 8 of the 24 mirror pairs have both rays `ok` | `yaw-unchecked` (warning; `asymmetry` is `null`) | the turn is not checked: exclude less, or use a clearer view |
| samples occluded by hair | `occluded` (warning, count and sectors) | |
| line contrast below 25 | `faint_line` (warning) | reference measurements may be less repeatable |
| mouth heights differ by more than 2× between the two faces | `expression` (warning, likeness) | compare neutral faces |
| the ROI was downscaled | `downscaled` (info) | |

Known limits, stated rather than hidden: hair in front of both cheeks symmetrically reads as a narrow face (exclude
it); hair occlusion smaller than 0.04 IOD is not detected (in the prototype a side lock's edge left up to 4% of IOD of
error over three samples); lineless painterly styles with no edge contrast are refused, not guessed.

## 8. How the pieces fit with what exists

| Neighbour | What changes |
|---|---|
| `render.preview` | reports `face_landmarks`; takes a named `preset`, starting with `measure` (D10, T11) |
| stage library (`crates/omuzaki-stage/library/`) | the `measure-flat` light rig and the `measure` render preset file (T12) |
| `CharacterDef` (`omuzaki-char-api`) | an optional `likeness` map of target references by hash (§6.2, T17) |
| `face.layout.measure` | accepts a partial reference, so `layout_ratios` from an image can be passed straight in (T14) |
| evals (`docs/design/evals.md`) | `query` checks need a nested op argument (`image = { op = "render.preview", args, at = "/image" }`) to measure a render in one check, as `render.diff`'s `region` already does; the `face.more-oval` check reads `/aspect` of `measure.face_outline` (comments) |
| `render.compare` | unchanged; `measure.face_overlay` is the outline-specific evidence |
| ML-11 critics | the likeness critic takes these measures as inputs and the §11.3 ratings as its target |
| CO-06, CB-90 | the analysis step sets the sheet as a target (`measure.target.set`), each iteration measures the render against `{character, name}`; the likeness gate is `within` at `cross_look` |
| VF-09 | the default check of the shape and likeness op family is `measure.likeness` against the character's target |

## 9. The prototype and its numbers

A throwaway Python prototype (numpy, scipy; about 500 lines not committed) implemented §4 and drew test
faces with known geometry. **Faces** use our own contract defaults: `HeadOutline::default()`, `Cranium::default()` and
`FaceLayout::default()` for A, and two variants (Appendix A). **Styles:** *render-like* (thin outline in a skin-palette
line colour, no hatching, closed mouth, hair behind the face) and *drawing* (dark tapered lines 2.6% of IOD thick,
hatched blush, an ear, an open mouth, a side lock over one cheek, JPEG quality 80). Drawn at 4× and box-filtered.

### 9.1 Accuracy against the analytic outline (true landmarks, snap off)

| Faces | IOD | RMS error | Largest error |
|---|---|---|---|
| A, B, C render-like | 120, 200 px | 0.27 to 0.52% of IOD | 1.5% |
| A, C drawing | 64, 120, 200 px | 0.34 to 1.07% | 4.2% (one sample at a side lock's edge, under the occlusion threshold) |
| render-like | 64, 80 px | refused (leak) | |

### 9.2 Repeatability

Each face and style measured 12 times: IOD 100, 128, 160 and 200 px, roll -8°, 0° and 6°, random sub-pixel offsets,
and eye and mouth points clicked with Gaussian noise of 2% of IOD, snap on. `O` is the outline term of D7 between two
measurements of the same face (66 pairs).

| Faces | Profile spread (RMS of per-angle SD) | `O` median | `O` p95 | `O` max | Spacing SD (largest field) | Refused |
|---|---|---|---|---|---|---|
| render-like A, B, C | 0.14 to 0.16% | 0.20 to 0.22% | 0.33 to 0.38% | 0.41% | 0.66% (`width_at_mouth`) | 1 of 36 (IOD 100, rolled 6°: leak) |
| drawing A, B, C | 0.14 to 0.18% | 0.19 to 0.20% | 0.33 to 0.45% | 0.54% | 1.02% (`width_max`, C: the side lock) | 0 of 36 |
| **snap off**, render-like A and drawing C | 3.05 to 3.21% | | 6.7 to 8.8% | | 6.1% | |

The snap is what makes clicked or agent-given landmarks usable. An earlier version that snapped to the eye's enclosed
shape had drawing outliers up to 4.2% (the lash-brow-bangs case of §4.1); the window fixed them.

### 9.3 The same face as a render and as a drawing

The analogue of "the sheet against the render": each face drawn in both styles, 12 × 12 pairs.

| Face | `O` median | `O` p95 | `eye_to_chin` | `width_max` | `eye_to_mouth` |
|---|---|---|---|---|---|
| A | 0.62% | 0.84% | +0.83% | +0.22% | -2.9% |
| B | 0.76% | 0.92% | +1.00% | +1.10% | -2.4% |
| C | 0.77% | 1.06% | +0.99% | -1.07% | -2.4% |

The `eye_to_chin` bias is the drawing's thicker lashes raising the eye mark's centroid. The mouth difference is the
open mouth of the drawing against the closed one of the render: an expression difference, hence D10's neutral face and
the `expression` warning.

### 9.4 Different faces, and graded edits

**Different faces** (same style, all pairs of shots): `O` between A and C 5.3 to 5.5% (smallest 5.1%), B and C 7.1%
(smallest 6.7%), A and B 12.1 to 12.4% (smallest 11.9%). The smallest difference between two distinct faces is ten
times the repeatability p95 or more.

**Graded edits of face A** (11 steps, -5 to +5; each step measured at 4 shots with click noise, against one clean
measurement of A):

![Outline distance against graded edits of face A, for both styles: the lower face, the jaw's width, the jaw's
squareness and the eye spacing all rise steadily from zero on both sides; moving the mouth leaves the outline at the
noise floor.](img/likeness-measures-perturbations.png)

| Edit (per step) | `O` at ±1 step | `O` strictly increasing with \|step\| on both sides | The matching spacing field (Spearman with the step) |
|---|---|---|---|
| lower face 4% longer | 1.50 to 1.81% | yes, both styles | `eye_to_chin` +1.000, `aspect` +1.000, `mouth` -1.000 |
| jaw 2% wider | 0.64 to 0.85% | yes | `width_max` +1.000, `aspect` -1.000, `width_at_mouth` +1.000 |
| jaw exponent +0.08 (squarer) | 0.56 to 0.82% | yes | `width_at_mouth` +1.000 |
| eyes 1.5% further apart | 0.95 to 1.25% | yes | `eye_to_chin` -1.000, `width_max` -1.000 (all IOD-normalised) |
| mouth 0.002 m lower (1.1% of IOD) | 0.15 to 0.30% (noise: the outline must not move) | not applicable | `mouth` +1.000 (both styles), `width_at_mouth` -1.000 |

Every outline edit moves `O` monotonically from the first step, and the smallest step of each is above the
repeatability p95 (0.45%). The measure separates what changed: moving the mouth moves `mouth`, not the outline.

### 9.5 Faint outlines and small images

- **Faint outline** (a render-like face with the line 18.6 L* below the lit skin, the palette's worst case): with a
  fixed skin window of -20 the fill leaked and the measurement was refused; with the adaptive window (D4) it measured,
  with RMS error 0.7% on face B and 3.1 to 4.0% on face A. Hence the adaptive window *and* the `measure` render preset (D10).
- **Small images:** render-like faces (outline 1.2% of IOD) were refused below 96 px of IOD and measured from 96 px;
  drawings (outline 2.6% of IOD) measured at 64 px.

## 10. Tolerances

### 10.1 Where the numbers come from

- **`same_look`** (render against render): about twice the worst repeatability. `outline` 0.010 against a p95 of 0.45%
  and a maximum of 0.54%; `eye_to_chin` and `eye_to_mouth` 0.010 against SDs of 0.18% and 0.30% (more than 3σ);
  widths 0.020 against SDs up to 0.66%.
- **`cross_look`** (drawing against render): the style difference of §9.3 plus room for the drawing's own
  imprecision, and well below the smallest difference between distinct faces. `outline` 0.025 is 2.4× the style p95
  (1.06%) and half the smallest distinct-face distance (5.1%); `eye_to_chin` 0.030 is 3× the lash bias (1.0%); widths
  0.035 are 3× their style bias (1.1%).
- In edit terms, the `cross_look` outline tolerance is about 1.5 steps of the lower-face edit of §9.4 (a lower face
  about 6% longer) and the `eye_to_chin` tolerance about 1.3 steps of it. Whether that matches what people notice is
  the human calibration of §11.3. **Until it reports, tolerances may only tighten** (as model floors do in evals §10.1); loosening one is a
  maintainer decision with evidence.

### 10.2 The private acceptance check (VF-01)

VF-01's acceptance: "the outline measured from a test character's sheet and from its render agree with the prototype's numbers
within the stated tolerance". "The prototype" is the JavaScript prototype the Rust port follows. The check lives only in
the private, assets-gated test path (`crates/omuzaki-test/tests/likeness_private.rs`, T13: it skips without the private assets and never runs
in CI):

| Comparison | Inputs (all in the private assets folder, never in git) | Presets | Role |
|---|---|---|---|
| **the sheet against the Rust render** | the sheet's front view as a private target in the character's project store (§6.3); its `game` variant through `render.preview {preset: "measure"}` | `cross_look` | **the gate** (drawing against render, the real use case): `within`, and `outline` ≤ 0.025 |
| the sheet against the JS render | the same target; the prototype's front golden under `goldens/js/` | `cross_look` | reported: the bar the prototype reached |
| Rust render against the JS render | the two renders | **both** `same_look` and `cross_look` | reported: port parity under each preset |

Results (profiles and all terms) are written next to the character's project in the assets store (`characters/<name>/likeness/`), and the PR
shows only pass or fail and the `outline` distances. A failure is a finding about the character's fit and goes to the
maintainers with the overlay. It never loosens a tolerance.

## 11. Validation: how we know the measures track perceived likeness

Three layers, from free and automatic to human:

### 11.1 Synthetic perturbation ladders (CI, every PR touching `omuzaki-measure`)

The prototype's experiments become Rust tests on faces drawn in code (T2; Appendix A), each with exact expectations:

- **Monotonic:** for each of the four outline edits of §9.4 in both styles, `O` against the unedited face strictly
  increases from step 0 to ±5 on both sides; the matching spacing field is monotonic in the step (Spearman = 1).
- **Separable:** the mouth edit leaves `O` below 0.6% and `eye_to_chin` within 0.005; the lower-face and jaw edits
  leave `eye_to_mouth` within 0.01.
- **Repeatable:** the 12-shot battery of §9.2 stays within the bounds of §9.2 plus 20%; refusals only where the
  prototype refused.
- **Accurate:** against the analytic outline, render-like RMS ≤ 0.6% and max ≤ 1.8% at IOD 120 and 200 px.
- **Known negatives:** a blank image, a skin-coloured disc with no features, a face with the eyes swapped, a render-like
  face at IOD 64 px (must refuse with `measure-leak` or `measure-too-small`, never return numbers); the same face
  against a different face (A against B) fails `cross_look`.

### 11.2 Real renders (T11, T15)

On kit characters through `render.preview {preset: "measure"}`: render-given landmarks against the snapped ones agree
within 2% of IOD; `face.set_profile` between `round-u` and `egg-oval` moves `aspect` and `O` monotonically with the blend;
yaw ±5/10/15° and pitch ±5/10° sweeps give the pose sensitivity that sets D11's thresholds.

### 11.3 People (T16, with the evals review set)

What the numbers cannot settle alone is whether they agree with people. Using the local rating page of
`docs/design/evals.md` §10.3, on synthetic faces only (renders of kit characters and drawn faces, never private ones):

- **Triads:** a reference and two candidates at different graded distances; "which looks more like the reference?".
  The measure (`score`) must pick the same candidate on at least 85% of the triads the raters agree on, over at least
  40 triads.
- **Noticeability ladders:** pairs at steps 0 to 5 of each edit; the step where raters notice a difference 75% of the
  time calibrates `cross_look`. The tolerance of each term moves toward that step's measured distance, tightening only
  unless the maintainers decide otherwise.
- The same ratings are the ML-11 likeness critic's training and agreement target; that critic may only become a gate
  through the evals promotion rule (§10.4 there).

## 12. Alternatives rejected (beyond D5's table)

- **Measuring characters, not images** (D1): breaks "the same code on references and renders" and needs the stage.
- **A landmark detector now** (D2): no stylised-face model clears the licence policy; waiting for ML-09 costs
  nothing because the snap makes given landmarks repeatable.
- **Segmenting renders with an object-id mask:** exact, but a sheet has no such mask, so the two sides would be
  measured differently and the half-line bias would land on one side only. The mask is still useful as a *test oracle*
  (T15 may use it to check the extractor on renders).
- **One tolerance for everything:** either too loose for render-against-render regressions or too tight for a drawing
  against a render (§10.1 differ by 2.5×).
- **A score without per-term gates:** a face whose outline is perfect except a chin 6% too long would average to a
  pass.
- **Storing only numbers in a target:** stale silently when the measure improves; D8 stores the inputs.
- **Target files beside the image, found by path** (this design's first draft): a path can change under the target and
  is not part of the project; store assets referenced by hash were chosen instead (§14).
- **Targets inline in the character file:** the character would carry a copy of a reference's data, private or not.

## 13. The tasks

Tasks under VF-01, each blocked by this design and the tasks named.

| Task | Does | Pri | Blocked by |
|---|---|---|---|
| T1 | `omuzaki-measure` crate: manifest, fs fence, link wiring, `ImageRef` and decode with limits (PNG, JPEG), sRGB to CIELAB, the error messages | P0 | this design |
| T2 | test support: the synthetic face drawer of Appendix A (DrawList, `MockRasterizer` at 4×, box filter), analytic truth profiles | P0 | T1 |
| T3 | `face::frame` and `face::segment`: the frame, ROI and downscale, skin and line estimates, adaptive window, fill, holes, background and hair colours | P0 | T1 |
| T4 | `face::snap`: windowed mean shift for eyes, mouth (top edge) and nose; swapped-eye and outside checks | P0 | T3 |
| T5 | **the core:** `face::outline` and the `measure_face` pipeline (rays, strokes, widths, beyond, states, occlusion, leak guard, chin, coverage, asymmetry), the accuracy, repeatability, refusal and determinism tests, a mirror test (a left-right mirrored face gives the same asymmetry and warning), an eligible-pair boundary test at 0, 7 and 8 pairs (0 and 7 give `asymmetry: null` with `yaw-unchecked` and no number; 8 gives a number; asymmetry is `Option<f64>` in the Rust output), the benchmark | P0 | T2, T3, T4 |
| T6 | `face::spacing`: the §4.3 fields and `layout_ratios`; the mouth and chin ladders | P0 | T5 |
| T7 | `face::likeness`: terms, presets, score, gate, worst sector, notes; the four ladders and known negatives | P0 | T6 |
| T8 | the ops `measure.face_outline`, `measure.feature_spacing`, `measure.likeness`: schemas, l10n, errors, tool descriptions, op tests | P0 | T7 |
| T9 | `measure.face_overlay`: the evidence PNG to the store, private provenance | P0 | T8 |
| T10 | likeness targets: the store asset format, `measure.target.set` and `.remove`, `{target}` subjects (by hash or by character and name), the cache and staleness, reload tests with non-default options (`refine` off, `skin` set, `exclude`), private provenance | P0 | T8, T17 |
| T11 | `render.preview`: `face_landmarks` and named presets (`preset: "measure"`); kit-character integration tests | P0 | `render.preview` (the render ops), T8, T12 |
| T12 | the `measure-flat` light rig and the `measure` render preset file | P0 | this design |
| T13 | the private acceptance run of §10.2 (gated on the private assets; gate on sheet against render) | P0 | T10, T11 |
| T14 | `face.layout.measure` accepts a partial reference | P1 | T8 |
| T15 | pose and lighting sensitivity on real renders; D11's thresholds | P1 | T11 |
| T16 | human calibration: triads and noticeability ladders on the review set | P1 | the evals review set, T9 |
| T17 | `omuzaki-char-api`: the optional `likeness` field (`LikenessRef` by hash) | P0 | this design |

Each task spells out its files, the contract it implements (sections of this page) and its acceptance numbers.

Follow-ups (P2, phase 3): research for a licence-clean stylised landmark detector (D2); side and
three-quarter measures (D11).

## 14. Decisions (2026-10-13)

1. **No landmark detector in v1: accepted** (D2, ADR 0029). Snapped given points avoid a model-licence question and
   are exact on our own renders. P2 research for a licence-clean detector (it must pass the licence policy with no
   non-commercial weights or data).
2. **The flat-light measurement look: accepted, as a named render preset** (`measure`, D10), not ad-hoc settings: one
   canonical flat light removes shading as a confound.
3. **Likeness targets are store assets referenced by content hash** from an optional character-document field, the
   pattern of `TracedHead::asset` (D8, §6). The data lives in the store, or in the assets folder for private references, never inline;
   private targets stay out of git, as the traced head (CB-11) does.
4. **Side and three-quarter views: later.** Front first; P2 follow-up.
5. **The private Rust-against-JS check runs both presets; the gate is drawing against render**, the real use case, and
   the check lives only in the private, assets-gated test path (§10.2).

## Appendix A. The synthetic faces (for T2)

Geometry in head-local metres (+y up), drawn front-on; the IOD is `2 · eye_spacing`.

| Face | Outline (`half_width`, `top`, `height`, `exponent`) | Layout (`eye_y`, `eye_spacing`, `eye_width`, `nose_y`, `mouth_y`, `blush_y`) |
|---|---|---|
| A | `HeadOutline::default()`: 0.15, -0.10, 0.10, 1.9 | `FaceLayout::default()`: -0.09, 0.088, 0.05, -0.115, -0.15, -0.13 |
| B | 0.142, -0.095, 0.135, 2.15 | -0.09, 0.088, 0.05, -0.125, -0.17, -0.14 |
| C | 0.155, -0.105, 0.115, 1.82 | -0.09, 0.092, 0.058, -0.122, -0.158, -0.13 |

- **Silhouette:** below `top`, `half_width · (1 - t^P)^(1/P)` with `t = (top - y) / height` (`omuzaki-char`'s
  `half_width_at`); above it, `Cranium::default()`'s front ellipse (half-width 0.186, up 0.186, down 0.197), blended into
  `half_width` with a smoothstep over 0.03 m above `top`.
- **Draw order:** back hair (an ellipse 1.12 × the cranium plus a box down to -0.2 m), neck (half-width 0.055 to 0.06,
  from 0.04 above the chin to 0.12 below), the cast shade under the chin, the collar, ears (drawing only), the face fill,
  the outline stroke from y = 0 round the chin, blush at x = ±0.09 (fill, or three hatch strokes), the nose tick, the mouth (a curve,
  or an open shape filled `#96323c`), the eyes (white ellipse, iris, pupil, highlight, a thick upper lash with an outward
  flick, a thin lower line), brows at `eye_y + 0.058`, the bangs (a jagged edge between about y = -0.037 and +0.005), the side
  lock (drawing only, over the character's right cheek from y = 0.03 to -0.16).
- **Styles:**

| | Render-like | Drawing |
|---|---|---|
| skin, shade | `#f7dccb`, `#e9bda8` | `#ffe6d8`, `#f0c4b2` |
| line | `#b07868`, 1.2% of IOD | `#3b2a2a`, 2.6% of IOD, tapered (0.65 to 1.2 × along each stroke) |
| hair | `#4b5a8c` | `#5c3b2e` |
| extras | | ears, hatched blush, open mouth, side lock, JPEG quality 80 |

- **Shots:** IOD in px, roll about the eye midpoint, a sub-pixel offset; drawn at 4× and box-filtered to the size.
- **Graded edits** (§9.4): `height × (1 + 0.04 t)`, `half_width × (1 + 0.02 t)`, `exponent + 0.08 t`,
  `mouth_y - 0.002 t`, `eye_spacing × (1 + 0.015 t)`, for `t = -5..=5`.
