---
title: "Character design: MetaHuman grade and the perfect VTuber"
summary: "Every level of character design, from light transport to the creator's tools: parity with the best character tools, what would beat them, and what nobody has invented yet, with research directions and the numbers that would prove them"
area: quality
---

# Character design: MetaHuman grade, and the perfect VTuber

**The question this answers:** what features and tooling would let a person, or an agent through MCP alone, make a
MetaHuman-grade human and an absolutely perfect VTuber in Omuzaki? This page goes level by level, from how light
moves through skin up to how someone who can't draw describes the character they want. Each level has three parts:

1. **Parity:** what the best character tools do at that level, and where Omuzaki stands.
2. **Beyond:** what would make Omuzaki better than all of them, using what only Omuzaki has (characters as
   parametric recipes, one operation layer for people and agents, measurement built in, real-time first).
3. **Not invented yet:** where today's best still visibly fails. For each one there is a candidate invention, a
   research direction and the number that would tell us it worked. Subsurface scattering was once an entry in a list
   like this one.

**How it relates to the other lists.** This page sits above them and goes past them; it does not repeat their rows.

| List | What it covers | Where |
|---|---|---|
| [human-realism.md](human-realism.md) | the photoreal human's feature rows: renderer, skin, eye, face and head, body, motion | #2507 to #2510 |
| [creator-parity.md](creator-parity.md) | the stylised parts against creators: eyes, brows, face shape, mouth, skin, makeup, body | #2465 to #2477 |
| hair, garments and accessories as systems | 84 features with a done-when each, summary and order on #2445 | #2511 to #2571 |
| [master-checklist.md](master-checklist.md) | the whole product; character design is its section B | |

**The benchmarks,** studied as feature lists only. None of their assets, meshes, textures or code are used, and
MetaHuman's assets stay out entirely (Epic's licence).

- **Epic MetaHuman** (Creator and the Unreal digital-human stack): region blending, sculpt handles, Mesh to MetaHuman,
  Substrate materials, the eye shader, strand grooms with card LODs, the facial rig and control board, wrinkle maps,
  LOD0 to LOD7, MetaHuman Animator.
- **Unity digital humans** (Enemies, Heretic) and **Ziva**: the eye with caustics, occlusion and tear layers, skin with
  expression wrinkles and blood flow, GPU strand hair, machine-learned deformation.
- **Reallusion Character Creator 4**: Headshot (photo to head), SkinGen (layered skin), morph sliders by region, Smart
  Hair, expression editor, ActorCore motion, InstaLOD export.
- **Daz Studio** (Genesis 9): hundreds of named morph dials, a shared base mesh for every figure, dForce cloth and hair,
  Iray path tracing, a huge asset ecosystem built on one topology.
- **VRoid Studio**: the anime standard: face and body sliders, layered face textures, a guide-based hair editor,
  texture painting, outfit templates, VRM export with springs.
- **Blender** (sculpt, Geometry Nodes hair, Human Generator, MB-Lab) and **ZBrush**: sculpting and anatomy.
- **Marvelous Designer** (garments from patterns), **Houdini** (grooms), **Substance** (layered materials).
- **The random-eyes generator**: one part as its own anatomical system with priors, so every seed is believable.
- **Live2D Cubism and VTube Studio**: what a drawn VTuber face does that 3D usually can't.

**Status:** ✓ in Omuzaki · ◐ partly · ✗ missing, judged from the code on `main` (`56c2f70b`, 2026-10-30). A row
marked **new** was filed with this page.

---

## Level 1. Light transport and shading

