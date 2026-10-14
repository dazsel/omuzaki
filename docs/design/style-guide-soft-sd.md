# Soft SD: the default character style

**Soft SD** is the pipeline's default look: a 3D chibi rendered like a soft anime CG frame. It has real volume and warm soft lighting, plus **soft cel shading, thin coloured line art and a painted anime face**. It's halfway between a plastic figure (too glossy) and flat 2D line art (no volume), and it's exactly what a real-time 3D toon renderer produces. Every new character starts here unless the user deliberately picks something else.

- **What others call it:** chibi, or more precisely **SD (super-deformed)**. In Japanese it's described as **2-head-tall** (2頭身, *nitōshin*). As a 3D toy it's often called "Nendoroid-style", but that's a Good Smile Company trademark, so don't use it as our name or in marketing.
- **What we learned getting here:**
  - v2 had volume but read as a plastic toy.
  - v3 had line art but went flat 2D.
  - v4/v5 were Codex regenerations that never matched the sheets.
  - **The 3D test inputs are the turnarounds cropped straight from the user's sheets** (`characters\<name>\concept\sheet-views\`).

## Why this is the default

**For users: almost no decisions.** A hobbyist answers about 7 questions (below) and gets a finished, stream-ready character. They never see proportions, bone counts, shader settings or topology.

**For the pipeline: one style makes everything after it simple and reliable.**

- **One template rig instead of general auto-rigging.** Every Soft SD body has the same proportions, so we *fit a single tested template* (skeleton, weights, spring bones) rather than solving auto-rigging for arbitrary meshes. That turns the hardest and least reliable step into a mostly solved one.
- **Painted face instead of sculpted face.** Eyes, brows and mouth are texture layers on the head, as in the prototype, not sculpted geometry. This closes the "face gap": generated meshes come without expressions, but we only need a face texture set, and swapping between those is trivial to rig and drive.
- **Mitten hands by default.** A thumb plus a fused finger block. AI generators most often fail on fingers, and chibi hands read fine without them. Five-finger hands are a style option on every template (RG-09).
- **It forgives AI-generation flaws.** Simple shapes and matte shading hide the small surface errors that glossy, realistic styles expose.
- **It reads at every size.** A big head and big eyes stay readable as a 28 px Twitch emote, a pixel sprite or a phone-sized stream.
- **It's game-ready by default.** Low poly counts and one texture atlas fit VRChat's "Good"/"Excellent" PC ranks (≤32k triangles for Excellent) and mobile budgets with simple LODs.
- **Everything is shared, so it scales.** All Soft SD characters share one body template, so every emote, animation, gesture, outfit and accessory works on every character. That's what makes the marketplace work: a hoodie or a dance made for one character fits all of them.

## The fixed spec (decided for the user)

| Area | Default |
|---|---|
| Proportions | ~2–2.5 heads tall. Head ≈ 40–45% of total height. Short, soft, slightly tapered limbs. Small torso. |
| Head | Large and round, slightly wider than tall. Minimal chin. Ears small or hidden by hair. |
| Face | **Painted texture layers**: large eyes with a heavy upper lash line (iris + pupil + 1–2 soft highlights), thin brows, tiny mouth, nose omitted or a faint dot. Soft blush layer. Default resting expression is calm or understated rather than grinning. |
| Expressions | VRM presets (happy, angry, sad, relaxed, surprised, blink L/R, aa/ih/ou/ee/oh) plus extras: smug, wink, >_<, sparkle eyes, tears. Each one is a face-texture swap or blend, not a sculpted shape. |
| Hands | Mitten by default: thumb + fused fingers, 3 hand poses (open, fist, point). Five-finger hands with a hand-shape library are available on every template (RG-09). |
| Feet | Simple shoes, as one rounded shape per foot. |
| Hair | A few large clumps. 3–8 spring chains (bangs, side locks, tails), with no strand detail. |
| Shading | **Soft cel shading**: 2–3 tones per material with slightly soft edges, gentle gradients on hair, a soft rim light and a soft contact shadow under the feet. Matte, with no glossy speculars (shoes and earrings may have one small highlight). Lighting is warm and soft. |
| Line art | **Thin, soft, coloured lines**: each line is a darker shade of the colour it borders (e.g. a lavender-grey line on silver hair), never heavy pure black. Silhouette line slightly stronger, interior lines faint. Rendered live: an inverted-hull silhouette tinted per material, plus interior lines from creases marked on the mesh or painted into the texture. Thickened automatically for small outputs (emotes, sprites). |
| Mesh budget | ~15–25k triangles, one 2048² texture atlas (a 1024² mobile variant is generated). |
| Skeleton | Minimal VRM humanoid + spring bones for hair and skirt. Same template for every character. |
| Lighting presets | Studio (default), Cozy room, Night stream. |

## The ~7 decisions a user makes

1. **Hair:** style (from presets or a prompt) and colour
2. **Eyes:** shape preset and colour
3. **Skin tone**
4. **Outfit:** from a prompt, a photo or marketplace parts
5. **Accessories:** up to two (glasses, hat, ears, horns…)
6. **Personality:** a short description, which seeds the AI persona and default expressions
7. **Voice:** pick a preset or a consented clone

Everything else uses the defaults above. Pro users can unlock any setting, including the proportions slider (the chibi variant generator in reverse) for a full-height version of the same character. The 5–7-head and 7–8-head templates (CB-01) are separate tested rigs, not just a slider setting.

## How the pipeline uses it

1. **Character sheet first.** The 7 answers produce a **character sheet** like the user's references: a hero pose, a front/side/back turnaround, an expression row and a few action poses. It's the first thing the user sees and approves, and each part feeds a later step:
   - the turnaround → 3D
   - the expression row → the face texture set
   - the poses → default emotes and animations
2. **3D:** a clean, high-res A-pose turnaround of the approved design (plain background, soft even light) goes to image → mesh (Meshy, or a local model like TripoSG). Soft SD's soft shading gives the generator real depth cues. Then **fit the Soft SD template** instead of auto-rigging from scratch. **Watch for:** long hair or wide sleeves overlapping the arms in the front view, which tend to fuse in the mesh. Ask for hair behind the shoulders, or rely on the side and back views.
3. **Face:** generate the painted face texture set from the concept's face (eyes, brows and mouth as separate layers) and map it onto the template's face UVs.
4. **Hair and skirt:** detect clumps → attach the template's spring chains.
5. **Check:** the Rig Doctor runs the standard pose and expression battery. Every Soft SD character is tested against the same set.
6. **Export:** VRM 1.0, game packages, emotes and sprites, which all work immediately because the body template is shared.

## Risks

- **Sameness:** if every character looks alike, there's nothing to differentiate. Mitigate with hair, outfit, accessories and face art, which is where Soft SD characters actually differ.
- **Trade dress:** stay clearly distinct from specific commercial figure lines (Nendoroid, Cu-poche and others). Don't copy their face templates, base shapes or packaging look.
- **Not everyone wants chibi:** Soft SD is the default, not a limit. Full-height anime (the CB-01 templates) and flat 2D (the cel-flat style, and Inochi2D import, EX-08) are first-class paths. Live2D Cubism is not used (`docs/decisions/0010-live2d-and-inochi2d.md`).
