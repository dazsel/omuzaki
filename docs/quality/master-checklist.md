---
title: "Master checklist"
summary: "Everything Omuzaki covers, level by level: parity with the best tools, what would beat them, and what nobody has invented yet, each marked against the code"
area: quality
---

# Omuzaki master checklist

One list for the whole product. For every area, at every level of abstraction, it says:

1. **Parity:** what the best existing tool in that area does, and whether Omuzaki does it today.
2. **Beyond:** features that would make Omuzaki better than those tools, using what Omuzaki has that they don't (one
   operation layer for people and agents, procedural characters, recorded inputs, measured quality gates).
3. **Not invented yet:** problems where today's best still visibly fails, with a research direction Omuzaki could
   pioneer and the number that would tell us it worked. If subsurface scattering hadn't been invented, this is where it
   would go.

It is the Director's map for starting owners: an area with many ✗ rows and no thread is the next one to staff.

## How to read it

✓ done · ◐ partial · ✗ missing. The status rule, so anyone can re-check a row:

- ✓ the code is on `main` and the row's issue is closed (or the row never had one and its op or test exists);
- ◐ code exists but the row's spec issue is still open, or it covers only part of what the best tool does;
- ✗ no code on `main`.

Spec feature IDs (`CB-30`, `BR-02`) point into `docs/product/02-studio-spec-v3.md`. Issue numbers are the open issue
that carries the row; a row marked **new** was filed with this checklist. Status is as of 2026-10-30 (`main` at
`b5a6d83c`).

## The deep lists this one links to

Some areas already have their own exhaustive list, kept current by the thread building them. This checklist gives
those areas one summary row each and does not repeat them.