How light meets a character: the renderer's shading programs. The feature-by-feature photoreal list is
[human-realism.md §1](human-realism.md#1-renderer-light-transport-and-materials) (#2507); this level adds the stylised
side and the inventions.

### Parity

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| GGX specular, image-based light, shadow maps | everyone | ✓ | `omuzaki-render` |
| A refracting eye program (cornea, iris depth, limbus) | MetaHuman, Unity eye | ✓ | `program/eye.wgsl` |
| A skin program: pre-integrated SSS, wrapped terminator, oily GGX, procedural pores | Unreal subsurface profile | ◐ | `program/skin.wgsl`; Burley diffusion and per-region profiles #2507 |
| SSAO and contact shadows | Unreal, Unity | ✓ | `Scene::occlusion` |
| Transmission through ears, nostrils, lids | Unreal, Unity | ✗ | #2507, #2509 |
| Layered specular (dual-lobe skin, clear-coat tear film) | Substrate, HDRP | ✗ | #2507 |
| Hair BSDF (Marschner R, TT, TRT) with multiple scattering | everyone photoreal | ✗ | #2522, #2523 |
| Area lights, PCSS, TAA, physical camera, a path-traced reference | Unreal | ✗ | #2507 |
| Toon shading with tinted shades, rim, MToon fidelity | MToon, UTS | ✓ | `shader/toon.wgsl` |
| Anime face shadows that fall like a drawing (SDF maps or edited normals) | UTS3, lilToon | ✗ | #2572 |
| Line art: per-part weight, taper, crease lines, stable | Pencil+ 4 | ◐ | outlines exist; #2573 |
| Cast shadows that stay inside the toon ramp | game toon pipelines | ✗ | bug #2595 |

### Beyond

- **One shading system with a style axis.** The same eye, skin and hair render anywhere from photoreal to cel, per
  part, from one material description, so one character can be a realistic close-up on Monday and an anime emote on
  Tuesday. Nobody keeps one asset across that range. **new** #2607
- **Scored against ground truth by default.** Every shading change is rendered beside a path-traced frame of the same
  scene and scored, so "more real" is a number, not an opinion (the path-traced reference is in #2507).

### Not invented yet

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Real-time skin still reads as "very good CG" at macro: the light doesn't diffuse through the tissue the way it does in a photo, especially at shadow edges and in backlight | **Real-time random-walk SSS, learned:** a small network trained on random-walk path-traced skin patches (Omuzaki renders its own training data) that predicts the diffusion per pixel from curvature, thickness and light direction | train on offline renders of the kit's skin under many lights; distil into a shader-sized network; compare against Burley and separable SSS | perceptual difference (FLIP) to the path-traced frame at macro below Burley's by half, at ≤ 1 ms at 1080p on a mid-range GPU |
| Toon shading on a 3D head reads as a 3D model, not a drawing, at three-quarter and profile | **View-dependent drawing rules:** learn per view angle how anime artists cheat a face (nose to a tick, jaw line moved, far eye squashed, mouth rotated toward camera) and apply them as live mesh and shadow deformation | study licence-clean turnaround drawings as rules, not pixels; parameterise each cheat; let an agent tune them per character | blind test with artists: the share of frames judged "drawn" rather than "3D", target ≥ 50% at three-quarter. **new** #2608 |
| Lines swim (screen space) or miss inner detail (hull) | **Stroke identity over time:** feature curves extracted from the mesh (silhouette, crease, suggestive contours) drawn as strokes that keep their identity frame to frame | curve extraction on the GPU; stroke matching across frames | per-stroke jitter < 0.5 px under a slow orbit at 1080p (part of #2573) |
| Photoreal and stylised looks need different assets and different renderers | **A continuous abstraction axis in the BSDF:** shading terms that interpolate from physical to cel (ramp quantisation, specular shape, SSS colour) without a discontinuity | define the axis per lobe; verify both ends against their references | one character scored against a photoreal and an anime reference without re-authoring (#2607) |

---

## Level 2. Materials

What a surface is made of: the layers on each part, the texture detail, the library.

### Parity

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Procedural textures from parameters, seeded, at any resolution | Substance Designer | ◐ | face painter, eye atlas, skin pores, hair strand texture; per part |
| A material layer stack on every part (base, detail, wear, wetness, makeup, paint) editable through ops | Substance Painter, CC4 SkinGen | ✗ | **new** #2609 |
| Paint on the model (base, shade, masks) | VRoid, Substance Painter | ✗ | #2575 |
| Decals that cross parts: tattoos, body paint, scars, birthmarks | CC4 SkinGen, Daz | ◐ | face marks only (`face.add_mark`); **new** #2610 |
| Fabric (weave, sheen, fuzz) | Substrate, Marvelous | ✗ | #2489, #2557 |
| Metal, gem, sheen programs | Unreal, lilToon | ✓ | `program/metal`, `gem`, `sheen` |
| Leather, plastic, glass verified on accessories | Substance | ✗ | #2566 |
| Named palette slots, colourways, a contrast checker | (none ship it) | ✓ | `palette.*`, `colourway.*` |
| Edit an imported model's materials | UniVRM, VRoid | ✗ | #2574 |
| Normal, roughness and AO maps on kit parts | everyone | ✗ | #2507, #2489 |

### Beyond

- **Materials are recipes too.** A material is ops plus parameters, so a colourway swap, a style change or an age
  change re-derives every layer instead of repainting it (#666 keeps recolouring fast).
- **Every layer has a physical meaning an agent can reason about** ("more melanin", "oilier T-zone", "worn at the
  elbows"), not an opaque texture.

### Not invented yet

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Making a material read as real takes scanned textures; procedural ones look clean and synthetic | **Procedural detail with the statistics of the real thing:** generators fitted to published, licence-clean measurements (pore density and direction by face region, fabric weave statistics), not to images | collect permissive measurement data; fit generator priors; validate with texture statistics | texture-statistic distance (Portilla-Simoncelli or a learned perceptual metric) from licence-clean photo references below a set bar, with no image used as a texture |
| Wear and ageing of materials is hand-painted | **Simulated use:** wear, creases and shine where a garment or skin actually bends and rubs in the character's motion library | accumulate strain and contact over a motion set; turn it into wear masks | rater preference for simulated over hand-painted wear |

---

## Level 3. The parts

Each part as its own system, the way the random-eyes generator treats the eye: anatomy as layers, every layer a
reusable parameterised generator with priors, every parameter an op.

### 3.1 Eyes

The first part taken to full realism (owner: the eye and face thread). Photoreal rows are in
[human-realism.md §3](human-realism.md#3-the-eye-first-to-full-realism); stylised rows in
[creator-parity.md](creator-parity.md).

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Anime eye: iris, catchlights, sclera, lids, lashes, aperture, presets, seeded random | VRoid, random-eyes | ✓ | `face.eyes.*`; #2465 to #2469 |
| 3D eye: eyeball and cornea, refraction, procedural iris and sclera atlas, lids, lash ribbons, occlusion shell | MetaHuman, Unity | ✓ | `omuzaki_face::eye3d`, `program/eye.wgsl` |
| Lashes read as real (not a comb), iris reads fibrous, tear meniscus, skin round the eye | MetaHuman, random-eyes | ◐ | #2500 to #2503 |
| Iris colour model with priors (pigment, heterochromia, freckles) | random-eyes | ◐ | five colours today |
| The 3D eye on the character's head, with ops | MetaHuman | ✗ | #2505 |
| Lids that ride the cornea as the eye turns | MetaHuman rig | ✗ | #2510 |
| Saccades, micro-movements, pupil response in 3D | random-eyes | ✗ | #2510 |
| The anime eye as a style on the same eyeball | (nobody) | ✗ | #2505, #2607 |

**Beyond.** One eye system from photoreal to anime with every field an op, so an agent can make any eye and a person
can slide one character between styles.

**Not invented yet**

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Eyes that read as alive. Even the best real-time eyes look like glass marbles in motion: the tear film doesn't move, the pupil never breathes (hippus), micro-saccades are missing, the lids don't drag skin | **The living eye:** a coupled model of tear film (thickness changing between blinks, the meniscus refilling), pupil hippus and light response, micro-saccades and drift, and lid and skin follow, all driven by the gaze and blink channels | measure each behaviour from published physiology; animate on the 3D eye; drive from tracking | blind "is this eye looking at me / alive" test against a video of a real eye at macro, target indistinguishable in ≥ 40% of trials. **new** #2611 |
| Eye contact through a screen feels off on every avatar | **Gaze that compensates the camera:** the eyes aim at the viewer's perceived position, correcting for the lens, the screen and the head's angle | psychophysics of perceived gaze on rendered eyes | rate of "it's looking at me" on a calibrated test |

### 3.2 Skin

Photoreal rows are in [human-realism.md §2](human-realism.md#2-skin) (#2508).

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Curated tone range with toon tints per tone | (none tune toon per tone) | ✓ | `palette.set_skin_tone` |
| Procedural albedo with mottling and freckles; pores that fade with distance | MetaHuman, CC4 SkinGen | ✓ | `eye3d::skin`, `program/skin.wgsl` |
| Melanin and haemoglobin colour model, flush and pallor animated | research grade | ✗ | #2508 |
| Regional SSS, roughness and oil | Unreal | ✗ | #2508 |
| Expression wrinkles, static wrinkles by age | MetaHuman | ✗ | #2508, #2615 |
| Peach fuzz | offline | ✗ | #2508 |
| Makeup as layered materials on 3D skin | MetaHuman, CC4 | ◐ | 2D makeup (#2472); 3D in #2508 |
| Tattoos, body paint, scars across parts | CC4, Daz | ✗ | #2610 |

**Not invented yet**

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Skin colour doesn't respond to emotion and exertion in real time; blushes are painted blobs | **Physiological skin:** blood flow per face region driven by emotion, exertion, temperature and pressure (a squeeze whitens, a blush spreads from the cheeks), on the melanin and haemoglobin model | regional perfusion maps from published physiology; drive from channels and the life layer | blind rating of flush timing and spread against real video; works in toon style too (a blush that animates like a drawn one) |
| Wrinkles need hand-sculpted maps per expression | **Wrinkles from strain:** generate wrinkle normals and displacement from the skin's compression under each expression, scaled by age and skin thickness | compute strain from the expression shapes; a thin-shell buckling model; fold into the skin program | wrinkles appear where artists would sculpt them (rated), with no hand maps (in #2508) |

### 3.3 Face shape and head

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Face-shape profiles (superellipse, chin, cheeks), 5 presets, feature layout | VRoid | ✓ | `face.set_profile`, `face.set_layout` |
| Blend several presets globally and per region | MetaHuman | ✗ | #2470 |
| Named regional shape dials, symmetric and asymmetric | Daz, CC4 | ✗ | **new** #2612 |
| Sculpt handles and free sculpting that survive parameter changes | MetaHuman, ZBrush | ✗ | **new** #2613 |
| 3D nose, cheekbones, jaw angle on the mesh | MetaHuman | ✗ | #2471 |
| Anatomy under the surface: skull, fat pads, muscle, skin thickness | (offline only) | ✗ | **new** #2614 |
| Traced head from a mesh or sheet silhouettes | (none) | ✓ | `omuzaki-char/src/head/trace` |
| Age as one parameter across head, skin, hair and body | CC4, Daz | ✗ | **new** #2615 |
| Face priors spanning human diversity | MetaHuman (scan database) | ✗ | **new** #2616 |
| A head from a consented photo | CC4 Headshot, Mesh to MetaHuman | ✗ | **new** #2617 |

**Beyond.** Face shape driven by anatomy, so every slider and every random seed is a believable face, and the same
anatomy drives the stylised head (an anime face is a deliberate exaggeration of a real skull, not a separate model).

**Not invented yet**

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Sliders make faces that look "off" in combination; blending presets averages faces into blandness | **An anatomical face solver:** the surface is solved from skull, fat pads and skin thickness, so changes stay plausible and blends keep character | build a layered head model from published anatomy; fit its parameters to the kit's heads; blend in anatomy space | rater "plausible face" rate for random samples, target ≥ 90%; distinctiveness of blends measured by an identity metric (#2621) |
| Nobody can describe a face precisely in words, and no tool turns words into a face reliably | **Face from description:** a language-to-parameter model over the anatomical space with an agent loop that renders, compares to the description and refines | train on synthetic pairs (parameters, generated descriptions); refine with human ratings | blind match rating ≥ 70% for first results (with #2578) |

### 3.4 Mouth, lips, teeth and tongue

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Painted mouth: lips, interior colours, teeth band, gums, fang, tongue with shape | VRoid, Live2D | ✓ | #2473 |
| Teeth, gums and tongue in 3D with SSS; lips with their own profile, lines and gloss | MetaHuman | ✗ | #2509 |
| Mouth interior occlusion and saliva gloss | MetaHuman | ✗ | #2509 |
| Lip contact and seal, sticky lips, teeth hidden correctly in speech | film rigs | ✗ | **new** #2618 |

**Not invented yet**

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Real-time mouths pass through each other, show teeth through closed lips and lose the lip seal on "m", "b", "p" | **Contact-correct lips at frame rate:** a collision and adhesion layer on the lip shapes (seal, roll, sticky peel apart) driven by the viseme channels | soft contact on the lip region; adhesion from published lip-seal studies | zero interpenetration on a speech battery; viewers rate bilabials clearer |

### 3.5 Nose and ears

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Painted nose styles (none, dot, tick, highlight) | VRoid | ✓ | face painter |
| Ears: human, elf, animal, with placement and hair interaction | VRoid | ✓ | `omuzaki-char/src/head/ears` |
| Nose and ears with transmission and occlusion in 3D | MetaHuman | ✗ | #2509 |

### 3.6 Brows, lashes and facial hair

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Brow designer drawn over the hair | VRoid | ✓ | face painter |
| Brows as strands | MetaHuman | ✗ | #2509 |
| Lashes as strands with a hair BSDF | MetaHuman | ✗ | #2500 |
| Beards, moustaches, stubble, peach fuzz | MetaHuman, CC4 | ✗ | #2475 |

### 3.7 Hair

Owned by the hair, garments and accessories thread; 34 features with a done-when each in #2511 to #2538.

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Clumps, bangs, guides, physics, colour, strands, angel ring, lock shapes, parting, volume, ten styles | VRoid, Houdini | ◐ | `hair.*` ops (with `hair.style.*`) |
| Guide brushes, traced guides, child strands, frizz and curl, flyaways, hairline | UE grooms, Houdini | ✗ | #2511 to #2517 |
| Strand rendering and cards with LOD | MetaHuman | ✗ | #2518 to #2520 |
| Marschner program, multiple scattering, melanin colour model | Unreal, Unity | ✗ | #2521 to #2524 |
| Toon hair: painted highlight shapes, two-tone ramp, fringe shadow on the face | VRoid, lilToon | ✗ | #2525, #2528 |

**Not invented yet**

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Real-time hair that looks groomed: strand hair costs too much live, cards look like cards, and neither keeps a stylist's shape in motion | **Groom-preserving hair at card cost:** strands authored and simulated offline, compressed into a learned reduced model that drives cards or a few thousand strands live with the groom's silhouette and clumping intact | train per hairstyle on offline strand simulations; reduced-space dynamics; neural appearance for the volume | ≥ 100k visible strands' look at ≤ 1 ms per character; silhouette error against the offline groom under a fast-motion battery |
| Anime hair in 3D looks wrong from some angles: the drawn shapes only work from the front | **Hair that holds its drawn silhouette from every angle:** lock shapes and the fringe deform with the view the way an animator redraws them | the view-rules work (#2608) applied to locks; artist-rated turnarounds | share of turnaround frames artists rate "on model", target ≥ 80%. **new** #2619 |

### 3.8 Body

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Four templates (Soft SD, mini, standard, tall), rigged | VRoid (one) | ✓ | `omuzaki-char/src/template` |
| Proportions and a five-axis shape space with presets | CC4, Daz | ◐ | `body.*` ops; measurement targets #2474 |
| Regional dials (shoulders, waist, hips, calves), asymmetry | Daz, CC4 | ✗ | #2474, #2612 |
| Muscle, fat and skeleton layers behind the shape | Ziva, CC4 | ✗ | #2614 |
| Machine-learned deformation and correctives | Ziva, MetaHuman | ✗ | #2510 |
| Non-human templates (animal, winged, blob, object) | (none) | ✗ | #402 |
| Body through clothing stays hidden in motion | Marvelous | ✗ | bugs #2604, #2601 |

### 3.9 Hands, feet and nails

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Five-finger, simple and mitten hands; a hand-shape library | VRoid | ✓ | `body.set_hand_style`, `hand.list_shapes` |
| Hands and nails at macro quality (knuckle creases, nail beds) | MetaHuman | ✗ | #2510 |
| Nail styles and polish as a material layer | CC4 | ✗ | #2609 |

### 3.10 Garments

Owned by the hair, garments and accessories thread; 30 features in #2539 to #2559 (2D patterns behind every
template, sewing, fit on every body, fabric physics per axis, layer collision, prints, stitches, gathers, toon ink
lines, PBR cloth, export).

**Not invented yet**

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Cloth that holds a designed shape (a stiff collar, pleats, a puffed sleeve, a crinoline) collapses or explodes under a streamer's fast motion | **Shape-holding cloth:** the designed rest shape stays the attractor (shape matching with stiffness learned per region from the pattern), on a guaranteed-stable 60 Hz integrator | constrain the solver to the authored shape; learn stiffness maps from designer intent | zero explosions in 10 h of a fast-motion battery; shape drift < 5 mm. **new** #2620 |
| Turning a drawing of an outfit into a sewable, simulatable garment needs a pattern maker | **Patterns from a drawing:** infer 2D panels and seams from front and back drawings, then fit and simulate | pattern priors from the garment library; a solver that matches the drawn silhouette | silhouette IoU against the drawing ≥ 0.9 (extends #2554) |

### 3.11 Accessories

Owned by the same thread; 20 features in #2560 to #2571 (fit and clearance, bevel and filigree profiles, combining
parts, gem and toon metal programs, chains and ribbons that swing, traced from a drawing, export).

---

## Level 4. The whole human

Parts become a person: coherence, identity, age, and the uncanny valley.

### Parity

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| One base skeleton every part fits | Daz (one topology), MetaHuman | ✓ | the VRM humanoid contract; parts refit by template |
| Every part consistent at once: age, build and colouring agree | CC4, MetaHuman | ✗ | #2615 |
| A likeness check against a reference | (none) | ✓ | `measure.likeness` |
| An identity check across expressions, styles and LODs ("still the same person") | (none) | ✗ | **new** #2621 |
| A standard lookdev sheet: turntable, framings, light set, macro of each part, expression strip | MetaHuman preview, Daz Iray scenes | ◐ | `render.turnaround`, `render.contact_sheet`; **new** #2622 |
| LOD0 to LODn | MetaHuman | ✗ | #2476 |
| A photoreal human end to end through ops | (nobody) | ✗ | #2505, #2509, #2510 |

### Beyond

- **The whole person is a recipe:** age it, change its build or move it from photoreal to anime, and every part
  follows coherently, because each part reads the same body, anatomy and palette.
- **Characters come with their evidence:** one op renders the lookdev sheet and the report card (CB-92), so a person
  or an agent sees at once whether it holds up.

### Not invented yet

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| The uncanny valley: digital humans pass in stills and fail in motion, and nobody can say which cue broke it | **An uncanny-valley detector:** a model trained on paired human ratings of real and rendered faces in motion that names the cue (eyes too still, skin too even, mouth contact, timing) | collect ratings on Omuzaki renders with controlled defects; train a per-cue predictor | the detector's ranking agrees with human ratings (ρ ≥ 0.7) and its fixes measurably raise "alive" ratings. **new** #2626 |
| Identity drifts: a character looks like a different person in another expression, style or LOD | **Identity as a measured invariant:** a licence-clean identity embedding for stylised and real faces, used as a constraint when anything changes | train on kit renders with known identities; validate on held-out characters | identity distance across expressions and styles within the bar, checked on every change (#2621) |
| Ageing a character convincingly needs a different model per age | **Coherent ageing:** one age parameter drives bone growth, fat redistribution, skin thickness and wrinkles, hair greying and posture | anatomy-based age model (#2614, #2615) | raters estimate the intended age within ±5 years |

---

## Level 5. The stylised VTuber

What makes a VTuber look perfect in the anime and toon style, which is how most VTubers stream. VRoid, Live2D and
VTube Studio are the bar here, and the drawn look is the target.

### Parity

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| A painted vector face, crisp at any zoom | Live2D | ✓ | `omuzaki-face` |
| ARKit-52 plus anime extras on every character | iPhone perfect sync | ✓ | creator-parity |
| Toon shading, outlines, MToon export | VRoid, UniVRM | ✓ | renderer, `export.vrm` |
| Five style presets (Soft SD, cel-flat, painterly, figure, pixel) | (none ship all) | ◐ | `character.set_style`; pixel #243 |
| Anime face shadows, line art an artist controls | UTS3, Pencil+ | ✗ | #2572, #2573 |
| Toon hair highlights and fringe shadows | VRoid | ✗ | #2525, #2528 |
| Drawn deformation: squash, smear frames, deformer-style exaggeration | Live2D | ✗ | covered by the inventions below |
| VRM export with perfect sync, springs and MToon | VRoid, HANA_Tool | ◐ | `export.vrm`; #2534, #2558, #2571 verify parts |

### Beyond

- **A 3D VTuber that moves like a Live2D one** (snappy, exaggerated, drawn) while keeping 3D's free camera.
- **Every VTuber is perfect sync** with no paid rigging, procedural or imported.

### Not invented yet

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| 3D VTubers look stiff next to Live2D: no squash, no smear, no deformer exaggeration on fast motion | **Drawn deformation in 3D:** a Live2D-style deformer layer on the 3D mesh (warp lattices per region, smear stretch on fast head turns, squash on blinks and laughs), driven by channel velocity | define deformers in the head's screen-aligned space; learn timing from licence-clean animation principles | blind preference against plain 3D, target ≥ 65%; no artefacts in profile |
| The face reads as a model at angles (see Level 1) | view-dependent drawing rules | #2608 | |
| Anime hair looks wrong off-front | hair that holds its drawn silhouette | #2619 | |

---

## Level 6. Rig and expressions

### Parity

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| VRM humanoid skeleton fitted to proportions; generated weights with a brush | VRoid, Blender | ✓ | `omuzaki-rig`, `rig.weights.*` |
| ARKit-52 plus tongue and anime extras, expression library with fades and layers, range test | VTube Studio, MetaHuman | ✓ | `face.expression.*` |
| Painted expressions baked to blendshapes for export | (none) | ✓ | `omuzaki-face/src/bake` |
| FACS-level 3D facial rig with correctives | MetaHuman | ✗ | #2509 |
| Expression wrinkles and flush driven by the rig | MetaHuman | ✗ | #2508, #2510 |
| A control board with 2D handles per region | MetaHuman | ✗ | #2582 |
| Spring bones, colliders, cloth, hair physics | UniVRM, Magica Cloth | ✓ | `omuzaki-rig/src/spring`, `omuzaki-char/src/cloth` |
| Spring and collider gizmos | UniVRM | ✗ | #2580 |
| Auto-rig an unrigged mesh | Mixamo, AccuRIG | ✗ | #2579 |

### Not invented yet

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Expressions are static poses blended linearly; the life of a performance (anticipation, overshoot, asymmetric timing) is missing | **Expressions as trajectories:** each expression is a short, asymmetric motion with anticipation and settle, played under tracking | fit trajectory models to licence-clean animation and consented performances | liveliness gate (§15) and blind preference over static blends ≥ 65%. **new** #2623 |
| Correctives need a rigger per character | **Correctives from the recipe:** because parts are parametric, solve correctives per template and shape from volume preservation and anatomy | per-template solve; transfer across shapes | volume loss at 120° elbow < 2%; no candy-wrapper (in #2510) |

---

## Level 7. Creation tooling

How a person, or an agent, makes the character: sculpting, drawing, the shape library, procedural generation,
randomise and explore, and authoring from text and images over MCP.

### Parity

| Feature | Best at it | Status | Where, or the issue |
|---|---|---|---|
| Every part a parametric generator with ops (121 part kinds) | (nobody at this coverage) | ✓ | `omuzaki-char`, `omuzaki-face` |
| Sliders and presets for every part | VRoid, CC4 | ✓ | ops and library items |
| Sculpting with brushes, stored as a layer that survives rebuilds | ZBrush, Blender, MetaHuman sculpt | ✗ | #2613 |
| Drawing: hair guides from a stroke, garments and accessories traced from a drawing | VRoid (hair guides) | ✗ | #2513, #2554, #2570 |
| Underlay tracing on the face | VRoid | ✓ | `face.underlay.set` |
| Paint on the model | VRoid | ✗ | #2575 |
| A shape library: whole styles and named regional shapes | Daz, VRoid presets | ◐ | ten hair styles (`hair.style.*`), face profiles, eye and makeup presets; dials #2612 |
| Priors on every parameter, so any seed is believable | random-eyes | ◐ | eyes only (`face.eyes.randomise`) |
| Explore mode: six takes, per-part randomise with locks, mix two takes | game creators, MetaHuman | ✗ | #2477 |
| Region blending of presets | MetaHuman | ✗ | #2470 |
| Create from a sheet (analysis, build, compare, iterate) | (none) | ✗ | #461 |
| Create from a text description | (none) | ✗ | #2578 |
| Create from a consented photo | CC4 Headshot, Mesh to MetaHuman | ✗ | #2617 |
| Fit proportions to a reference with a heatmap | (none) | ✓ | `fit.reference`, `fit.apply` |
| A reference loop that scores and refuses copied pixels | (none) | ◐ | `tools/reference-loop` |
| Every step undoable, with part-level variants and branches | (none) | ✓ | `variant.*`, `branch.*` |
| Plain-language critique with one-click fixes | (none) | ✗ | **new** #2624 |

### Beyond

- **An agent builds the whole character through MCP,** from a sentence, a sheet or a sketch, and a person watches
  every step and undoes any of it. No creator has an agent surface.
- **Tools that compose:** a sculpt, a painted layer and a parameter change all live on the same recipe, so changing
  the template re-fits the sculpt and the paint.
- **Quality is measured while you work:** likeness, readability and identity update live in the inspector.

### Not invented yet

| What still fails | Candidate invention | Research direction | How we'd know |
|---|---|---|---|
| Non-artists can't say what's wrong, only that it's "off", and tools can't tell them | **Critique from measurements:** readability, proportion, palette, silhouette and identity checks phrased as "the eyes sit 12% lower than this style's norm", each with a fix op | style norms measured from the kit's own presets and rated characters; a critic that ranks issues by impact | share of critiques accepted; minutes from "it's off" to "it's right" (#2624) |
| Agents author characters blind to taste: they call tools but can't judge appeal, so results are generic | **Taste as a tool:** appeal, distinctiveness and style-consistency measures, calibrated on human ratings, exposed as MCP tools the agent optimises against | collect pairwise ratings of kit characters; train a small preference model; publish it as a measure | correlation with held-out ratings ρ ≥ 0.7; blind preference for optimised characters. **new** #2625 |
| Sculpting and parametric design don't mix: a sculpt breaks when a slider moves | **Sculpts in parameter space:** a sculpt stored as a displacement in the part's own coordinates (UV and normal frame of the generator), so it re-applies after any parameter change | define stable per-part coordinates; reproject on rebuild | a sculpt survives every template and shape change in the fit suite with < 1 mm error (#2613) |
| No tool makes a character from a description that a person calls "that's her" on the first try | **Search over the kit with a judge:** an agent proposes parameters from the description and priors, renders, scores against the description with a vision model and the identity and taste measures, and keeps branches | the description, critique and taste work combined, over MCP | first-result match ≥ 70% (#2578) |

---

## Level 8. The deliverable: the perfect VTuber, measured

What "absolutely perfect" means when the character leaves the creator. Each line is a number, so it can be checked.

| Check | Target | Status | Where |
|---|---|---|---|
| Expression coverage | all 52 ARKit plus tongue plus anime extras move the face visibly; no two expressions too similar | ✓ | `face.channel.sweep`, `face.expression.range_test` |
| Readability | the face reads at 28, 56 and 112 px | ◐ | `measure.readability` |
| Clipping | no interpenetration in the stress battery (hair, garments, accessories, body) | ◐ | fit checker, #2485, #2541, #2560 |
| Physics | springs and cloth settle within a set time, stable for 12 h | ◐ | `hair.settle`; soak in the QG rows |
| Performance | the whole character inside the §10 live budget, with LODs for the rest | ◐ | #2476, #2593 |
| Export | VRM 1.0 and 0.x with perfect sync, springs, MToon, licence fields; VRChat rank | ◐ | `export.vrm`, `io.check_export`; #2534, #2558, #2571 |
| Identity | the same person across expressions, styles and LODs | ✗ | #2621 |
| Realism (photoreal) | perceptual difference to the path-traced reference at macro below the bar | ✗ | #2507 |
| Report card | all of the above on one page (CB-92) | ◐ | #463 |

---

## The order

The tooling comes before chasing scores: each system lands, then the target is rebuilt and re-scored (project rule,
2026-10-29).

1. **The eye to full realism** (eye and face thread: #2500 to #2505, #2507), then skin and face (#2508, #2509).
2. **Hair, garments and accessories as systems** (hair thread: #2511 to #2571).
3. **The shared foundations nobody owns yet**, in this order because each unblocks the next:
   #2609 (material layer stack) → #2612 and #2613 (shape tools) → #2614 and #2615 (coherent shape) →
   #2607 (one character, any style) → #2622 and #2621 (evidence and invariants).
4. **Creation from intent:** #2477 explore, #2470 blending, #2578 description, #2617, #2624, #2625.
5. **The inventions,** each as a research spike with its metric first: #2611, #2608, #2619,
   #2618, #2620, #2623, #2626.

## Keeping this current

The thread that lands a system flips its rows here and in its own deep list. A new gap found by the main loop gets a
row here and an issue. The Director re-reads this page after each character chunk lands.
