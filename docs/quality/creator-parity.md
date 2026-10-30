# Character creator parity: every part, at the best creators' level

The bar: every part of a character can be made at the level of the best character creators, in Omuzaki's anime and
toon style (with a realistic option where it makes sense), by a person in the panels or by an agent through ops alone.
If a part can't be reached through MCP, a feature is missing.

**Benchmarks, studied as feature lists only** (none of their assets, meshes, textures or code are used; MetaHuman's
assets are tied to Epic's licence and stay out entirely):

- **MetaHuman Creator** (now inside Unreal Engine): preset blending globally and per face region, sculpt handles, a
  parametric body, skin tone, texture, freckles and makeup, eyes (iris pattern, colours, limbal ring, sclera), eyelashes,
  teeth, strand grooms with card LODs, clothing, a full facial rig with a control board, LOD0 to LOD7, lighting and
  animation previews.
- **Unity's digital-human tooling** (the digital-human package and the strand hair system behind its real-time demos,
  and Ziva's face and deformation tech): a refractive eye shader with caustics and an occlusion and tear layer, skin with
  subsurface scattering and expression-driven wrinkle maps, GPU-simulated strand hair, machine-learned deformation.
- **VRoid Studio**: dozens of face and body sliders, layered face textures (eye white, iris, highlight, lash line,
  lashes, brows), a guide-based hair editor, texture painting, outfit templates, the expression editor, VRM export.
- **A dedicated eye generator** (random-eyes): one part treated as its own system. An anatomical eye (cornea, chamber,
  limbus), an iris built from radial fibres, a toothed collarette, crypts and furrows, a pigment colour model with
  heterochromia and iris freckles, anatomical lids, ~180 upper and ~90 lower strand lashes with curl and clumping, a
  sclera with vessels and a caruncle, pupil reflex, saccades and blinks, and every parameter sampled from priors so any
  seed is a believable eye.

## The matrix

✓ at or past the bar · ◐ partly there · ✗ missing. "Owner" says which line of work carries the row.

| Part | Best of the benchmarks | Omuzaki today | | Issues |
|---|---|---|---|---|
| **Eyes: iris** | fibres, collarette, crypts, limbal ring, two-colour pigment model, heterochromia | gradient, lit crescent, ring, pupil shapes, per-eye colours; seeded fibres, collarette, flecks, limbal ring, pupil glow | ✓ | #2465 |
| **Eyes: highlights** | highlight texture layer with position and size; real light rig reflections; caustic glow | blob, star, heart, window, ring and crescent catchlights, up to 8 extra, shimmer, view motion, caustic glow | ✓ | #2466 |
| **Eyes: sclera, lids** | tint, vessels, caruncle, tear meniscus, lid margin thickness, crease | three-stop white, corner shade, vessels, caruncle, wet waterline, lid shadow, crease, lower lid line | ✓ | #2467 |
| **Eyes: lashes** | strand lashes (count, length, curl, lift, clumping), styles | lash band with flick, plus upper and lower strand lashes (count, length, curl, lift, clumping, randomness) | ✓ | #2468 |
| **Eyes: aperture** | anatomical lids (fissure width, canthal tilt, lid heights, peaks) | roundness, tilt, corner shapes, lid heights | ✓ | |
| **Eyes: presets, random** | priors for every knob; presets; blend | 8 presets (`face.eyes.preset`) and a seeded random eye with locks (`face.eyes.randomise`) | ✓ | #2469 |
| **Eyes: motion** | pupil reflex, saccades, micro-movements, lid follow, blink rate | gaze, vergence, pupil channel, blink, anime extras; saccades in the performer | ◐ | |
| **Brows** | brow grooms; brow shape sliders and texture | brow designer, drawn through hair | ✓ | |
| **Face shape** | blend 3 presets globally or per region; sculpt handles; dozens of sliders | outline profile (superellipse, chin, cheeks), 5 presets blended one at a time, feature layout | ◐ | #2470 |
| **Nose, cheekbones, jaw (3D)** | sculpted regions | painted nose only; the head mesh has none | ✗ | #2471 |
| **Mouth, teeth, tongue** | teeth variation, length, spacing, bite, gums; mouth interior | painted mouth with lips, interior colours, teeth band, fang, tongue | ◐ | #2473 |
| **Ears** | sculpted regions | human, elf, animal ears with placement and hair interaction | ✓ | |
| **Skin** | tone picker, texture by age, freckles, SSS, wrinkle maps | curated tone range with toon tints per tone; marks (freckles, beauty marks) | ◐ | #1986 |
| **Makeup** | foundation, eyeshadow, liner, blush, lipstick | blush only | ✗ | #2472 |
| **Hair** | strand grooms and cards, colour model, ombre, highlights | clumps, bangs, guides, physics, gradient, streaks, strand texture, angel ring | ◐ | hair line of work (#2445 and its issues) |
| **Facial hair** | beards, moustaches, stubble, peach fuzz | none | ✗ | #2475 |
| **Body** | parametric body from measurements; many regional sliders | 4 templates, 5-axis shape space, proportions, hands | ◐ | #2474 |
| **Clothing** | presets, colours | parametric garment generators, trims, fit checker, cloth | ✓ | garments line of work |
| **Accessories** | (few) | jewellery, hats, glasses, wings, tails, horns, halos | ✓ | accessories line of work |
| **Rig, facial controls** | full facial rig, control board, correctives | VRM humanoid, ARKit-52 plus anime extras, springs, cloth, weights tools | ◐ | |
| **LODs** | LOD0 to LOD7, hair strands to cards | build quality only | ✗ | #2476 |
| **Start fast** | presets and blending; one-click random eye | starter characters; no random or explore | ✗ | #2477 |
| **Through MCP** | (no agent surface) | every op above is an MCP tool | ✓ | |

## The plan, in chunks

1. **Eyes to the generator's bar** (#2465, #2466, #2467, #2468, #2469; done in the first chunk): iris detail, catchlight layers, sclera and lid
   detail, strand lashes, eye presets and a seeded randomise. New fields default to zero, so the prototype parity tests
   keep their pixels; the kit's presets turn them on.
2. **Makeup and skin detail** (#2472, then #1986's deep tones), **teeth and tongue** (#2473).
3. **Face shape**: n-way, per-region blending (#2470) and the 3D nose, cheekbone and jaw (#2471).
4. **Start fast**: `character.explore` and per-part randomise (#2477), built on every part's priors from chunks 1 to 3.
5. **Body measurements** (#2474), **facial hair** (#2475) and **LODs** (#2476).

After each chunk the main-loop target (#2445) is rebuilt through ops and re-scored, and this matrix is updated.