| Area | Deep list | Issues |
|---|---|---|
| Character parts in the anime and toon style (eyes, brows, face shape, mouth, ears, skin, makeup, body, start fast) | [creator-parity.md](creator-parity.md) | #2465 to #2477 |
| The photoreal human: renderer for humans, skin, eye, face and head, body and motion | `docs/quality/human-realism.md` (lands with #2506) | #2507, #2508, #2509, #2510 |
| Hair, garments and accessories as systems (84 features, an 8-chunk order) | one issue per feature, with the summary and build order on #2445 | hair #2511 to #2538, garments #2539 to #2559, accessories #2560 to #2571 |
| Short-form video | [reference-shorts.md](reference-shorts.md) | #2393 |

## The whole at a glance

Counts are rows in this file only (the deep lists above keep their own).

| # | Area | ✓ | ◐ | ✗ | Thread on it today | Biggest gap |
|---|---|---|---|---|---|---|
| 1 | [Renderer and materials (stylised)](#1-renderer-and-materials-stylised) | 5 | 5 | 7 | render owner; photoreal side: face owner (#2507) | anime face shadows, material editing for imports, texture painting |
| 2 | [Lighting, post and the look](#2-lighting-post-and-the-look) | 6 | 1 | 5 | render owner | area lights, soft shadows, light matching |
| 3 | [Creating a character](#3-creating-a-character-the-flow-from-idea-to-finished) | 5 | 5 | 5 | creator parity owner (explore, blend); studio UX and agent owner (from a description) | the concept workspace (CO-01 to CO-10) and create from a sheet or a description |
| 4 | [Head, face and eyes](#4-head-face-and-eyes-the-parts) | 3 | 4 | 2 | eye and face owner | the photoreal eye, 3D nose and jaw |
| 5 | [Hair, garments and accessories](#5-hair-garments-and-accessories) | 0 | 3 | 0 | hair, garments and accessories owner | 61 system issues, #2511 to #2571 |
| 6 | [Body, rig and physics](#6-body-rig-and-physics) | 9 | 1 | 6 | **none** | non-human templates, auto-rig, spring gizmos |
| 7 | [Face rig and expression](#7-face-rig-and-expression) | 8 | 1 | 2 | eye and face owner (in part) | a control board; ARKit generation for imports |
| 8 | [Motion and animation](#8-motion-and-animation) | 3 | 3 | 2 | **none** | locomotion, co-speech gestures |
| 9 | [Tracking and inputs](#9-tracking-and-inputs) | 4 | 11 | 6 | **none** | hands, upper body, body over VMC |
| 10 | [Lip sync, voice and audio](#10-lip-sync-voice-and-audio) | 4 | 5 | 3 | **none** | noise suppression, the mixer, singing |
| 11 | [AI co-host and performer](#11-ai-co-host-and-performer) | 2 | 6 | 5 | **none** | perception, memory, listening reactions |
| 12 | [Stage, props and particles](#12-stage-props-and-particles) | 2 | 4 | 3 | render owner (in part) | scene transitions, imported props, collab |
| 13 | [Camera, shots and cinematics](#13-camera-shots-and-cinematics) | 3 | 5 | 5 | reference-video owner (shorts, in part) | the timeline (SH-02), takes, auto-clips |
| 14 | [Broadcast and outputs](#14-broadcast-and-outputs) | 4 | 4 | 5 | **none** | Windows and macOS virtual camera, NDI, direct streaming |
| 15 | [Interactivity and automation](#15-interactivity-and-automation) | 0 | 6 | 3 | **none** | YouTube events, widgets and alerts |
| 16 | [Studio UX](#16-studio-ux) | 9 | 3 | 0 | studio UX and agent owner | gizmos for props, lights, guides |
| 17 | [MCP and the agent surface](#17-mcp-and-the-agent-surface) | 10 | 3 | 0 | studio UX and agent owner | the in-studio assistant |
| 18 | [Versioning, collaboration and the library](#18-versioning-collaboration-and-the-library) | 2 | 4 | 6 | studio UX and agent owner | backups, review links, teams |
| 19 | [Import, export and interop](#19-import-export-and-interop) | 5 | 5 | 6 | render owner (import, in part) | web player, Unity and Godot, PMX |
| 20 | [Plugins, connectors and scripting](#20-plugins-connectors-and-scripting) | 0 | 3 | 2 | **none** | custom nodes, real connectors |
| 21 | [Performance and reliability](#21-performance-and-reliability) | 1 | 2 | 2 | **none** | hardware numbers, the soak, adaptive quality |
| 22 | [Platforms](#22-platforms) | 1 | 2 | 3 | **none** | web player, phone page |
| 23 | [Accessibility, localisation, privacy and safety](#23-accessibility-localisation-privacy-and-safety) | 0 | 7 | 3 | **none** | signed exports, colour-blind themes |
| F | [The whole VTuber](#f-the-whole-vtuber) | 2 | 7 | 2 | Director | time to first character, soak |
| | **Total** | **87** | **99** | **84** | | |

---

# A. Foundations

## 1. Renderer and materials (stylised)

The photoreal side of the renderer (SSS, layered specular, GTAO, area lights, TAA, a path-traced reference) is #2507
and `human-realism.md`. This section is the stylised look most VTubers stream with, and the renderer's general
features.

**Benchmarks:** UniVRM and three-vrm (MToon), Unity Toon Shader (UTS2/UTS3), lilToon, Blender EEVEE and Cycles, VSeeFace
and Warudo (the live look and its cost).

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Shading | Toon shading with a tinted shade colour, rim and skin floor (LK-01) | MToon, UTS | ✓ | `omuzaki-render/src/shader/toon.wgsl` |
| Shading | Every MToon 1.0 field drawn for imported VRMs (LK-07) | three-vrm | ✓ | matcap, rim, UV animation, outline modes in `omuzaki-render`; #592 closed |
| Shading | Special programs: hair band highlight, metal, gem, sheen, shadow catcher | lilToon | ✓ | `omuzaki-render/src/program/` |
| Shading | Anime face shadows that never blob: SDF face-shadow maps or edited face normals, so the nose and cheek shadow fall like a drawing | UTS3, lilToon, game toon pipelines | ✗ | **new** #2572 |
| Shading | Normal, roughness and occlusion maps on kit parts (the kit draws flat albedo) | every PBR tool | ✗ | #2507 (renderer), #2489 (fabric) |
| Shading | Photoreal programs: eye (refraction) and skin (wrapped terminator, GGX, pores) | Unreal, Unity HDRP | ◐ | open in #2506 |
| Shading | Subsurface scattering, transmission | Unreal, Unity HDRP | ✗ | #2507 |
| Lines | Inverted-hull and screen-space outlines, coloured, width by distance (LK-02) | Pencil+, UTS | ✓ | `pass/outlines.rs`, `pass/edges.rs` |
| Lines | Line-art control a 2D artist expects: per-part line weight, taper at tips, inner lines at creases, line boil off | Pencil+ 4, Blender Line Art | ◐ | **new** #2573 (hair #2482 and garment #2556 plug into it) |
| Geometry | GPU morph targets and VRM expressions | three-vrm | ✓ | `omuzaki-render/src/morph/gpu.rs` |
| Geometry | Levels of detail for every character (LOD0 to LODn, strands to cards) | MetaHuman | ✗ | #2476 |
| Transparency | Sorted or order-independent transparency for lashes, hair cards, glass | Unreal, lilToon | ◐ | alpha-to-coverage and a transparent pass exist; OIT is #2507 |
| AA | Supersampling offline; a temporal resolve live | Unreal TAA, DLSS/FSR | ◐ | `frame/supersample.rs`; TAA is #2507 |
| Style | Five style presets: Soft SD, cel-flat, painterly, figure, pixel (LK-06) | none ship all five | ◐ | style presets in `omuzaki-char/library/styles`; pixel is #243 |
| Material editing | Edit an imported model's materials (MToon fields, textures) in the inspector and over ops | UniVRM in Unity, VRoid | ✗ | **new** #2574 |
| Material editing | Paint textures on the model (base colour, shade, masks) | VRoid Studio, Substance Painter | ✗ | **new** #2575 |
| Material editing | A node-based material graph | Blender, Unreal | ✗ | leaf, after the BSDF library of #2507 |

**Beyond them**

- **One look, any style.** The same character renders photoreal, toon, painterly or pixel from one material
  description, with the style as a parameter an agent can sweep (no tool keeps one asset across styles today).
- **Style transfer by measurement.** Given a reference frame, solve shading parameters (shade shift, rim, line weight,
  palette) that minimise a perceptual distance, through ops, so "match this look" is an op, not an afternoon in Unity.
- **Budgets on the material.** Each material reports its GPU cost at live and quality settings, so an agent picks the
  best look that fits the §10 budget.

**Not invented yet**

| Problem today's best still fails | Direction Omuzaki could pioneer | How we'd know it worked |
|---|---|---|
| Toon shading on a 3D face reads as "3D model with a toon shader", never as a drawing, in three-quarter and profile | **View-dependent drawing rules:** learn, per view angle, how anime artists cheat the face (nose as a tick, jaw line moved, far eye squashed) and apply it as a vertex and shadow deformation on the live mesh | blind A/B with artists: the share of frames judged "drawn" rather than "3D", target ≥ 50% at three-quarter |
| Line art in real time either swims (screen-space) or misses inner lines (hull) | **Temporally stable stroke rendering:** extract feature curves from the mesh (silhouette, crease, suggestive contours) and draw them as parameterised strokes that keep their identity frame to frame | stroke flicker: per-stroke positional jitter under a slow orbit, target < 0.5 px at 1080p; artist rating of line quality |
| Photoreal and stylised looks need different assets | **A shared, stylisable BSDF:** one material description with a continuous "abstraction" axis from physical to cel | the same character scored by the parity tool against a photoreal and an anime reference without re-authoring |

## 2. Lighting, post and the look

**Benchmarks:** Unreal (Lumen, post volume), Blender, Warudo and VSeeFace lighting, OBS colour filters.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Lights | Directional, hemisphere, point lights with shadows | every engine | ✓ | `omuzaki-render-api` `Light` |
| Lights | Spot lights, scene fog, image-based lighting from HDRIs (LK-08) | Unreal | ◐ | spot, fog and IBL are in `omuzaki-render`; #593 open |
| Lights | Area lights (rect, disc) | Unreal (LTC) | ✗ | #2507 |
| Rigs | Named light rigs: studio, candle, moonlit, stage, phone-cam (LK-04) | prototype | ✓ | `stage.set_light_rig`, `stage.light_rigs` |
| Rigs | Estimate key, fill and rim from a reference image (LK-04 "match a reference") | none ship it | ✗ | spec LK-04; **new** #2576 |
| Rigs | Screen-reactive light: the avatar lit by the colours of the game or screen being captured | VNyan, Warudo (screen light) | ✗ | **new** #2577 |
| Shadows | Shadow maps fitted to casters | every engine | ✓ | `omuzaki-render/src/shadow/` |
| Shadows | Soft shadows with contact hardening, contact shadows | Unreal | ✗ | #2507 |
| Post | Exposure, tone mapping, bloom, depth of field, film grain, colour grade (LK-05) | Unreal post volume | ✓ | `Post` in `omuzaki-render-api`; `pass/bloom.rs`, `pass/dof.rs` |
| Post | Live mode and quality mode switch (LK-05) | Unreal scalability | ✓ | `Quality` in `omuzaki-render-api` |
| Post | Physical camera (aperture, focal length, EV); AgX and PBR Neutral tone maps | Unreal, Blender | ✗ | #2507, #2504 |
| Palette | Named palette slots, colourways, contrast checker (LK-03) | none ship it | ✓ | `palette.*`, `colourway.*` ops |

**Beyond them**

- **Lighting that keeps identity.** Every rig is solved so the character's palette still reads (hair hue, skin tone,
  eye colour within a set ΔE) under coloured light, the spec's "keep identity under coloured light", checked by
  `measure.*`.
- **Light from the stream.** Light the avatar from the actual on-screen game, the chat's mood or the music, through
  the automation graph.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Matching the lighting of a reference photo or a game scene by hand takes an expert hours | **Inverse lighting for stylised characters:** estimate an environment and a small rig from one frame of a reference, constrained so the toon ramp still reads | the light-match error: ΔE of key-lit, shade and rim regions between render and reference, target < 5 |
| Real-time global illumination on a small stage costs more than a VTuber's GPU budget | **Precomputed, character-aware GI:** bake light transport for the stage and a few character poses, blend at runtime | GI on within the 1.5 ms live budget on the reference laptop, against a path-traced frame within a set error |

---

# B. The character

## 3. Creating a character: the flow from idea to finished

**Benchmarks:** VRoid Studio, MetaHuman Creator, Ready Player Me, Character Creator 4, game character creators (presets,
randomise, sliders), and AI image tools for concept work.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Concept | Brief, mood board, sheet import and generation, turnaround, design analysis, style target, lock, variants, rights (CO-01 to CO-10) | none ship this as one flow | ✗ | #450 to #459 |
| Start | Start from a starter character in one step | VRoid samples, RPM | ◐ | `io.starter.list`, `io.starter.fetch`; #2456 |
| Start | Kit templates and presets for every part | VRoid, CC4 | ✓ | `body.set_template`, `face.eyes.preset`, `hair.bangs.preset`, library items |
| Explore | Explore mode: "six takes", per-part randomise with locks, mix two takes (CB-93) | MetaHuman (blend), game creators (randomise) | ✗ | #2477 |
| Explore | Blend several presets, globally and per region | MetaHuman | ✗ | #2470 |
| Build | Every part a parametric generator with ops | none | ✓ | 121 part kinds, 414 ops; see [creator-parity.md](creator-parity.md) |
| Build | Fit proportions to a reference sheet with a difference heatmap (CB-03) | none | ✓ | `fit.reference`, `fit.apply` |
| Build | Create from a sheet: analysis to build to compare, iterated until the likeness gate passes (CB-90) | none | ✗ | #461 |
| Build | Create from a text description alone, no sheet: brief to parameters to build to self-check | none ship it | ◐ | `character.create_from_description`, `character.brief_vocabulary` (`omuzaki-brief`); the Setup card and the palette; 20 descriptions build and self-check; the ≥ 70% rater bar is the hand check on #2578 |
| Build | Part-level variants and branches (CB-91, CB-13) | none | ✓ | `variant.*`, `branch.*` |
| Check | Character report card: likeness, readability, expression coverage, budget, licence (CB-92) | none | ◐ | `measure.likeness`, `measure.readability`, `face.expression.range_test`; #463 |
| Check | Stress test: poses and head turns report clipping with images (RG-07) | none | ◐ | `fit` checker and hair clearance; whole-character battery is #2485 for hair, rest open |
| Edit | Underlay tracing on the face (CB-25) | VRoid (guide image) | ✓ | `face.underlay.set` |
| Edit | Guide-curve hair editing in the viewport (CB-32) | VRoid | ◐ | `hair.guide.*` ops; no viewport drag yet (§16 gizmos place accessories so far) |
| Edit | Import a mesh as a part, auto-skinned (CB-44) | CC4 | ✗ | #460 |

**Beyond them**

- **An agent makes the whole character through MCP**, from a sentence or a sheet, and a person watches it in the
  Build Theatre and undoes any step. No creator has an agent surface.
- **Characters as recipes, not meshes.** A character is ops plus parameters, so it re-fits to any body, re-exports at
  any LOD and diffs as text.
- **Measured likeness.** "Does it look like the sheet" is a number (`measure.likeness`) the agent iterates against.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| No tool turns a written description into a character a person would call "that's her" without many manual passes | **Description to character by search over the kit:** an agent proposes parameters, renders, scores against the description with a vision model and the brief's constraints, and keeps branches; the kit's parameter priors make every sample valid | blind rating: the share of first results judged to match the description, target ≥ 70%; minutes to an accepted character, target < 10 |
| Turnaround sheets are inconsistent between views, so 3D from a sheet fails at the side and back | **View-consistent reconstruction into kit parameters** (not into a mesh): solve the parameters that explain every view at once, report which view disagrees | silhouette IoU per view after the solve, target ≥ 0.9 on front, side and back |
| Non-artists can't say what's wrong with a character, only that it's off | **Critique from measurements:** readability, proportion and palette checks phrased as "the eyes are 12% smaller than this style's norm", with a one-click fix op | the share of critiques a person accepts; time from "it's off" to "it's right" |

## 4. Head, face and eyes (the parts)

Covered part by part in [creator-parity.md](creator-parity.md) (stylised) and `human-realism.md` (photoreal). The
summary, so the totals here stay honest:

| Level | Feature | Status | Where |
|---|---|---|---|
| Eyes (anime) | iris, highlights, sclera and lids, lashes, aperture, presets and random | ✓ | creator-parity rows; #2465 to #2469 closed |
| Eyes (photoreal) | refractive cornea, iris depth, 3D lids and lashes | ◐ | #2506, #2500 to #2505 |
| Eye motion | pupil reflex, saccades, micro-movements, lid follow | ◐ | anim gaze and the performer; human-realism eye motion is #2510 |
| Face shape | profiles and layout; n-way regional blend | ◐ | #2470 |
| Nose, cheekbones, jaw in 3D | | ✗ | #2471 |
| Mouth, teeth, tongue | painted | ✓ | creator-parity; #2473 |
| Skin | tone range and marks; SSS, wrinkles, pores | ◐ | #1986, #2508 |
| Makeup | five looks | ✓ | #2472 |
| Facial hair | | ✗ | #2475 |

## 5. Hair, garments and accessories

Covered feature by feature in issues #2511 to #2571 (hair 34 features, garments 30, accessories 20, each with today's
state and a done-when), with the summary, the beyond lists and an 8-chunk order on #2445. Summary:

| Level | Feature | Status | Where |
|---|---|---|---|
| Hair | clumps, bangs, guides, physics, colour, strands, angel ring, lock shapes, parting and volume, a style library | ◐ | `hair.*` ops (26); #2511 to #2538; strand grooms with a hair BSDF are #2510 |
| Garments | parametric generators for 15 garment kinds, trims, fit checker, runtime cloth | ◐ | `omuzaki-char/src/outfit`, `cloth.set`; #2539 to #2559 |
| Accessories | jewellery, hats, glasses, wings, tails, horns, halos, headphones with anchors | ◐ | `omuzaki-char/src/accessory`; #2560 to #2571 |

## 6. Body, rig and physics

**Benchmarks:** VRoid (body sliders), MetaHuman (parametric body), Character Creator 4, UniVRM (spring bones), Mixamo and
AccuRIG (auto-rig), Blender (weights), Marvelous Designer (cloth).

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Body | Four templates (Soft SD, mini, standard, tall), each rigged (CB-01) | VRoid (one template) | ✓ | `omuzaki-char/src/template/` |
| Body | Proportion sliders and a five-axis shape space with presets (CB-02, CB-04) | CC4, MetaHuman | ◐ | `body.set_proportion`, `body.set_shape`; measurement targets are #2474 |
| Body | Skin-tone range with per-tone toon tints (CB-04) | none tune toon per tone | ✓ | `palette.set_skin_tone`, `palette.skin_tones` |
| Body | Non-human templates: animal, winged, tailed, blob, object with a face (CB-05) | none | ✗ | #402 |
| Hands | Five-finger hands, hand-shape library (RG-09) | VRoid | ✓ | `body.set_hand_style`, `hand.list_shapes` |
| Rig | VRM humanoid skeleton fitted to proportions (RG-01) | VRoid, UniVRM | ✓ | `omuzaki-rig/src/skeleton.rs` |
| Rig | Generated weights, a paint brush, mirror (RG-02) | Blender | ✓ | `rig.weights.*` ops |
| Rig | Auto-rig an unrigged humanoid mesh (an OBJ or a generated model) | Mixamo, AccuRIG | ✗ | **new** #2579 |
| Rig | IK: look-at, two-bone limbs, hand targets, foot planting (RG-05) | Unity Animation Rigging, Final IK | ✓ | `omuzaki-rig/src/ik/` |
| Rig | VRM node constraints (EX-11) | UniVRM | ✓ | `omuzaki-rig/src/constraint.rs` |
| Physics | VRM spring bones with sphere and capsule colliders, limits, master dial and wind (RG-03) | UniVRM, three-vrm | ✓ | `omuzaki-rig/src/spring/`; `power.set` |
| Physics | Spring bone and collider authoring with viewport gizmos | UniVRM in Unity, VRoid | ✗ | **new** #2580 |
| Physics | Cloth as particle sheets with body collision, pinning, a quality mode and a bake to springs (RG-04) | Unity cloth, Magica Cloth 2 | ✓ | `omuzaki-char/src/cloth/` |
| Physics | Physical reactions: an active ragdoll blended with animation for throws, knocks and falls | Warudo (ragdoll), PuppetMaster | ✗ | **new** #2581 |
| Physics | Hair-to-body clearance after settle | none check it | ✗ | #2485 |
| Deformation | Corrective shapes and machine-learned deformation (elbows, shoulders) | Ziva, MetaHuman | ✗ | #2510 |

**Beyond them**

- **Every body, every outfit, every rig, tested together.** The report card renders every template × shape × tone with
  every outfit in stress poses; nothing ships that clips.
- **Physics tuned by measurement.** Spring and cloth presets solved so motion matches a reference clip's settle time
  and overshoot, not by hand.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Cloth that holds a designed shape (a stiff collar, a pleated skirt, a puffed sleeve) collapses or explodes under fast streamer motion | **Shape-preserving cloth:** a rest shape the designer authored stays the attractor (shape matching plus learned stiffness per region), with a guaranteed-stable integrator at 60 Hz | max penetration and shape error under a fast-motion battery (head whips, arm flails at 3 m/s), target zero explosions in 10 h and < 5 mm shape drift |
| Skinning artefacts at shoulders and elbows on stylised bodies with no sculpted correctives | **Correctives generated from the kit:** since every part is parametric, solve correctives per template from volume preservation, no sculpting | volume loss at the elbow at 120°, target < 2%; candy-wrapper twist, target none |
| Spring-bone hair looks like rubber; strand simulation is too slow | **Learned reduced hair dynamics:** train a small network on offline strand simulations of each hairstyle so the live chain moves like strands at spring-bone cost | motion error against the offline strand sim, at ≤ 0.2 ms per character |

## 7. Face rig and expression

**Benchmarks:** Live2D Cubism (expressiveness), VTube Studio (expression toggles), VRoid (expression editor), MetaHuman
(control board, correctives), HANA_Tool (ARKit shape generation).

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Channels | ARKit-52 plus tongue on every character, per side (FE-01) | iPhone perfect sync | ✓ | `omuzaki-face`; #26, #171, #319 closed |
| Channels | Anime extras as channels: ^^, wink arcs, heart and star eyes, tears, sweat, blush, gloom, shock, fangs (FE-02) | VTube Studio (via art) | ✓ | #27 closed |
| Library | Named expressions with fades, auto-stop, exclusive groups, layers, a default (FE-03) | VTube Studio | ✓ | `face.expression.*` ops |
| Library | At least 18 expressions, a range test that flags look-alikes and dead controls (FE-04) | none | ✓ | `face.expression.range_test`, `face.channel.sweep` |
| Rules | Anime grammar rules: smiling blink to ^^, laugh to squint and bob (FE-05) | none | ✓ | `omuzaki-anim/src/grammar` |
| Profile | Exaggeration profiles, realistic to anime (FE-06) | VTube Studio (per-parameter) | ✓ | `face.set_exaggeration` |
| Gaze | Look-at targets, saccades, eyes lead the head, convergence (FE-07) | Warudo | ✓ | `omuzaki-anim/src/gaze` |
| Bake | Painted expressions baked to VRM blendshapes or flipbooks (FE-09) | none (perfect sync costs paid rigging) | ✓ | `omuzaki-face/src/bake/` |
| Imports | Generate missing ARKit shapes for an imported model from its own morphs (EX-12) | HANA_Tool | ◐ | `avatar.map_channel`; generator part of EX-12 |
| Control | A control board: 2D handles per region a person drags to pose the face | MetaHuman control rig | ✗ | **new** #2582 |
| Control | Expression authoring by example: pose the face from a reference photo of an expression | none | ✗ | covered by the "not invented" row below |

**Beyond them**

- **Every character speaks perfect sync for free**, procedural or imported, because the face is generated and the
  missing shapes are synthesised.
- **Expressions checked for readability** at emote size, stream size and close-up, as numbers.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Expressions are channel blends; the "life" of a drawn anime face (asymmetry, timing, smears, overshoot) is missing | **Expression dynamics, not poses:** model each expression as a short trajectory with anticipation, overshoot and asymmetric timing, learned from licence-clean animation, played under tracking | liveliness metric (§15) and blind preference against static blends, target ≥ 65% preferred |
| A person can't make a new expression without knowing what 52 channels do | **Expression from intent:** "smug but tired" or a reference photo becomes a channel blend through a solver over the readability metric | blind match rating ≥ 70%; time to a new expression < 1 min |

## 8. Motion and animation

**Benchmarks:** Mixamo, Unity Mecanim, Cascadeur (physics-aware posing), Warudo blueprints, VRM Animation, Blender.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Poses | Pose library with blending and mirroring (RG-06) | VRoid poses | ✓ | `pose.*` ops, `omuzaki-anim/src/poses` |
| Retarget | ARKit-52 and humanoid retarget onto any avatar | Warudo | ✓ | `omuzaki-anim/src/retarget`, `perform.set_retarget` |
| Life | Life layer: blinks, saccades, breathing, speech nods, personality presets (PF-24) | VTube Studio idle, Warudo | ◐ | `omuzaki-anim/src/life`; #477 |
| Clips | Motion import: BVH, FBX, VRMA retarget (RG-08) | Mixamo, Unity | ◐ | VRMA in `omuzaki-io-vrm`; BVH and video to motion #465, #588 |
| Clips | VRMA export (EX-16) | UniVRM | ✓ | `io.export_animation` |
| Locomotion | Walk, run, jump, sit and emotes from keyboard or gamepad with a state machine (PF-30) | Warudo, VNyan | ✗ | #764 |
| Gesture | Co-speech body gestures generated from speech for voice-only and AI performers | none in VTubing | ✗ | #2349 |
| Cleanup | Foot-lock, jitter filter, retime as layers over a take (SH-11) | Cascadeur, MotionBuilder | ◐ | foot lock in `omuzaki-rig/src/ik/feet.rs`; #606 |

**Beyond them**

- **Motion is an op stream.** Every clip, pose and correction is an op, so an agent can direct a performance ("wave
  twice, then look at chat") and a person can undo any beat.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| The uncanny valley in motion: tracked avatars look like puppets because the body below the face is frozen or noisy | **Whole-body inference from face and voice:** infer plausible shoulders, spine and hands from head motion, speech and emotion so a webcam-only streamer has a living body | blind rating against face-only and against mocap, target indistinguishable from mocap in ≥ 40% of clips; no frozen 0.5 s windows |
| Licence-clean motion models barely exist (most are trained on SMPL-derived data, banned by our policy) | **A licence-clean motion dataset and model**, recorded with our own volunteers and the project's tools (ML-12) | coverage of the pose library and gesture set; motion quality metric against a held-out set |

---

# C. Live

## 9. Tracking and inputs

**Benchmarks:** VSeeFace and VTube Studio (tracking feel), MediaPipe (accuracy), iFacialMocap and Live Link Face (iPhone),
Warudo (MediaPipe body and hands), XR Animator, mocopi and SlimeVR (body), Leap Motion.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Face | Webcam: 52 blendshapes plus head pose, off the main thread, newest frame wins (PF-01) | MediaPipe | ✓ | `omuzaki-track` |
| Face | Quality tiers, camera picker, image controls, landmark overlay, edge warnings (PF-01) | VTube Studio | ◐ | `input.cameras`, `input.select`, `input.preview` |
| Face | Tracking within 5 ms p95 (#903) | VSeeFace | ◐ | measured on CI's VM; hardware runs are hand checks |
| Face | iPhone ARKit receivers: iFacialMocap, VTube Studio phone, Live Link Face (PF-02) | Warudo | ✓ | `omuzaki-track-net/src/protocol/` |
| Face | Android phone apps over the same protocols (PF-03); a web phone page (PF-10) | VTube Studio Android | ◐ | receivers done; web page #595 |
| Face | Tongue from the webcam (MediaPipe has no tongue channel) | none reliable | ✗ | covered by the "not invented" row below |
| Body | VMC in for face; body bones over VMC (PF-04) | Warudo, VSeeFace | ◐ | face done; body bones #468 |
| Body | VMC out (PF-04) | VSeeFace | ✓ | `input.vmc_out` |
| Body | Upper body from the webcam (PF-07) | Warudo, XR Animator | ✗ | #471 |
| Body | OpenXR and SteamVR trackers (Vive trackers, Index controllers) as inputs | VNyan, VirtualMotionCapture | ✗ | **new** #2583 |
| Hands | Webcam hand tracking and Ultraleap; finger curls; gestures as triggers (PF-06) | Warudo, VSeeFace (Leap) | ✗ | #470 |
| Inputs | Keyboard, mouse, gamepad, MIDI, OSC bindings (PF-05) | Warudo | ✓ | `input.binding.*`, `omuzaki-input` |
| Inputs | Stream Deck: buttons that run ops, with state shown on the keys | VTube Studio and Warudo plugins | ✗ | **new** #2584 |
| Inputs | Input-driven body motion: hands on the keys, arm to the mouse (PF-29) | VNyan, Warudo | ◐ | #718, #783 |
| Inputs | A recorded video file as a tracking input (re-track footage offline) | none for VTubing | ✗ | **new** #2585 |
| Processing | Guided calibration in one step (PF-20) | VTube Studio | ◐ | `input.calibrate`, `omuzaki-track/src/guided.rs`; #473 |
| Processing | One-Euro smoothing and a latency dial (PF-21); per-channel curves (PF-22) | VTube Studio | ◐ | `omuzaki-track/src/smoothing.rs`; #474, #475 |
| Processing | Source blending and ownership per channel (PF-23) | none | ◐ | #476 |
| Processing | Triggers, combos, timed expressions, gestures (PF-25) | VTube Studio hotkeys | ◐ | `trigger.*`, `input.gestures`; #478 |
| Processing | Tracking doctor: glare, low light, backlight, jitter, overload, with fixes (PF-28) | none | ◐ | `input.doctor`, `input.recheck`; #597 |
| Processing | Live health meter (PF-26) | VSeeFace | ◐ | `input.health`; #479 |

**Beyond them**

- **The tracking doctor** says what is wrong in plain words and how to fix it; no tracker diagnoses itself.
- **Everything recorded is re-trackable.** Raw inputs are kept, so a better tracker next year improves last year's
  takes.
- **One channel bus** that a webcam, an iPhone, audio, an AI and a hotkey all write to, with ownership per channel.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Webcam trackers miss the small motions that make a face read as alive: micro-expressions, lip roll, cheek puff, tongue, asymmetric brows | **High-frequency facial detail from a webcam:** a licence-clean model trained on synthetic faces rendered by Omuzaki itself (the kit is a free, perfectly labelled data generator) for the channels MediaPipe lacks | per-channel accuracy on a synthetic test set and on hand-labelled real clips; tongue detection F1 ≥ 0.9 |
| Webcam gaze is noisy, so eye contact with the audience breaks | **Gaze with intent:** fuse iris tracking, head pose and the screen layout (where chat and the game are) to snap gaze to likely targets | angular gaze error, target < 3°; share of frames with correct target |
| Hands from one webcam lose fingers when they overlap the face or each other | **Hand tracking that knows the body:** a model conditioned on the avatar's skeleton and the face position | finger-joint error under occlusion against a mocap reference |
| Every tracker drifts over a long stream | **Self-calibrating tracking:** continuous re-estimation of the neutral face and ranges from the person's own idle moments | channel drift over 12 h, target < 2% of range |

## 10. Lip sync, voice and audio

**Benchmarks:** OVR Lipsync and uLipSync (visemes), Rhubarb, VTube Studio and VSeeFace (audio lip sync), Voicemod and
w-okada (voice changers), NVIDIA Broadcast (noise removal), VoiceMeeter (routing).

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Lip sync | From tracking, from audio visemes, or blended (FE-08) | uLipSync, Rhubarb | ✓ | `omuzaki-track-voice/src/lipsync.rs`; #464, #2351 closed |
| Lip sync | Voice activity, loudness and onset cues, laughter detection (PF-08) | VTube Studio | ◐ | `omuzaki-track-voice/src/onset.rs`, `cues.rs`; #472 |
| Lip sync | Per-user vowel calibration (PF-08) | none | ◐ | #472 |
| Lip sync | Singing: sustained vowels, vibrato and fast phrases on sung vocals, with vocal isolation from music | none | ✗ | **new** #2586 |
| Voice | Works with any voice changer, voice taps, A/V delay with a clap test (PF-12) | none | ✓ | #910 closed |
| Voice | Built-in voice effects: pitch, formant, robot, radio, gate, EQ, compressor (BR-24) | Voicemod | ✓ | `audio.fx.*` ops, `omuzaki-audio-fx` |
| Voice | AI voice conversion (ML-14) | w-okada RVC | ✗ | #690, #918 |
| Audio | A mixer with mic, soundboard, AI voice and music buses (BR-22) | VoiceMeeter, OBS | ◐ | `audio.bus.*` ops; #603 |
| Audio | Soundboard with provenance and ducking (BR-21) | Streamer.bot, Voicemod | ◐ | `omuzaki-audio-api`; #602 |
| Audio | Echo cancellation and noise suppression on the mic | NVIDIA Broadcast, RNNoise | ✗ | echo #1268; noise **new** #2587 |
| Speech | Local ASR (Whisper), local TTS (Kokoro), VAD (Silero) | Open-LLM-VTuber | ✓ | `omuzaki-speech`, registered models |
| Speech | Voice commands to avatar actions (AP-10) | none built in | ◐ | `voice_commands.*` ops; #719 |

**Beyond them**

- **The voice tap:** one place that says which signal drives the mouth, captions and the AI's listening, so a changed
  voice never breaks lip sync or recognition.
- **The mouth matches the character, not the phoneme.** Mouth shapes per style (a chibi mouth opens differently from a
  tall character's), mapped through the face painter.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Audio-driven mouths look like flapping: right shapes, wrong timing and no coarticulation, and no emotion | **Expressive audio to face:** one model from voice to the whole face (mouth, cheeks, brows, eyes, head), with emotion read from prosody, trained licence-clean on synthetic Omuzaki renders plus consented recordings | lip-sync error (LSE-D) and a blind rating against tracked performances; target: audio-only rated as good as tracking in ≥ 50% of clips |
| Lip sync for sung vocals over music fails everywhere | **Singing-aware visemes:** vocal isolation, pitch-aware vowel holding and breath detection | viseme accuracy on sung test tracks; blind rating |

## 11. AI co-host and performer

**Benchmarks:** Open-LLM-VTuber, AIRI, open-annie, Neuro-sama (as the reference for what audiences love), Character.AI
voice, NVIDIA ACE and Audio2Face.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Persona | Personality, backstory, style, boundaries, versioned (AP-01) | Character.AI | ◐ | `performer.persona.set`; #481 |
| Voice | TTS voice per character, local or hosted; speech-to-speech (AP-02) | ElevenLabs, OpenAI realtime | ◐ | `voice.*` ops, `omuzaki-llm-realtime`; #482 |
| Loop | Listen, read chat, think, speak, barge-in under 200 ms, turn-taking (AP-03) | open-annie, Open-LLM-VTuber | ◐ | `omuzaki-cohost`, `omuzaki-performer`; #483 |
| Loop | Opt-in perception: see a screen or camera and react; proactive speech (AP-03) | Neuro-sama | ✗ | #483 |
| Intents | emote, gesture, look_at, move_to, speak, play_clip (AP-04) | AIRI | ◐ | `omuzaki-performer/src/decide.rs`; #484 |
| Face | Voice and intent drive the face and body (AP-05) | Audio2Face | ◐ | AI mouth in `omuzaki-track-voice/src/ai_mouth`; #485 |
| Reactions | Listening reactions while the person talks | none | ✗ | #2453 |
| Decision | A rule floor that reacts when the LLM is slow or absent | none | ✓ | `omuzaki-performer/src/systemone.rs` |
| Providers | Local and hosted LLMs (Anthropic, OpenAI-compatible) as bring-your-own-key | Open-LLM-VTuber | ✓ | `omuzaki-llm` |
| Memory | Per-viewer and per-show memory with privacy controls (AP-06) | Neuro-sama | ✗ | #486 |
| Safety | Filter before speech, boundaries, kill switch, disclosure labels (AP-07) | none complete | ◐ | #487, #1449 |
| Modes | Co-host, cover, assist (AP-08); rehearsal against simulated chat (AP-09) | none | ✗ | #488, #489 |
| Chat | Viewer TTS chat reader (AP-11) | Speaker.bot | ✗ | #720, #740 |

**Beyond them**

- **The AI uses the same ops as the person.** It can change its own expression, the scene, the camera, a prop, under
  the same approvals and undo, so the co-host is a performer, not a talking head.
- **Reactions never wait for the LLM.** The rule floor keeps the face alive at frame rate while the model thinks.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| AI characters are slow and miss timing: they talk over people, pause oddly and laugh late | **Full-duplex conversational timing:** predict the end of the person's turn from prosody and content, start the reply's face and breath before its words, and backchannel ("mm", nods) while listening | response gap p50 < 300 ms; overlap and interruption rates within human ranges; blind rating of "felt natural" |
| AI performers lack comic timing and stagecraft | **A stage-aware performer:** the planner sees the scene, camera and chat state and plans beats (setup, pause, punchline, reaction shot) through ops | audience retention on A/B rehearsal runs; human rating of "entertaining" |
| An AI's face doesn't match what it means: the words are sarcastic, the face is happy | **Meaning to face:** the intent layer carries subtext tags the face model renders | rater agreement that the face matches the line's intent, target ≥ 80% |

---

# D. The show

## 12. Stage, props and particles

**Benchmarks:** Warudo (scenes, props, interactions), VNyan, VSeeFace, Unreal (level), VTube Studio items.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Scenes | Scenes with backdrops, props, lights, cameras and presets (ST-01) | Warudo | ◐ | `stage.*` (37 ops); image, video and 3D-room backdrops are #490 |
| Scenes | Scene switching with transitions from hotkeys, chat, timeline (ST-04) | OBS, Warudo | ◐ | switch by op (`stage.set_default_scene`); transitions and triggers #493 |
| Props | Built-in prop kinds (room, desk, chair, bed, cafe table, lamp, plant, rug, shelf, window, cup) | Warudo | ✓ | `stage.prop_kinds`, `stage.add_prop` |
| Props | Props attached to hands, physics props, animated, image, GIF and text props, prop sets, in-scene screens (ST-03) | Warudo, VTube Studio items | ◐ | `stage.interact`; #492 |
| Props | Import a glTF prop or room | Warudo, VNyan | ✗ | part of #490 and #492 |
| Cast | Several characters on one PC, a human and an AI side by side (ST-12) | Warudo | ◐ | `stage.place_character`; #496, #929 |
| Cast | Characters look at each other (ST-11) | none | ✗ | #495 |
| Effects | Particles and weather: confetti, emote rain, rain, snow, petals (ST-13) | Warudo | ✓ | `stage.effect.*`, `stage.set_weather` |
| Collab | Shared stage over QUIC with invite links and encryption (ST-10) | VSeeFace (VMC), Warudo | ✗ | #494 |

**Beyond them**

- **A stage an agent can dress.** "Make a cosy rainy night café for tonight's stream" is a sequence of ops an agent
  plans, renders for approval and the person tweaks.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Collab streams between VTubers add latency and drift, and characters can't touch or hand things to each other | **Shared physics across the internet:** predicted, authority-split physics for props and contact (high fives, hugs) at ≤ 30 ms added latency | added latency p95 and contact error (cm) between participants' views |
| Making a good stage takes a 3D artist | **Stages from a description,** built from the prop kit and procedural rooms, scored for composition behind the character | blind rating of stage quality; minutes to an accepted stage |

## 13. Camera, shots and cinematics

**Benchmarks:** Unreal Sequencer, Blender, Warudo cameras, Unity Timeline and Cinemachine, OBS move transitions.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Camera | Framings that follow the head; handheld drift; punch-ins; lens choice (ST-02) | Cinemachine | ◐ | `stage.set_camera`, `stage.set_active_camera`; #491 |
| Camera | Viewport orbit, pan and frame | Blender | ✓ | `omuzaki-ui/src/panels/viewport/camera.rs` |
| Camera | An automatic director that cuts between framings by who speaks, emotion and events | Cinemachine (rules), none for VTubing | ✗ | **new** #2588 |
| Takes | Record all inputs so a take re-renders later (SH-01) | none | ◐ | `omuzaki-take-api`; #505 |
| Timeline | Tracks for channels, head, camera, lights, props, audio; keyframes, curves, layers (SH-02) | Unreal Sequencer | ✗ | #506; a curve-editor widget exists in `omuzaki-ui/src/ds/widgets` |
| Score | Author a performance as a score; audio-synced (SH-03) | none | ◐ | `omuzaki-anim/src/score_templates`, `short.*` ops; #507 |
| Render | Deterministic offline render, any size, alpha, a queue (SH-05) | Blender | ◐ | `render.preview`, `render.turnaround`; #509, #1380 |
| Shorts | Recreate a short from a script (`short.*`) | none | ✓ | `omuzaki-short`; [reference-shorts.md](reference-shorts.md) |
| Reference | Reference replication from a clip (SH-04) | none | ✗ | #508 |
| Clips | Auto-clips from a stream: highlights, 9:16 reframe, captions (SH-06) | Opus Clip, Eklipse | ✗ | #510 |
| Stills | Emotes and stickers at 112/56/28 px (SH-07) | none | ◐ | emote readability in `omuzaki-measure`; packs open |
| Stills | Thumbnails and key art presets (SH-08) | none | ✗ | #511 |
| Stills | Character sheets: turnaround and expression sheets (EX-10) | VRoid (none) | ✓ | `render.turnaround`, `render.contact_sheet`; #1152 closed |

**Beyond them**

- **Re-render any stream.** Because takes are inputs, a two-hour stream becomes a 4K clip from a new camera, with a
  different outfit, after the fact.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Live VTuber streams are one static shot for hours; a human camera operator is unaffordable | **A virtual camera operator** that frames for emotion and story in real time (lean in on a sad moment, cut wide for a laugh), trained on licence-clean edited content and steered by rules | viewer retention A/B; editor rating of the cuts |
| Turning a stream into shorts still needs an editor | **Story-aware auto-editing over takes:** find the arc, re-stage it with new cameras from the recorded inputs, add captions | share of auto-shorts a creator posts unchanged |

## 14. Broadcast and outputs

**Benchmarks:** OBS (outputs, recording, replay), VTube Studio and VSeeFace virtual cameras, NDI Tools, Spout.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Out | Spout2 (Windows) and Syphon (macOS) with alpha (BR-01) | VTube Studio | ✓ | `omuzaki-stream-share` |
| Out | NDI (BR-01) | VTube Studio | ✗ | #497 |
| Out | Virtual camera on Linux (v4l2loopback) (BR-02) | OBS | ✓ | `omuzaki-vcam` |
| Out | Virtual camera on Windows and macOS (BR-02) | VSeeFace, VTube Studio | ✗ | #498 |
| Out | A clean capture window (BR-04) | VSeeFace | ✓ | `output.window.*` ops |
| Out | An OBS plugin source (BR-03) | VTube Studio's | ✗ | spec BR-03 (separate GPL repository) |
| Out | A second, vertical (9:16) program output with its own camera, alongside the 16:9 one | Aitum Vertical (OBS plugin) | ✗ | 9:16 program #2401; both at once **new** #2589 |
| Out | Direct streaming over RTMP, SRT, WHIP with hardware encoders (BR-05) | OBS | ✗ | #499 |
| Record | Recording with alpha: WebM VP9 alpha, PNG sequence; ProRes opt-in (BR-18) | OBS | ◐ | `record.*` ops, `omuzaki-stream/src/sinks/`; #1430 |
| Record | Replay buffer (BR-17) | OBS | ◐ | `omuzaki-stream/src/replay`; #599 |
| Control | OBS WebSocket v5 control (BR-19) | Streamer.bot | ✓ | `omuzaki-obs`; #600 closed |
| Reliability | Live-show watchdog: workers restart without dropping the stream (BR-23) | none | ◐ | `omuzaki-stream/src/watchdog`; #604 |
| Window | Desktop mascot mode (BR-25) | Desktop Mate, Kirara | ◐ | `omuzaki-ui/src/aux_window`; #722, #2039 |

**Beyond them**

- **The avatar arrives with alpha everywhere, with no plugin**, at the §10 latency.
- **The output recommends itself:** `output.recommend` picks the best route into whatever the person has running.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Glass-to-glass latency of a VTuber (camera to tracker to render to OBS to viewer) is rarely under 80 ms and nobody measures it | **An end-to-end latency probe:** a light pulse or audio click through the whole chain, measured automatically, shown in the health meter | measured camera-to-photon p95 ≤ 50 ms on desktop, published per release |

## 15. Interactivity and automation

**Benchmarks:** Streamer.bot, Mix It Up, Warudo blueprints, VTube Studio's item throws (Kawaii Throws), StreamElements
and Streamlabs (alerts, widgets).

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Events | Twitch EventSub: chat, bits, redeems, follows, raids, subs, polls, hype trains (BR-10) | Streamer.bot | ◐ | `omuzaki-twitch`; #500 |
| Events | YouTube: chat, Super Chat, memberships (BR-10) | Streamer.bot | ✗ | #2352 |
| Events | Bilibili, TikTok LIVE adapters (BR-15, BR-16) | Bilibili tools | ✗ | #598, #1428 |
| Graph | Automation graph: event to condition to action, templates, every node an op (BR-11, BR-26) | Streamer.bot, Warudo blueprints | ◐ | `omuzaki-live`, `omuzaki-live-nodes` (75 nodes), the Broadcast graph panel; #501, #723 |
| Throws | Item throws with hit reactions and cooldowns (BR-12) | Kawaii Throws, VTube Studio | ◐ | `stage.interact`; #502 |
| Overlays | Captions, an AI chat bubble and alerts drawn into the output (BR-13) | StreamElements | ◐ | captions in `omuzaki-speech`; #503 |
| Overlays | Native and web widgets: chat box, goals, timers, now playing (BR-20) | StreamElements, Streamlabs | ✗ | #601 |
| Moderation | Blocklists, rate limits, a panic button (BR-14) | Streamer.bot | ◐ | `omuzaki-live-api`; #504 |
| Bridges | VTube Studio API shim, VMC, OSC (PL-05) | VTube Studio | ◐ | `omuzaki-vts`, `vts.map`; #529 |

**Beyond them**

- **The graph is ops.** Any automation an agent writes is inspectable, testable with a dry run and undoable, and the
  same nodes run in the studio, the CLI and scripts.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Stream interactivity is canned (the same throw, the same alert) and gets stale | **Generative interactions:** chat and redeems produce new, safe, in-character reactions composed from ops at run time, rated and remembered | repeat rate of reactions over a month of shows; viewer engagement A/B |

---

# E. The platform

## 16. Studio UX

**Benchmarks:** Blender (editing feel), VRoid Studio (approachability), Figma (panel clarity), OBS (familiarity),
Warudo (live workflow).

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Shell | Workspaces (Build, Perform, Broadcast, Script, Theatre) with docked panels | Blender | ✓ | `register_workspace!`, `omuzaki-ui/src/shell` |
| Viewport | The character rendered live in the viewport with orbit, pan, frame | Blender, VRoid | ✓ | `panels/viewport` |
| Viewport | Click a part to select it, Shift+click to add, the selection boxed as posed | Blender | ✓ | `panels/viewport/pick.rs` |
| Viewport | Move, rotate and scale gizmos for props, lights, cameras, guides and colliders | Blender, Unity | ◐ | accessories: `panels/viewport/gizmo.rs` writing `part.set_attach`, one undo step per drag; props, lights, cameras, guides and colliders are next, #2590 |
| Panels | Outliner, schema inspector, library, history, agent panel | Blender, Figma | ✓ | `panels/` |
| Panels | Command palette over every op | Blender F3, VS Code | ✓ | `shell/palette.rs`; a query of three or more words offers "Make a character from these words" |
| Panels | History showing who did what with undo and redo, thumbnails (VC-01) | Figma | ✓ | `shell/history.rs` |
| Live | Perform workspace with health, channels and stage | Warudo | ✓ | `omuzaki-ui/src/live` |
| First run | First-run wizard to live in under 2 minutes (SE-01) | VSeeFace (fast), VTube Studio | ◐ | `panels/setup` (sample, describe in words, import); QG-03 #1467 |
| Save | Autosave and crash recovery (VC-08) | Blender | ✓ | `shell/saver.rs`, `omuzaki-app/src/recover.rs` |
| Feel | Undo for every action, including an agent's | Figma | ✓ | `omuzaki-ops` executor |
| Feel | Taste bar: a first-time user understands without a manual (QG-03 hand test) | VRoid | ◐ | hand-only check |

**Beyond them**

- **Watch an agent work.** The Build Theatre shows an agent's plan, steps and renders live, with approval cards.
- **No UI-only features.** Every button is an op, so the palette, MCP and scripts can always do the same thing.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Creative tools are either simple and shallow or deep and hostile; nobody adapts to the person | **An interface that grows with the person:** the studio watches which ops a person uses and surfaces depth progressively, and an agent offers the next step in context | time to first character for a new user, target < 30 min (QG-01); retention of depth features after a week |
| Describing a change in words is faster than finding the slider, but text commands are vague | **Point and say:** click a part in the viewport and say "a bit longer and wavier"; the agent maps it to ops with a preview | share of spoken edits accepted on the first try |

## 17. MCP and the agent surface

**Benchmarks:** the MCP reference servers, Blender MCP, Unity MCP, Figma's dev mode MCP, Blender's operator and undo
model.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Tools | Every registered op is an MCP tool with a typed schema | none at this coverage | ✓ | `omuzaki-mcp`; 414 named ops |
| Transport | stdio and streamable HTTP, attached to the running studio | MCP reference servers | ✓ | `omuzaki-mcp/src/http`, `in_app.rs` |
| Safety | Dry run, transactions, undo and redo from both sides | Blender operators | ✓ | `omuzaki-ops` |
| Safety | Sessions, approvals, budgets, scopes, tokens | none | ✓ | `omuzaki-ops/src/sessions.rs`, `approvals.rs`, `budget.rs` |
| Context | Resources and prompts; recipes | MCP spec | ✓ | `omuzaki-mcp/src/resources`, `prompts.rs`, `recipes` |
| Seeing | Renders, contact sheets and comparisons returned to the agent | Figma MCP (screenshots) | ✓ | `render.preview`, `render.compare`, `measure.*` |
| Create | A character from one sentence in one call: every choice reported with its words, what was set aside, a self-check, and the vocabulary to name facets directly | none | ✓ | `character.create_from_description`, `character.brief_vocabulary` |
| Jobs | Long jobs with progress and cancel | none | ✓ | `omuzaki-ops/src/jobs`, `omuzaki-mcp/src/jobs.rs` |
| CLI | `omuzaki run, undo, redo, mcp` | Blender CLI | ✓ | `omuzaki-cli` |
| Script | Rhai script host with macros | Blender Python | ✓ | `omuzaki-script`, `script.*` ops |
| Assistant | An in-studio assistant host (AG rows) | Copilot in tools | ◐ | `omuzaki-agent`; #547 to #553 |
| Coverage | 100% of UI actions as tools (QG) | none | ◐ | lint in `xtask lint-ui-writes` |
| Evaluation | An agent eval suite: make a target character through ops only, scored | none | ◐ | `docs/design/evals.md`; #2445 is the live target |

**Beyond them**

- **An agent can do anything a person can, and a person can watch and undo it.** That is the product's centre, and no
  creative tool has it at full coverage.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Agents edit creative work blind: they call tools but can't judge taste, so results are generic | **Taste as a tool:** perceptual and stylistic measures (readability, appeal, likeness, style consistency) exposed as MCP tools the agent optimises against, calibrated on human ratings | correlation of the measures with human ratings, target ρ ≥ 0.7; blind preference for agent-made characters after optimisation |
| Agents lose the thread over a long build | **Design memory:** the locked design, decisions and rejected options as a queryable resource | rework rate on long sessions |

## 18. Versioning, collaboration and the library

**Benchmarks:** Git, Figma (branching, comments, multiplayer), Blender asset browser, Unity Asset Store.

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| History | Browsable history, restore any point (VC-01) | Figma | ✓ | `history.*` ops |
| Branches | Branch, compare, merge part-level changes (VC-02) | Figma, Git | ◐ | `branch.list`, `branch.status`, `omuzaki-app/src/branches` |
| Diffs | Visual diffs with heatmaps (VC-03) | Figma | ◐ | `history.diff`, `render.compare` |
| Journal | Crash-safe journal, kill -9 loses nothing (VC-08) | none | ✓ | `omuzaki-doc/src/journal` |
| Backups | Rolling verified backups (VC-09) | Time Machine | ✗ | spec VC-09 |
| Git | Git integration with render previews (VC-04) | Git | ✗ | #521 |
| Review | Review links in a web player with pinned comments (VC-05, VC-07) | Frame.io, Figma | ✗ | #522, #524 |
| Co-editing | Live co-editing (VC-06) | Figma | ✗ | #523 |
| Teams | Roles, approvals, SSO (VC-10 to VC-12) | Figma | ✗ | #617 to #619 |
| Library | Items as recipes, install, lock, search, apply (LB-01, LB-04, LB-06) | Blender asset browser | ◐ | `library.*` ops; #338 |
| Library | Seed sets: bodies, hair, outfits, palettes, poses, stages (LB-09 to LB-15) | VRoid presets | ◐ | `omuzaki-char/library`; #615, #616 |
| Library | Image search, team library, marketplace (LB-05, LB-07, LB-08) | Booth, Unity Asset Store | ✗ | #613, #614, #1432 |

**Beyond them**

- **A character diffs as text.** Two variants compare as a parameter diff plus a render diff; a merge takes the hair
  from one and the outfit from the other.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Merging two artists' changes to one character is impossible in every tool | **Semantic merge of characters:** merge at the level of parts and parameters with visual conflict previews | share of merges completed without manual fix |

## 19. Import, export and interop

**Benchmarks:** UniVRM, VRoid, Blender, VRChat SDK, HANA_Tool, Inochi2D, Ready Player Me (one avatar everywhere).

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Import | VRM 0.x and 1.0 (EX-12) | UniVRM | ✓ | `omuzaki-io-vrm` |
| Import | glTF and GLB | Blender | ✓ | `omuzaki-io-gltf` |
| Import | FBX via ufbx (EX-09) | Blender | ✓ | `omuzaki-io-fbx`; #520 closed |
| Import | Bring your own model: drop, map, report, perform in under 60 s (EX-12) | VSeeFace | ◐ | `io.import`, `avatar.*`; acceptance timing is a hand check |
| Import | PMX (MikuMikuDance) models, with their terms in the import report | MMD tools, Blender MMD Tools | ✗ | **new** #2591 |
| Import | Inochi2D puppets at full scope (EX-08) | Inochi Session | ◐ | `omuzaki-inochi2d`, `omuzaki-puppet`; #519 |
| Import | Live2D through a VTube Studio bridge (never the Cubism SDK) | VTube Studio | ◐ | `omuzaki-vts` |
| Export | VRM 1.0 and 0.x with the full expression set baked (EX-01) | VRoid | ◐ | `export.vrm`, `io.export`; validation `io.check_export` |
| Export | glTF and GLB (EX-02) | Blender | ✓ | `GltfExporter` |
| Export | VRChat: rank checker, PhysBones, menu, Quest variant (EX-03) | VRChat SDK | ◐ | rank checker `omuzaki-io-ops/src/vrchat_rank.rs`; #514 |
| Export | Unity, Godot, Unreal packages (EX-04) | UniVRM | ✗ | #515 |
| Export | Web embed player (EX-05) | three-vrm demos | ✗ | #516 |
| Export | PNGTuber sprite sets; sprite sheets (EX-06, EX-07) | PNGTuber Plus | ✗ | #517, #518 |
| Export | Roblox, cluster, Resonite with budget validators (EX-13 to EX-15) | platform SDKs | ✗ | #608, #610, #611 |
| Export | USD for film and studio pipelines | Omniverse, Blender | ✗ | **new** #2592 |
| Export | VRM extension fidelity: extended colliders, limits, constraints round-trip (EX-11) | UniVRM | ✓ | #1334 closed |

**Beyond them**

- **One character, every output,** each with its platform's budget checked before export and an auto-fix that
  decimates and atlases.

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Moving an avatar between platforms loses its physics, expressions and look | **A portable avatar behaviour format:** expressions, physics, life layer and look as data any runtime can play, proposed as a VRM extension | features preserved in a round trip through two other runtimes |

## 20. Plugins, connectors and scripting

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Plugins | WASM component plugins with capability scopes (PL-01) | Figma plugins | ◐ | `omuzaki-plugin` (wasmtime); #525 |
| Plugins | Custom nodes, inputs, outputs, exporters, part generators (PL-03) | Warudo | ✗ | #527 |
| Plugins | Plugin store (PL-04) | Blender extensions | ✗ | #528 |
| Scripting | Script console and macros (PL-02) | Blender | ◐ | `script.*` ops, the Script workspace; #526 |
| Connectors | Artists' AI tools as connectors (image, 3D, voice, music, DCC) with approval and provenance (CN rows) | none | ◐ | `omuzaki-connect` (mock connectors); #566 to #577 |

## 21. Performance and reliability

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Budgets | §10 budgets as gates (face-only stream ≤ 3% CPU, ≤ 1.5 ms GPU) | VSeeFace (light) | ◐ | `omuzaki-budget`; hardware timings are hand checks (QG rows #620 to #628) |
| Budgets | Face rasteriser ≤ 0.5 ms on the GPU | none | ◐ | `omuzaki-face/src/raster/gpu`; CPU fallback cost #2443 |
| Budgets | Adaptive quality that holds the frame budget beside a game | Unreal dynamic resolution | ✗ | **new** #2593 |
| Soak | 12-hour soak with no drift, leak or crash | none publish it | ✗ | QG rows |
| Robustness | Hostile-input audit of importers and ops | none | ✓ | `omuzaki-stage/tests/hostile_avatar.rs`, `op_conformance.rs` (#2444) |

## 22. Platforms

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Desktop | Linux | | ✓ | CI on Linux with lavapipe |
| Desktop | Windows: camera, Spout, raw input | VTube Studio | ◐ | `omuzaki-camera-mf`, `windows.raw_input`; hand checks |
| Desktop | macOS: camera, Syphon, event tap | VTube Studio | ◐ | `macos.event_tap`; hand checks |
| Web | A web player and runtime on WebGPU | three-vrm | ✗ | #516 |
| Phone | A web phone page for Android tracking, remote deck, preview (PF-10, PF-11) | VTube Studio phone | ✗ | #595, #596 |
| Phone | Our own iPhone app (PF-09) | iFacialMocap | ✗ | #594 (planned later) |

## 23. Accessibility, localisation, privacy and safety

| Level | Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|---|
| Accessibility | Keyboard control, screen-reader labels (AccessKit), scalable UI, reduced motion (SE-02) | Figma | ◐ | `omuzaki-ui/src/ds/a11y.rs`; QG-08 #626 |
| Accessibility | Colour-blind-safe themes | none | ✗ | spec SE-02 |
| Localisation | Six languages, pseudo-locale in CI, no hard-coded text (SE-03) | VTube Studio | ◐ | `omuzaki-l10n-api/locales/` (en, ja, ko, zh-Hans, es-419, pt-BR) |
| Localisation | Traditional Chinese (SE-09) | | ✗ | #696 |
| Privacy | Camera frames never leave the device; no telemetry by default (SE-04) | VSeeFace | ◐ | #530, #1049 |
| Safety | Consent records for voice and likeness (SE-05) | none | ◐ | #531 |
| Safety | AI disclosure labels (SE-06) | none | ◐ | #532, #1449 |
| Safety | Signed exports, optional watermark, provenance viewer (SE-07) | none | ✗ | #533 |
| Updates | Delta updates, model updates by hash, rollback, a beta channel (SE-08) | Steam | ◐ | `omuzaki-update-fs`; #534 |
| Settings | Profiles, session restore, release notes (SE-10) | OBS profiles | ◐ | `omuzaki-settings/src/profiles.rs`; #725 |

**Not invented yet**

| Problem | Direction | Measure |
|---|---|---|
| Avatars are trivially stolen and re-uploaded | **Robust avatar provenance:** a watermark in geometry and textures that survives re-export, decimation and re-texturing (research `2026-10-model-watermarking.md`) | detection rate after a battery of laundering steps, target ≥ 95% with < 1% false positives |
| Sign-language users have no VTuber path | **Sign-capable avatars:** hand and finger fidelity good enough that signers can be read through their avatar | intelligibility rated by fluent signers |

---

# F. The whole VTuber

The product-level bar from spec §2, as one checklist. Each line is a gate in §34.

| Target (v1.0) | Status | Where, or the issue |
|---|---|---|
| Idea or sheet to stream-ready character in under 30 minutes, guided | ✗ | QG-01 #620; needs §3 Explore and CB-90 |
| All 52 ARKit controls plus tongue and anime extras on every character | ✓ | §7 |
| Camera to photon ≤ 50 ms desktop, ≤ 80 ms laptop (p95) | ◐ | QG-02 #621; hand check |
| Face-only streaming at 1080p30 ≤ 3% CPU and ≤ 1.5 ms GPU | ◐ | QG-04 #622; hand check |
| 12-hour soak | ✗ | QG-05 #623 |
| Webcam path in under 2 minutes; iPhone under 5; OBS without plugins | ◐ | QG-03 #1467 |
| 100% of UI actions as MCP tools with schemas, dry run and undo | ◐ | §17 |
| VRM 1.0 and glTF in and out; VMC in and out; OSC | ◐ | §19, §9 (body over VMC open) |
| Life-layer liveliness ≥ human-driven reference; no frozen 0.5 s windows | ◐ | QG-09 #627 |
| Windows, macOS, Linux; a web player; a web phone page | ◐ | §22 |
| The core is free with no watermark | ✓ | spec §37 |

**Beyond the whole**

- **The character is made by code and performs anywhere:** one recipe drives the stream, the game export and the
  offline renders, and an agent can build, perform and edit it through the same ops a person uses.

**Not invented yet, for the whole**

| Problem | Direction | Measure |
|---|---|---|
| No virtual human yet reads as alive for a whole stream: eyes that look at you, timing that lands, a body that breathes, without a person driving every channel | **A liveliness model trained on what audiences respond to,** combining the life layer, gaze with intent, expression dynamics and whole-body inference into one controller that keeps a tracked or AI character alive | the liveliness gate (§15) plus a blind "alive" rating against human-driven references, target parity in ≥ 50% of clips |
| A person can't yet describe a character and be streaming as it in minutes | **Description to live in one session:** brief to character to calibration to output, all through ops, with the agent doing the steps and the person approving | median minutes from first sentence to live, target < 15 |

---

## Keeping this current

- The Director updates this file after chunks land: flip rows, add the issue a row now carries, and re-count the
  summary table (`grep -c` of the status marks per section).
- A thread that keeps its own deep list (the table at the top) updates that list; this file only changes its summary
  row.
- A new parity feature found by the main loop (CLAUDE.md) gets a row here and an issue.
