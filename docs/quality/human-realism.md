---
title: "Human realism: every feature for parity, then supremacy"
summary: "A living checklist of every feature a photoreal (and stylised) digital human needs, from the renderer up to the creator and MCP, against the best real-time and offline tools, with what would put Omuzaki ahead"
area: quality
---

# Human realism: every feature for parity, then supremacy

**The strategy.** Take one thing to full realism first: the eye, at macro framing, next to a photoreal close-up.
Everything that eye needs (subsurface scattering, refraction, layered specular, occlusion, depth of field, micro
detail) is the same machinery that makes skin, the face, and then the whole human real. Once one part holds up at
macro, the rest is the same work done again. The anime and toon looks are styles on top of the same system, not a
separate one.

**The benchmarks**, studied as feature lists only. None of their assets, code or textures are used.

- **Epic's MetaHuman** (Creator and the Unreal digital-human stack): Substrate materials, Burley and sub-surface
  profile skin, the MetaHuman eye shader, strand grooms, the facial rig and its control board, and wrinkle maps.
- **Unity's digital-human tooling** (the Enemies and Heretic demos): the eye shader with caustics and the occlusion
  and tear layers, skin with SSS and expression-driven wrinkles, GPU strand hair, and Ziva's machine-learned
  deformation.
- **Offline path tracers** (Cycles, Arnold, RenderMan): random-walk SSS, true refraction and caustics, multiple
  scattering in hair (Chiang, d'Eon), and accurate light transport. These are the ground truth the real-time look is
  measured against.
- **Dedicated part generators** (the random-eyes eye generator): one part treated as its own anatomical system with
  priors.
- **For the stylised path:** VRoid Studio, three-vrm/MToon and VTube Studio.

**Status:** ✓ in Omuzaki, ◐ partial, ○ missing. Each section ends with the issue that tracks it. A feature is checked
off only with evidence: a render, a number, or a test.

**Supremacy** means what would make Omuzaki better than all of these for its users: a person or an agent making a
custom character that holds up at macro, live in real time, through operations alone.

## 1. Renderer: light transport and materials

| Feature | Status | Parity (who has it) | Supremacy |
|---|---|---|---|
| Physically based GGX specular, energy-conserving | ◐ per program (`eye`, `skin`) | everyone | one shared BSDF library every program calls; multiple-scattering compensation (Kulla-Conty) |
| Layered specular: dual lobe for skin, clear coat for the tear film | ○ | UE Substrate, Unity HDRP | a layered material model (slabs) shared by skin, eye, lips and nails |
| Subsurface scattering, screen-space separable (Jimenez) or Burley | ◐ pre-integrated on d'Eon's six-Gaussian skin profile round the mesh's curvature, red-softened normals (`skin` program) | UE subsurface profile, Unity diffusion profiles | Burley diffusion with per-region profiles (lips, ears, lids), plus a validated match to random-walk ground truth |
| Transmission through thin parts (ears, nostrils, lids) with thickness maps | ○ | UE, Unity | thickness baked from the mesh automatically; backlit ears without hand maps |
| Refraction through the cornea, iris parallax | ✓ ray-refracted in `eye` | MetaHuman, Unity eye | true per-pixel refraction with a varying cornea shape (keratoconus to normal) |
| Caustics on the iris | ◐ a far-side brightening term | Unity eye shader | a caustic computed from the cornea's lens for each light |
| Shadow maps fitted to the casters | ✓ | everyone | |
| Soft shadows with contact hardening (PCSS) | ○ | UE, Unity | |
| Contact shadows (screen-space rays) for lids, lashes and nostrils | ✓ a 16-step depth march toward the shadow-casting light (`Scene::occlusion`) | UE, Unity | |
| Ambient occlusion: GTAO or SSAO, with bent normals | ◐ horizon-based SSAO from the depth (`Scene::occlusion`), no bent normals yet | UE, Unity | GTAO with bent normals for the specular occlusion of skin and eyes |
| Specular occlusion | ○ | UE, Unity | |
| Image-based lighting: HDRI, SH irradiance, prefiltered specular | ✓ | everyone | |
| Area lights (rect, disc) with LTC | ○ | UE, Unity | a softbox as a real area light, so catchlights and highlights come from geometry |
| Light linking and per-light catchlight control | ○ | UE (lighting channels) | catchlight placement as an operation |
| Order-independent or sorted transparency for lashes, hair cards and the tear film | ◐ one blended pass | UE, Unity | weighted blended OIT, or per-strand sorting for hair |
| Hair BSDF: Marschner or Chiang (R, TT, TRT), with multiple scattering | ◐ the `hair` program is stylised | UE, Unity, path tracers | a dual-scattering approximation in real time |
| Strand rendering: line or curve rasteriser, with LOD to cards | ○ (ribbons only) | UE grooms, Unity hair | |
| TAA or another temporal resolve (thin lashes and hair without shimmer) | ○ (supersampling only) | everyone | |
| Depth of field (thin lens, bokeh gather) | ✓ the `DepthOfField` pass | everyone | physical aperture and focal length on the camera |
| Tone mapping: ACES, and AgX or Khronos PBR Neutral | ◐ ACES | everyone | |
| Bloom, grain, grade | ✓ | everyone | |
| Exposure from physical camera units (EV, ISO, shutter) | ○ | UE, Unity | |
| Path-traced reference mode (offline, the same scene) | ○ | UE path tracer, Cycles | one-click ground truth beside the real-time frame, scored automatically |
| Sheen or fuzz lobe (fabric, peach fuzz) | ○ | UE Substrate, Unity fabric | |
| Two-lobe anisotropic specular with a tangent input (satin, brushed metal, hair) | ○ | UE, Unity | |
| Refraction for gems and glass | ○ | UE, Unity | |

Tracked in #2507.

## 2. Skin

| Feature | Status | Parity | Supremacy |
|---|---|---|---|
| Albedo with tone, mottling and freckles | ✓ procedural (`eye3d::skin`) | MetaHuman texture sets | fully procedural at any resolution, seeded, through operations |
| Melanin and haemoglobin colour model (spectral tone, flush) | ○ | research-grade (Jimenez, Donner) | tone from two physical sliders; flush, pallor and blush animate |
| Pores: procedural micro-normals that fade with distance | ✓ (`skin` program) | MetaHuman micro detail | regionally varying pore size and direction (nose, cheek, forehead) |
| Fine lines and wrinkles, static | ◐ albedo lines only | MetaHuman | displacement plus normal, driven by age |
| Expression wrinkle maps, blended by the rig (brow, crow's feet, nasolabial) | ○ | MetaHuman, Unity | wrinkles generated from the expression's strain, no hand maps |
| Specular: dual lobe with an oily T-zone and per-region roughness | ◐ single lobe, one roughness | UE | regional roughness and oil maps generated |
| Subsurface profile per region | ○ | UE, Unity | see the renderer section |
| Peach fuzz (vellus hair) as sheen or short strands | ○ | offline, some real-time | |
| Wet skin: tear line, lips, sweat | ◐ the lid margin's wet line | UE | sweat and tears as animated layers |
| Blood flow and flush maps driven by expression | ○ | the Heretic demo | |
| Makeup layers (foundation, blush, liner, shadow, lipstick) with their own roughness | ◐ 2D painter only (#2472) | MetaHuman | makeup as layered materials on the 3D skin |
| Detail at 4K to 8K, or procedural at any scale | ◐ 2K textures plus procedural pores | MetaHuman 8K | resolution-free procedural detail |

Tracked in #2508.

## 3. The eye (first to full realism)

| Feature | Status | Parity | Supremacy |
|---|---|---|---|
| Eyeball mesh with a separate cornea cap | ✓ | MetaHuman, Unity | |
| Refraction at the cornea, iris at depth, the pupil as a hole | ✓ | MetaHuman, Unity | |
| Iris: radial fibres at several scales, a jagged collarette, crypts, furrows, specks, limbal ring | ✓ procedural atlas | random-eyes | |
| Iris relief lit per light | ✓ (`iris-relief`) | offline | |
| Iris colour model: pigment priors, heterochromia, central heterochromia, iris freckles | ◐ five colours | random-eyes | priors, so every seed is a believable eye |
| Pupil dilation that reshapes the stroma | ✓ (radial remap) | MetaHuman | the pupil reacts to scene light and emotion |
| Sclera: vessel network, branching vessels, pinker corners | ✓ | random-eyes, MetaHuman | |
| Sclera SSS and a yellowing or redness tint (fatigue, crying) | ◐ wrap only | Unity | |
| Limbus darkening and the clear cornea edge | ✓ | MetaHuman | |
| Tear film: a clear coat with its own normal, the meniscus along the lids | ◐ the meniscus bead | Unity tear layer | |
| Occlusion where the lids meet the eye | ◐ a vertex-alpha shell plus SSAO and contact shadows | MetaHuman, Unity | a shell plus contact shadows plus GTAO |
| Caruncle and plica semilunaris | ◐ the caruncle | MetaHuman | |
| Lids with thickness, wet margins, crease and pocket | ✓ | MetaHuman | |
| Lid geometry that follows gaze (the lid rides the cornea) | ○ | MetaHuman rig | |
| Lashes as strands, with clumping, curl, strays and uneven roots | ✓ ribbons | MetaHuman, random-eyes | |
| Lashes as true strands with a hair BSDF and AO | ○ | MetaHuman grooms | |
| Catchlights from area lights | ◐ from the environment | everyone | |
| Saccades, blinks, micro-movements, vergence | ○ in 3D | random-eyes, MetaHuman | |
| The anime eye as a style on the same eyeball and lids | ○ | (no one does both) | one eye system from photoreal to anime, morphing between them |
| Every `EyeDesign` field as an operation | ○ | (no one) | an agent makes any eye through MCP |

Tracked in #2500, #2501, #2502 and #2505.

## 4. Face and head

| Feature | Status | Parity | Supremacy |
|---|---|---|---|
| Parametric head shape: sculpt handles, region blending of presets | ◐ the kit's head kinds | MetaHuman | anatomical priors so any blend is believable |
| Teeth, gums and tongue in 3D with SSS | ○ (2D painter) | MetaHuman | |
| Mouth interior: occlusion, saliva gloss | ○ | MetaHuman | |
| Lips: their own SSS profile, lines and gloss | ○ | MetaHuman | |
| Brows as strands | ○ | MetaHuman | |
| Ears with transmission | ○ | MetaHuman | |
| Nose with nostril transmission and occlusion | ○ | MetaHuman | |
| Facial rig: FACS action units, the 52 ARKit channels, corrective shapes | ◐ ARKit-52 on morphs | MetaHuman control board | correctives generated from the shape |
| LODs from the hero mesh to real-time | ○ | MetaHuman LOD0 to LOD7 | |

Tracked in #2509.

## 5. Hair

| Feature | Status | Parity | Supremacy |
|---|---|---|---|
| Strand grooms with guides, clumping, frizz and curl | ◐ kit hair generators | UE grooms | groom generators driven by operations |
| Hair cards with LOD | ◐ | MetaHuman | automatic cards from strands |
| Strand physics (GPU) | ◐ spring bones | UE Niagara, Unity hair | |
| Hair BSDF and deep shadows | ○ | everyone | |

Hair is the hair, garments and accessories owner's chunk G (#2445).

## 6. Body and clothing

| Feature | Status | Parity | Supremacy |
|---|---|---|---|
| Parametric body, skinning, corrective shapes | ◐ | MetaHuman | |
| Machine-learned deformation (muscle and skin sliding) | ○ | Ziva | |
| Cloth: garments, simulation, wrinkles | ◐ (#2095, #2121) | Marvelous, UE Chaos | |
| Nails, hands at macro quality | ○ | MetaHuman | |

Tracked in #2510.

## 7. Animation and performance

| Feature | Status | Parity | Supremacy |
|---|---|---|---|
| Live face tracking to ARKit-52 | ✓ (`omuzaki-track`) | MetaHuman Animator (offline), VTube Studio | |
| Eye tracking with saccades and vergence on the 3D eye | ○ | MetaHuman | |
| Wrinkle and flush driven by the rig | ○ | MetaHuman | |
| The life layer: breathing, blinks, micro-expressions | ◐ | | |

Tracked in #2510.

## 8. The creator and MCP

| Feature | Status | Parity | Supremacy |
|---|---|---|---|
| Every part designable in the panels | ◐ | MetaHuman Creator, VRoid | |
| Every part designable through MCP operations alone | ◐ (49 character and 16 face operations; not the 3D eye) | (no one) | an agent builds a photoreal human end to end |
| A macro preview of any part (eye, lips, skin) | ◐ the eye lab (`eye_macro`) | MetaHuman | a turntable at three framings, scored against a reference automatically |
| A reference loop with a parity score (no copied pixels) | ◐ `tools/reference-loop` | (no one) | |

Tracked in #2505 and #2509.

## Order

1. **The eye, to full realism** (this chunk and the next):
   - lashes, iris, sclera, tear film and skin round the eye (#2500 to #2503);
   - then the renderer features the eye needs: Burley SSS, a clear-coat tear film, GTAO and contact shadows, area-light catchlights, TAA (#2507).
2. **Skin around the eye, then the whole face** (#2508, #2509): the same SSS, wrinkle maps, a colour model, lips, teeth and ears.
3. **The eye and face on the character** (#2505): in the head's sockets, with three-quarter and profile views, the rig driving the lids, and the anime style on the same system.
4. **Body and motion** (#2510). Hair runs in parallel, as the hair owner's chunk G (#2445).
