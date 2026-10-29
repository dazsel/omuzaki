---
title: "The scene contract: stage, cameras, light rig, props and cast"
summary: "stage, cameras, light rig, props and cast"
area: design
---

# The scene contract: stage, cameras, light rig, props and cast

**Status:** accepted (2026-10-12); the review decisions are in §7.
**Feature:** ST-01; the contract part was pulled into phase 1 for LB-12.
**Decision record:** `docs/decisions/0024-scenes.md`.
**Spec:** v3 §7 (project and data model), §8 (runtime model), §21 (LK-04 light rigs, LK-05 post, LK-08 spot lights, fog
and image-based lighting), §24 (ST-01 to ST-12), §26 (SH-08, SH-10), §28 (LB-12, LB-14).
**Builds on:** `crates/omuzaki-stage` (the light-rig library and `SceneRecipe`, the ground disc, the frame loop
stubs), `crates/omuzaki-render-api` (`Scene`, `Light`, `Camera`, `Background`, `Post`, `MAX_LIGHTS`; shadows R2c),
`crates/omuzaki-doc` (the folder-is-the-tree rules), `crates/omuzaki-char-api` (`CharacterDef` as the pattern
for a node type, `BuiltCharacter::stature`), ADR 0014 (the stage's base pose is the bind pose), ADR 0019 and
`docs/design/library.md` (the reserved `scene` target, records, slots), ADR 0020 (agent sessions)
(one record per continuous gesture; framing and light ranges at real stature).

This page explains the decisions so that an implementer or a reviewer can disagree with a reason. The types will be
documented in the code; the page names them and gives their TOML shape, and doesn't repeat their docs.

## 1. What this covers

| Asked for | Where |
|---|---|
| The scene document: stage settings, backdrop and ground | D1, D2 |
| The light rig: key, fill and rim; directional, point and spot lights; image-based light; fog | D3, D4, D12 |
| Cameras and framing presets (face, bust, full body) at real stature | D4, D5 |
| Props and backgrounds | D2, D7 |
| Which characters are placed, and how | D6 |
| Where it lives in the project tree | D1 |
| The `stage.*` ops, one undoable edit per user intent | D8 |
| How library items target a scene (ADR 0019) | D9 |
| How `omuzaki-stage` builds render input from it, with no allocation per frame | D10 |
| A good-looking default scene for every new project | D11 |
| The ST-01 tasks confirmed or re-cut | §6 |

**Not covered here, on purpose** (each has its own feature row):
- **Scene switching** (ST-04): hotkeys, chat, timeline and transitions. The live scene and the live camera are runtime
  state of a show, not document edits. This contract gives them ids to switch between.
- **Collab** (ST-10, ST-11): the shared stage over QUIC and gaze across participants. The flat, id-keyed layout below is
  what the CRDT (§7) mirrors; nothing here assumes one machine.
- **Broadcast** (BR-*): outputs, overlays and the per-scene OBS source take a scene id and render what D10 builds.
- **Camera motion** (ST-02 runtime, SH-10): heads followed live, handheld drift and punch-ins on cue animate a camera
  this contract defines. They add fields to a camera when designed; nothing here blocks them.
- **Animated lights and cameras on a timeline** (SH-02): timeline tracks drive the same values the ops set.
- **What drives a cast member** (live tracking, the AI performer, a score): the perform session binds inputs to
  `scene/member` pairs (PF-*, ST-12). The scene only places the member.
- **Physics props and throws** (ST-03, BR-12): they add fields to props (D7).
- **Wind** stays an input of the part runtimes until a scene-level wind is designed; it would be one more
  `lighting`-like key, additive (D14).

## 2. The shape of it

```
 project tree (omuzaki-doc)                       omuzaki-stage-api (types, paths, check, pure light resolve)
 scenes/<id>/scene.toml      name, subject,  ─┐
                             camera, backdrop,│   SceneDef::from_node ──► check() ──► findings
                             ground, post     │
 scenes/<id>/lighting.toml   ambient, lights, ├──► stage.* ops (omuzaki-stage): one patch per intent, undo auto
                             exposure, fog... │      ▲ library.apply (ADR 0019): records in scenes/<id>/applied/
 scenes/<id>/cameras/<c>.toml framing, angles │      │
 scenes/<id>/cast/<m>.toml   character, place │   omuzaki-stage, on a document change (may allocate):
 scenes/<id>/props/<p>.toml  kind, params...  │     builds of the cast ──► Figure (root, height, landmarks)
 scenes/<id>/applied/<a>.toml library records ┘     resolve ──► ResolvedScene (fixed capacity, metres, render-api types)
 project.toml: default_scene = "<id>"               apply on the render thread (uploads only what changed)
                                                  every frame (never allocates):
                                                    Scene.camera, lights, background, post, outline_light,
                                                    ground and prop instances, cast instances and joints
```

## 3. Decisions

### D1. A scene is a folder of small files under `scenes/<id>/`, and the project names its default scene

A scene is the node `scenes/<id>` (`SceneId`, a slug like `CharId`). Its keys are fixed, so the store's layout rules
(`crates/omuzaki-doc/src/lib.rs`) put each in the file shown. The scene node holds only objects, so it is a folder; each
key holding a plain value is a file; each collection of objects is a folder with one file per entry. A scratch test
on the store (`omuzaki_doc::Project`, 2026-10-12) wrote exactly these files for a scene shaped like §4's; the test is not committed.

| Key | File | Holds |
|---|---|---|
| `scene` | `scenes/<id>/scene.toml` | `name`, `subject`, `camera`, `[backdrop]`, `[ground]`, `[post]` (D2) |
| `lighting` | `scenes/<id>/lighting.toml` | the light rig as values: ambient, lights, exposure, bloom, background, fog, environment (D3) |
| `cameras` | `scenes/<id>/cameras/<camera>.toml` | one camera each (D5) |
| `cast` | `scenes/<id>/cast/<member>.toml` | one placed character each (D6) |
| `props` | `scenes/<id>/props/<prop>.toml` | one prop each (D7, phase 2) |
| `weather` | `scenes/<id>/weather.toml` | the scene's weather as values, optional (ST-13: `docs/design/particles.md` D10, phase 2) |
| `applied` | `scenes/<id>/applied/<app>.toml` | library application records (ADR 0019 D5); the scene's only sibling key |

- **Why a folder, not the spec's single `scenes/<id>.toml`:** the light rig is replaced as a whole by one op and one
  library slot, cameras and cast members are edited and merged one at a time, and the library writes one record per
  application. One file per concern keeps those edits in separate files, so two people (or a person and an agent)
  editing a camera and a light never touch the same file (§7, "one entity per file"). Spec §7 is updated to this layout.
- **Collections that become empty are removed from the node**, not left as empty objects: an empty object would be
  saved as an empty file (store rule 4) and clutter the folder.
- `SceneDef` (in `omuzaki-stage-api`) is the typed view of the node, with `NODE_KEYS = ["scene", "lighting", "cameras",
  "cast", "props"]` (ST-13 adds `"weather"`) and `SIBLING_KEYS = ["applied"]`, read and written like `CharacterDef` (`from_node` refuses an
  unknown key with the nearest valid one; `to_node` leaves sibling keys alone).
- **Ids are stable.** `stage.rename_scene` changes the display name, as `character.rename` does. Ids appear in shows,
  automation rules and library records, so renaming one would be a migration.
- **The default scene** is `default_scene` in `project.toml`: the scene a project opens in, and the one
  `render.preview`, library previews and `$scene` use when none is named. When it is unset or names no scene, the first
  scene by id is used, and with no scene at all the stage renders the built-in default (D11) without writing anything.

### D2. Scene settings: the subject, the opening camera, the backdrop, the ground and the post look

```toml
# scenes/main/scene.toml
name = "Main"
subject = "main"        # the cast member framings and subject-anchored lights follow (D4)
camera = "full"         # the camera the scene opens with

[backdrop]
kind = "lighting"       # lighting | colour | transparent; phase 2: gradient | image | video | environment

[ground]
kind = "shadow"         # shadow (a shadow-catcher disc) | none
opacity = 0.25          # how dark the shadow is, 0..=1
# radius = 0.83         # subject units (D4); absent: fits the cast

[post]
bloom = true            # on or off; the strength comes with the lighting (D3)
tonemap = "aces"        # aces | none
# depth_of_field = { focus = "subject", range = 0.25, max_blur = 0.01 }
# grain = 0.04
# grade = { saturation = 1.0, contrast = 1.0 }
```

- **Backdrop** (ST-01 "image, video, 3D room, transparent"):

  | `kind` | Shows | Render input | Phase |
  |---|---|---|---|
  | `lighting` | the lighting's `background` colour, so a light rig brings its own mood | `Background::Solid` | 1 |
  | `colour` | `colour` | `Background::Solid` | 1 |
  | `transparent` | nothing: alpha for OBS and Spout (BR-01) | `Background::Transparent` | 1 |
  | `gradient` | `top` to `bottom` colours | a new `Background` variant | 2 |
  | `image`, `video` | `asset` (a store hash, provenance and licence in the store), `fit = cover \| contain` | a new `Background` variant over a texture | 2 |
  | `environment` | the lighting's environment map (D3) | with LK-08 | 2 |

  A **3D room** is not a backdrop kind: it is props and a ground (D7), so it is lit, casts shadows and is edited like
  any other prop. The ST-01 presets (desk, bedroom, cafe, night library, green screen) are library items in the
  `stages` section (LB-14) that write a backdrop, a ground and props; green screen is `colour = "#00b140"` with the
  ground off.
- **Ground:** the existing shadow-catcher disc (`GroundRecipe`, `shadow-catcher` program). Its radius is in subject
  units, so it scales with the character; absent, it covers every cast member plus 0.83 subject units.
- **Post** is what the author asks for (render-api D6). `bloom` switches the lighting's bloom on; `Quality` still
  decides what runs. Depth of field focuses on the subject's aim point unless `focus` is a distance in metres.

### D3. The lighting is values, not a reference: a light rig is copied in, and its name is kept for information

```toml
# scenes/main/lighting.toml (the studio rig, as the default scene has it)
rig = "viewer-studio"   # informational: the rig these values came from; editing a light doesn't clear it
exposure = 0.9
background = "#4a3b31"  # shown when the backdrop is `lighting`
outline_light = 1.0     # scales outline colours; lower it in dark scenes so line art doesn't glow
# skin_floor = 0.42     # optional override of every cast skin material's shadow floor (see below)

[ambient]               # the hemisphere light every rig starts from
sky = "#f1eef8"
ground = "#4a3e36"
intensity = 0.75

[bloom]
strength = 0.32
radius = 0.5
threshold = 0.92

[lights.key]
kind = "directional"
role = "key"            # key | fill | rim | accent | practical: for people, agents and LK-04 "match a reference"
colour = "#fff6ec"
intensity = 1.7
azimuth = 33.69         # degrees around the anchor: 0 = from the front, + = from screen right (front camera's view)
elevation = 46.13       # degrees above the horizon
shadow = true
anchor = "subject"      # D4

[lights.rim]
kind = "directional"
role = "rim"
colour = "#dfe6ff"
intensity = 0.8
azimuth = -138.65
elevation = 30.99
anchor = "subject"

[lights.fill]
kind = "directional"
role = "fill"
colour = "#fff0e0"
intensity = 0.45
azimuth = -50.19
elevation = 21.01
anchor = "subject"
```

- **Materialised, as ADR 0019 D5 requires.** `stage.set_light_rig` copies the rig's values into `lighting`; rendering
  reads only the scene. So a new app version that retunes a rig file never changes a saved scene, and a person can edit
  any light afterwards. `rig` is kept as information, like `skin_tone` on a character.
- **One type for rig files and scenes.** A light-rig file is a `Lighting` value plus its metadata (`name`, `title`,
  `summary`, `tags`, `version`, `source`; D12). The seven existing rig files are converted once to this format, so
  applying a rig is a plain copy and there is no second light model to keep in step. The prototype's shadow-camera
  numbers stay in git history; the renderer fits its own frustum (render-api, R2c).
- **Light kinds:**

  | `kind` | Fields | Render input | Phase |
  |---|---|---|---|
  | `directional` | `azimuth`, `elevation` | `Light::Directional` (direction toward the light) | 1 |
  | `point` | `position`, `range` | `Light::Point` (three.js falloff, render-api D10) | 1 |
  | `spot` | `position`, `target`, `cone` (outer half-angle, degrees), `penumbra` (0..=1), `range` | `Light::Spot` (LK-08) | 2 |

  Every light also has `role`, `colour`, `intensity` (three.js units, render-api D10), `shadow` (default `false`) and
  `anchor` (default `subject`). Directions are angles because that's how people and rig descriptions speak ("a low
  warm key about 10 degrees above the horizon"); the resolve turns them into vectors once (D10).
- **Shadows follow the render contract:** today only the first directional light with `shadow = true` casts (R2c), so a
  scene may mark at most one directional light. Setting `shadow` on another moves it in the same edit (D8). When LK-08
  adds spot-light shadows, the limit grows with the render contract.
- **Limits:** the ambient plus the lights stay within `MAX_LIGHTS` (8) from `omuzaki-render-api`, checked by the ops, so a
  scene never asks the renderer for more than it draws. The environment and fog are not lights.
- **Fog and image-based light** (LK-08, phase 2), so LK-04 rigs can use them:

  ```toml
  [fog]
  colour = "#2a2238"
  near = 2.0            # subject units from the camera (D4)
  far = 9.0

  [environment]
  kind = "sky"          # sky: a procedural gradient (seed-safe, no asset) | hdri: a store asset
  zenith = "#6f86b8"
  horizon = "#e8c9a0"
  ground = "#3b3026"
  # asset = "blake3:..." (hdri: a CC0 HDRI with its provenance in the store)
  intensity = 0.6
  rotation = 0.0        # degrees about the vertical axis
  toon_ramp = true      # quantise the diffuse irradiance onto the toon ramp, keeping the cel look
  ```

  First-party rigs carry no binary assets (ADR 0019 D15), so a seed rig that wants image-based light uses `sky`; HDRIs
  come from packs or the person's own files with their provenance.
- **Skin floor: the material owns it.** A character's skin materials get their shadow floor from the skin colour
  (`SkinToneParams::for_colour`), tuned per tone. The prototype tuned one floor per rig instead. Both are kept: a
  scene's `skin_floor` overrides every cast skin material while it is set, and no seed rig sets it, so seed rigs
  don't undo the per-tone tuning. The golden-manifest recipes (JS parity) pass the manifest's own floor.

### D4. Units and anchors: physical things in metres, lighting and framing in subject units

Characters are built at real stature: a 0.97 m soft-sd and a 1.71 m tall adult share a scene. A rig tuned on one
must light the other the same way, and a framing must fill the frame the same way.

- Every light and free camera has an **`anchor`**: `stage` (the stage origin, axes fixed), `subject` (the scene's
  subject member) or `member:<id>` (a named member).
- **Subject units:** with a member anchor, lengths are given as if that member were 1 m tall, in its frame (its feet at
  the origin, facing +Z, turning with its yaw). The resolve multiplies them by the member's height in metres. Point and
  spot intensities are given for that 1 m figure and scale with the height squared, which cancels the inverse-square
  falloff, so the light on the face is the same on every stature. Angles don't scale.
- **Metres:** `stage`-anchored lights and cameras, cast positions, and props (a mug is 10 cm whoever holds it).
- **No cast:** the subject is a 1 m figure at the origin facing +Z, so subject units equal metres.
- **The reference height** of the prototype's rigs is `REFERENCE_HEIGHT_M = 0.966` (soft-sd's authored stature, the
  figure they were tuned on). Converting a rig divides its local-light positions and ranges by it and its local-light
  intensities by its square; at that height the converted rig resolves to the original numbers.
- **Why not metres everywhere:** every rig and camera preset would then be tuned for one stature, and every stature
  change, template switch or second character would need re-lighting by hand.

### D5. Cameras: framing presets measured on the subject, plus free cameras

```toml
# scenes/main/cameras/full.toml
framing = "full"       # face | bust | full | hands | free
subject = "subject"    # whom a preset frames: subject | member:<id>
yaw = 30.0             # degrees around the subject: 0 = in front, + = toward screen right
pitch = 6.0            # degrees: + = from above
fov = 30.0             # vertical field of view, degrees
zoom = 1.0             # > 1 moves in from the preset's distance
shift = [0.0, 0.0]     # composition offset, fractions of the frame's width and height
# projection = "orthographic"   # sprites and pixel style (EX-07): the preset sets the view height instead
# free cameras only: anchor = "stage", position = [x, y, z], target = [x, y, z]
```

- **Landmarks** are measured on the built subject **in its bind pose** (ADR 0014), so a preset doesn't drift while the
  character animates: `top` and `bottom` (whole-character bounds, hair and hats included), `crown` and `chin` (the head
  part's bounds), `eyes` (the face layout's eye line on the head), `chest` (the upper-chest joint), `hands` (both hands'
  bounds), and the height in metres (`BuiltCharacter::stature`).
- **Presets** aim at a landmark and set the distance so a span fills a fraction of the frame height at the camera's
  `fov`; the distance is the larger of that and the distance that fits the span's width at the target's aspect ratio:

  | `framing` | Span that fills the frame | Fill | Vertical aim |
  |---|---|---|---|
  | `face` (close-up) | `chin` to `crown` | 0.75 | `eyes` at 0.45 from the top |
  | `bust` (medium) | `chest` to `crown` | 0.80 | crown 0.08 below the top edge |
  | `full` | `bottom` to `top` | 0.80 | centred |
  | `hands` | the hands' bounds | 0.70 | centred (pose previews) |

  The head fills the same share of a face shot on a chibi and on an adult, which is what SH-10 means by "the face reads
  the same across characters". The fill numbers are the targets the framing task measures; it may tune them once, on
  renders, with a maintainer's sign-off.
- **One framing function** for scene cameras, `render.preview`, library previews (`preview.framing`, ADR 0019 D12) and
  the sheet's turnaround: `omuzaki_stage::scene::frame`, so a "bust" means the same thing everywhere.
- **Free cameras** (`framing = "free"`) keep an explicit `position` and `target` in their anchor's units. To turn a
  preset into a free camera (a drag past what yaw, pitch and zoom express), the viewport sends the pose it is showing
  as `position` and `target`; the op refuses `free` without them, so no op needs a character build.
- **The active camera** in the document is the one a scene opens with (`scene.camera`). Switching cameras live is
  ST-02/ST-04 runtime state, and the viewport's own navigation is view state that never edits the document, unless the
  viewport is locked to a scene camera (then a drag is `stage.set_camera` in one transaction).

### D6. The cast: which characters stand where

```toml
# scenes/main/cast/main.toml
character = "aiko"     # characters/<id>
# variant = "close-up" # a per-use variant (CB-13)
position = [0.0, 0.0]  # metres on the floor: x toward screen right, z toward the front camera
yaw = 0.0              # degrees; 0 faces the front (+Z)
visible = true
```

- A scene places 0 to `MAX_CAST` (8) members. Member ids are slugs other than `stage` and `subject`. The same character
  may stand twice (a thumbnail with two poses); each member gets its own runtimes.
- **The subject** (`scene.subject`) is the member presets frame and subject-anchored lights follow. Unset, it is the
  first member by id. Removing the subject member moves `subject` to the next member in the same edit.
- **Placement is applied after skinning** (`Instance.transform`), so moving a member never kicks its hair or cloth: part
  runtimes run in the member's model space.
- **A member whose character is missing** (deleted, or a branch without it) is skipped by the stage and reported by
  `stage.describe_scene`, never an error that blanks the viewport.
- **Size is the character's own** (its stature and body ops). A member has no scale, so real stature holds on stage.

### D7. Props and image backdrops (phase 2)

```toml
# scenes/desk/props/mug.toml
kind = "mug"           # a registered prop kind (our generator) | "mesh" (an imported model)
params = { colour = "#d9c7f0", height = 0.1 }
# asset = "blake3:..." # kind = "mesh": the model in the store, with its provenance and licence
anchor = "stage"       # stage | subject | member:<id>
# attach = "rightHand" # a humanoid bone of the anchor member: the prop follows it (ST-03)
position = [0.25, 0.74, 0.1]   # metres in the anchor's (or bone's) frame
rotation = [0.0, 0.0, 0.0]     # yaw, pitch, roll in degrees
scale = 1.0
visible = true
casts_shadow = true
```

- **Prop kinds self-register** (`register_prop_kind!`, §39 rule 2), like part kinds: a kind turns `params` into meshes
  and materials (and, for a lamp, a practical light that counts toward `MAX_LIGHTS`). Library prop and stage items are
  recipes over these kinds, so they adapt and stay editable (ADR 0019 D1). Imported models use the `mesh` kind.
- **Rights:** every binary a scene uses (an imported prop, an image or video backdrop, an HDRI) is a store asset with
  its provenance, and library items record theirs in `applied`. The export gate for renders and clips joins both with
  the cast's character licences.
- Physics props and throws (ST-03, BR-12) add fields to a prop when designed.
- **Built (#705, #932, #1166).** `omuzaki-stage-api::props` holds the contract (`PropDef`, `PropKind`,
  `register_prop_kind!`); `omuzaki-stage::props` registers eleven interior kinds (`room`, `rug`, `desk`, `chair`,
  `cafe-table`, `bed`, `shelf`, `lamp`, `window`, `cup`, `plant`). The stage keeps each prop's build while its kind
  and params stay the same, uploads only builds the renderer lacks, draws every visible prop each frame without
  allocating, and moves an attached prop with its bone, in a cast as well. A lamp's bulb and a window's daylight join
  the rig's lights within `MAX_LIGHTS`; the extra lights are dropped in prop-id order and counted. The ops are
  `stage.add_prop`, `stage.set_prop`, `stage.remove_prop` and `stage.prop_kinds`. Stage presets
  (`library/stages/*.toml`: study, bedroom, cafe, night-library, desk, green-screen) are applied with
  `stage.set_stage_preset`, which replaces the backdrop, the ground and the props an earlier preset placed (ids
  starting `set-`). Their Library-item form waits on the scene target (#427). The `mesh` kind is not built yet.

### D8. The `stage.*` ops: one op per intent, one patch per op

Every op on a scene takes `scene` (a `SceneId`; default: the project's default scene, D1), writes only under
`scenes/<id>` (the lifecycle ops also write `project.default_scene`), has scope `edit:scene` (queries: `read`) and
`Undo::Auto`, and checks the whole resulting scene with `SceneDef::check` before returning its patch.

| Op | Does | Writes | Phase |
|---|---|---|---|
| `stage.create_scene {id, name?, cast?}` | a new scene with the default content (D11); `name` defaults to the id with a capital first letter, `cast` to the project's default character; becomes the default scene if there is none | `scenes/<id>`, maybe `project.default_scene` | 1 |
| `stage.list_scenes` | query: ids, names, cast, which is default | | 1 |
| `stage.set_default_scene {scene}` | the scene the project opens in | `project.default_scene` | 1 |
| `stage.set_light_rig {scene, rig}` | replaces the whole lighting with a rig's values (did-you-mean on a typo) | `lighting` | 1 |
| `stage.light_rigs` | query: rig names, titles and one-line summaries | | 1 |
| `stage.describe_scene {scene}` | query: the scene, its findings (missing characters, dangling anchors, limits), what each camera frames | | 2 |
| `stage.rename_scene {scene, name}` | the display name | `scene.name` | 2 |
| `stage.duplicate_scene {scene, id, name?}` | a copy without library records (they pin the original's applications) | `scenes/<id>` | 2 |
| `stage.delete_scene {scene}` | removes it; clears or moves `project.default_scene` in the same edit | `scenes/<id>`, `project.default_scene` | 2 |
| `stage.add_light {scene, id?, kind, ...}` | a new light, within `MAX_LIGHTS` | `lighting.lights.<id>` | 2 |
| `stage.set_light {scene, light, ...}` | any subset of a light's fields; `shadow = true` moves the shadow here | that light (and the old caster's `shadow`) | 2 |
| `stage.remove_light {scene, light}` | | that light | 2 |
| `stage.set_lighting {scene, exposure?, background?, outline_light?, skin_floor?, ambient?, bloom?, fog?, environment?}` | lighting-wide values; `fog = "off"` and `environment = "off"` remove them | `lighting` keys | 2 |
| `stage.add_camera {scene, id?, framing?, ...}` | a new camera (default: `bust`, front) | `cameras.<id>` | 2 |
| `stage.set_camera {scene, camera, ...}` | any subset of a camera's fields | that camera | 2 |
| `stage.remove_camera {scene, camera}` | moves `scene.camera` to another camera in the same edit if it was this one | that camera, maybe `scene.camera` | 2 |
| `stage.set_active_camera {scene, camera}` | the camera the scene opens with | `scene.camera` | 2 |
| `stage.place_character {scene, character, id?, position?, yaw?, variant?}` | a new member; without `position` it stands 0.7 m right of the rightmost member | `cast.<id>` | 2 |
| `stage.set_member {scene, member, position?, yaw?, variant?, visible?, character?}` | moves, turns, swaps or hides a member | that member | 2 |
| `stage.remove_member {scene, member}` | re-points cameras and lights anchored to it to `subject`, and moves `subject` if needed, in the same edit | that member and what pointed at it | 2 |
| `stage.set_subject {scene, member}` | | `scene.subject` | 2 |
| `stage.set_backdrop {scene, kind, ...}` | | `scene.backdrop` | 2 |
| `stage.set_ground {scene, kind?, opacity?, radius?}` | | `scene.ground` | 2 |
| `stage.set_post {scene, bloom?, tonemap?, depth_of_field?, grain?, grade?}` | | `scene.post` | 2 |
| `stage.set_weather {scene, preset, intensity?, area?}` | the preset's values copied in; `preset = "none"` removes them (ST-13, `docs/design/particles.md` D10, D11) | `weather` | 2 |
| `stage.add_prop`, `stage.set_prop`, `stage.remove_prop` | props (D7) | `props.<id>` | 2 |

**One undoable edit per user intent** comes from four rules:
1. **An intent is one op.** Every op returns one patch, and the runner makes it one undo step.
2. **Consequences ride in the same patch.** Moving the shadow, re-pointing anchors of a removed member, moving the
   default scene or the opening camera: the op writes them too and lists them in its output, so undo restores all of it
   and an agent sees what else changed.
3. **A continuous gesture is one transaction:** dragging a light, orbiting a scene camera or sliding a member sends one
   `stage.set_*` per frame inside a transaction that the runner coalesces into one record. Esc rolls it back.
4. **Patches touch only the changed leaves** (a light's `intensity`, not the whole lighting), except `set_light_rig`,
   which replaces the lighting by intent. So a person's edit and an agent's edit to different lights merge, and the
   stale-write guard of ADR 0020 (decision 5) names only real conflicts.

Agents hold `edit:scene` by default (§6). None of these ops needs a person; going live and switching scenes on air are
`broadcast` ops of ST-04 and BR-*.

### D9. Library items on a scene (ADR 0019)

- **Target:** `scene` binds `$scene` to `scenes/<id>`: the caller's `scene`, else the project's default scene (D1). A
  scene must exist; the Library panel creates the default one first, in the same transaction, when the project has none.
- **Records** go to `scenes/<id>/applied/<app>`, one file each (D1), and `applied` is the scene's only sibling key.
  Duplicating a scene leaves them out; deleting a scene deletes them with it.
- **Slots own nodes:**

  | Section (seed task) | Kind and slot | Writes | Phase |
  |---|---|---|---|
  | `light-rigs` | setting, slot `light-rig` | the whole `lighting` (`stage.set_light_rig`) | 1 |
  | `stages` (LB-14) | bundle, slot `stage` | `scene.backdrop`, `scene.ground`, the props and cameras it creates; never `lighting` | 2 |
  | `camera-moves` (LB-14) | member | the node ST-02 defines for named moves | 2 |

  A stage preset that pairs with a rig is applied together with that rig item, as two records, by the Library panel in
  one transaction; the `stage` slot never writes the lighting, so replacing a stage never undoes a rig choice.
- **Safety rule 2 holds by construction:** every `stage.*` edit op an item may call writes only under the scene it
  names. The lifecycle ops write outside it (`project.default_scene`, other scenes), so a recipe that calls them is
  refused by the confinement check.
- **Compatibility:** scene items declare no templates; any scene accepts them.
- **Previews:** a scene item's preview (`framing = "scene"`) is the neutral preview character staged as the subject of a
  scratch default scene with the item applied, rendered through `render.preview {scene}`.
- **The light-rig adapter** (ADR 0019 D14) turns each viewer and perform rig file into `omuzaki/light-rigs/<name>` with
  the recipe `stage.set_light_rig {scene: "$scene", rig: <file name>}`; the `dev-*` rigs are not items.

### D10. From document to render input: resolve on change, fill every frame without allocating

Three steps, split by when they may allocate:

1. **Resolve** (`omuzaki_stage::scene::resolve`, on a document change; may allocate): read `SceneDef`, take each visible
   member's build (the caching builder) as a `Figure` (root transform from position and yaw, height in metres, the D5
   landmarks), and produce a `ResolvedScene`: every light as a render-api `Light` in metres and world space (anchors
   and subject units applied, D4), every camera as a render-api `Camera` for the current target aspect, the background,
   `Post`, `outline_light`, the ground recipe, and the skin-floor override. Lights sit in a fixed-capacity array of
   `MAX_LIGHTS`; cameras in one of `MAX_CAMERAS` (16). The pure part (lights from a lighting and its anchor frames) is
   in `omuzaki-stage-api`, so the rig conversion test and the stage share it.
2. **Apply** (`Stage`, on the render thread between frames): diff against the current `ResolvedScene` by stable ids
   and touch the renderer only for what changed: upload or release the ground disc and prop meshes, `update_material`
   on cast skin materials when the skin-floor override changes. **A value-only change (a light's colour, a camera's
   yaw, the exposure) allocates nothing here.** A resize re-resolves the cameras for the new aspect, also without
   allocating.
3. **Fill** (`Stage::tick`, every frame): `Scene::clear()`, then copy the active camera, the lights
   (`extend_from_slice` into a `Vec` reserved at `MAX_LIGHTS` when the stage was made), background, post and
   `outline_light`; push the ground and prop instances (capacity reserved at apply); then the cast's instances and
   joints with each member's root transform. **No allocation, no lock, no document access.** The active camera's
   position also goes to each member's life layer as its gaze target (ST-11 adds others).

- **Threads:** resolve runs where the document lives and hands its result to the render thread through a one-slot
  mailbox (newest wins, spec §8), so a burst of edits during a drag never queues frames.
- **Budget:** resolve of the default scene with one member, without the build: ≤ 0.2 ms; apply of a value-only change:
  ≤ 0.05 ms; fill: ≤ 0.02 ms on the reference machine, all with a benchmark. None of them is in the 1.5 ms GPU frame.
- **Golden-manifest recipes** (`SceneRecipe`, JS parity) resolve their lighting at `REFERENCE_HEIGHT_M` with the
  manifest's camera, so the parity goldens keep their numbers.
- **The CRDT later** (ST-10, §7) mirrors the same flat, id-keyed node. Anchors are parent links, like a part's `parent`,
  so loro's movable tree maps onto them without a second model.

### D11. The default scene: the studio look, three framings, the default character

`stage.create_scene {id}` with no other arguments, and the built-in fallback of a project with no scene, both use
`omuzaki_stage::scene::default_scene(project)`:

| Part | Value | Why |
|---|---|---|
| id, name | `main`, "Main" | |
| lighting | the studio rig (`viewer-studio`): warm-white key from the upper right with the shadow, cool rim from behind left, warm low fill | the prototype viewer's neutral default; tuned on the toon look |
| backdrop | `lighting` (`#4a3b31`, warm dark brown) | the rig's own mood; a transparent backdrop is one op away |
| ground | shadow disc, opacity 0.25, radius fitted to the cast | grounds the figure without a visible floor |
| cameras | `full` (yaw 30, pitch 6, fov 30: the prototype viewer's three-quarter view), `bust` and `face` (front, fov 30); opens on `full` | full body for building, bust and face for performing; framed at any stature (D5) |
| post | bloom on, ACES | the prototype's look; live mode switches bloom off by cost (`Quality`) |
| cast | `main` = the project's default character, else its only character, else empty | |

- **Every new project gets it as a real scene:** the new-project paths (the first-run flow, a future `project.create`
  op, `omuzaki new`) run `stage.create_scene {id: "main"}` after creating the project. A project without a scene (an old
  one, or one made by `omuzaki_doc::Project::create` alone) still renders the same default, unsaved, so a viewport is
  never black. The Stage and Library panels create it in the same transaction as the first scene edit or scene-item
  apply; an op called directly on a missing scene is refused with the hint `stage.create_scene {id: "main"}`.
- **Its look is a starting point, judged on renders:** the render task attaches reference / before / after
  renders of the default scene on the mock character, and a contact sheet on the four templates at all three cameras:
  reference (the studio-rig golden), before (today's preview) and after.

### D12. Light-rig files

A rig file under `crates/omuzaki-stage/library/light-rigs/` becomes:

```toml
name = "viewer-candle"
title = "Candle"
summary = "A warm flickering key from a candle, a cool rim and a low warm fill."
tags = ["warm", "night", "practical"]
version = "1.0.0"
source = "js/viewer.js LIGHTS.candle, converted at the 0.966 m reference height"

[lighting]
exposure = 0.92
background = "#2a1810"
# ... the same keys as lighting.toml (D3), with local lights in subject units
```

- `title`, `summary`, `tags` and `version` are what the light-rig library adapter (ADR 0019 D14) puts in each
  item's manifest; `stage.light_rigs` lists them too.
- The loader keeps its table of embedded files (one line per rig) until `omuzaki-embed` can embed the folder.
- `LightRig` and its prototype-shaped structs go; `SceneRecipe` and the golden tests read the new files (D10).
- New rigs (golden hour; stage with coloured spots once LK-08 lands) are authored in this format, in angles.

### D13. Validation and errors

`SceneDef::check` is the one validator, shared by the ops, the store's reader, library planning and the stage. It
returns errors naming the path and the rule (a typo gets the nearest valid name), and findings that don't stop an edit:

| Refused | Reported as a finding |
|---|---|
| unknown keys; non-finite numbers; intensities, ranges, exposure, opacity below 0; angles outside their range; fov outside (0, 180) | a member whose character doesn't exist |
| more lights than `MAX_LIGHTS`, cameras than `MAX_CAMERAS`, members than `MAX_CAST`, props than `MAX_PROPS` (128) | a backdrop asset missing from the store |
| a second directional light with `shadow` | a prop kind not registered in this build (the prop is skipped) |
| an anchor, subject or camera naming nothing that exists; a member id `stage` or `subject` | a lighting with no light other than the ambient |

### D14. Compatibility

- All phase 2 fields (spot lights, fog, environment, props, the new backdrop kinds) are additive: optional, with
  defaults, so phase 1 scenes read unchanged and no migration is needed.
- `stage-api` types follow `omuzaki-doc`'s migrations like any other content. Renaming a field is a migration; op
  arguments that recipes use follow ADR 0019 D16 (aliases, defaults, step migrations).
- Scenes from before this contract don't exist (no scene node was ever written), so nothing needs converting.

## 4. Worked example: a two-character desk scene on disk

What `stage.create_scene {id: "desk"}`, `stage.place_character {character: "kiri"}`, `stage.set_light_rig {rig:
"perform-facecam"}`, `library.apply {item: "omuzaki/light-rigs/candle", scene: "desk"}` and `stage.set_camera {camera:
"face", fov: 50}` leave behind (five undo steps):

```
scenes/desk/scene.toml             name = "Desk", subject = "main", camera = "full", backdrop, ground, post
scenes/desk/lighting.toml          rig = "viewer-candle", the candle values (the library apply replaced the facecam rig)
scenes/desk/cameras/full.toml      framing = "full", yaw = 30, pitch = 6, fov = 30
scenes/desk/cameras/bust.toml      framing = "bust"
scenes/desk/cameras/face.toml      framing = "face", fov = 50 (a wide close-up lens, ST-02)
scenes/desk/cast/main.toml         character = "aiko", position = [0, 0]
scenes/desk/cast/kiri.toml         character = "kiri", position = [0.7, 0]
scenes/desk/applied/light-rig.toml item = "omuzaki/light-rigs/candle", version, hash, wrote = the lighting values, rights
```

The candle's point light is in subject units (`position = [1.501, 1.283, -1.190]`, `range = 7.246`,
`intensity = 5.187`). On Aiko, a 0.966 m soft-sd, it resolves to the prototype's `[1.45, 1.239, -1.15]`, 7.0 and 4.84.
Swapping the subject to Kiri, a 1.714 m tall adult, puts it at `[2.573, 2.198, -2.040]` with range 12.42 and intensity
15.24, and the light reaching her face is the same.

## 5. Crates and contract changes

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-stage-api` (new) | API | ids (`SceneId`, `MemberId`, `LightId`, `CameraId`, `PropId`); `SceneDef` and its parts (`SceneSettings`, `Backdrop`, `Ground`, `PostDef`, `Lighting`, `LightDef`, `Anchor`, `CameraDef`, `Framing`, `CastMember`, later `PropDef`); `paths`; `NODE_KEYS`, `SIBLING_KEYS`; `check` and findings; limits; the pure light resolve (D10 step 1). Depends on `omuzaki-core`, `omuzaki-doc-api`, `omuzaki-char-api` (`CharId`, `VariantId`), `omuzaki-render-api` (`Colour`, `Light`, `Bloom`, `Tonemap`, `ColourGrade`, `MAX_LIGHTS`), `omuzaki-store-api` (asset hashes) |
| `omuzaki-stage` | App | the rig library in the new format; `scene::{landmarks, frame, resolve, apply, default_scene}`; the `stage.*` ops in `ops/scene/`; `render.preview {scene?, camera?, framing?}` |

- No mock crate: the API crate holds data types and pure functions, tested on their own, and the ops are tested against
  `omuzaki-doc-mock` and the char and render mocks, as `omuzaki-char-api` does.
- **Other contracts** (each its own task): `omuzaki-render-api` gains `Light::Spot`, a fog and an environment on `Scene`
  (LK-08), and new `Background` variants (gradient, texture). `omuzaki-library-api` lifts the `scene` refusal
Spec §7 shows the scene folder (this PR).

## 6. The task cut

Phase 1 is what LB-12 needs: the types, the rig files, the light-rig op, framing, the stage wiring and the preview
path the light-rig seed renders through. Everything else is phase 2. Every task is blocked by this design.

| # | Does | Phase | Blocked by |
|---|---|---|---|
| S1 | `omuzaki-stage-api`: ids, `SceneDef` and its phase 1 parts, `paths`, keys, `check`, limits; the stage's scene skeleton (stub modules, manifest) | 1 | this design |
| S2 | `omuzaki-stage-api`: the pure light resolve (anchors, subject units, angles to vectors), allocation-free | 1 | S1 |
| S3 | the seven rig files in the lighting format; `LightRig` replaced; parity at the reference height | 1 | S1, S2 |
| S4 | landmarks and `frame` (face, bust, full, hands) at real stature; `ResolvedScene` from a scene and its figures | 1 | S1, S2 |
| S5 | the stage applies and fills a `ResolvedScene` (ground, skin floor, member root), no allocation per frame | 1 | S4, the stage frame loop |
| S6 | `stage.create_scene`, `stage.list_scenes`, `stage.set_default_scene`, `default_scene`, `scene_for` | 1 | S1, S3 |
| S7 | `stage.set_light_rig`, `stage.light_rigs` (re-cut) | 1 | S1, S3, S6 |
| S8 | the library's `scene` target (re-cut: the sibling key and paths come from S1) | 1 | S1, S7, the library's scene-target work |
| S9 | `render.preview {scene?, camera?, framing?}`; the default-scene and light-rig contact sheets | 1 | the render ops, S5, S6 |
| S10 | five seed rigs (re-cut: the coloured-spot "stage" rig moved to S24) | 1 | S3, S7, S8, S9, the library light-rig tasks |
| S11 | `stage.describe_scene`, `stage.rename_scene`, `stage.duplicate_scene`, `stage.delete_scene` | 2 | S6 |
| S12 | `stage.add_light`, `stage.set_light`, `stage.remove_light`, `stage.set_lighting` | 2 | S1, S6 |
| S13 | `stage.add_camera`, `stage.set_camera`, `stage.remove_camera`, `stage.set_active_camera` | 2 | S1, S6 |
| S14 | `stage.place_character`, `stage.set_member`, `stage.remove_member`, `stage.set_subject` | 2 | S1, S6 |
| S15 | several cast members on one stage (ST-12): an upload, runtimes and a life layer per member | 2 | S5, S14 |
| S16 | `stage.set_backdrop`, `stage.set_ground`, `stage.set_post` | 2 | S1, S6 |
| S17 | gradient and image backdrops (render-api `Background` variants, both renderers, the scene) | 2 | S16 |
| S18 | video backdrops | 2 | S17 |
| S19 | LK-08 render contract: `Light::Spot`, fog and an environment on `Scene`, the mock and conformance | 2 | this design |
| S20 | spot lights, fog and environment in the scene: types, resolve, ops, the `environment` backdrop | 2 | S4, S12, S19 |
| S21 | props: the prop-kind registry, the `props` node, `stage.add_prop`, `stage.set_prop`, `stage.remove_prop` (ST-03) | 2 | S1, S6 |
| S22 | staging props: upload, placement, bone attach, practical lights | 2 | S5, S21 |
| S23 | the licence gate for renders and clips: scene records, scene assets, the cast's licences | 2 | S8, S21, the export licence work |
| S24 | the "stage" light rig with coloured top spots | 2 | S10, S20 |
| S25 | interior prop kinds for the presets (desk, chair, shelf, bed, lamp, window, cafe table, cup, plant) | 2 | S21, S22 |
| S26 | the ST-01 presets as `stages` items: desk, bedroom, cafe, night library, green screen | 2 | S8, S16, S17, S25 |

The stage-framing question is settled by the framing task (S4); the light-range question by D4, and is tested in S2 and S3.

## 7. Decisions taken on review (2026-10-12)

1. **Subject-height units** for lights and cameras (D4): accepted. Characters and props stay in metres.
2. **The folder layout** (D1): accepted; it merges cleanly and works with branches (ADR 0025).
3. **The default scene** (D11): accepted as a starting point. The render task attaches reference / before / after renders of it
   on the mock character, and the look is judged then.
4. **The skin shadow floor** (D3) stays with the character's material; the lighting's floor is only an override.

## 8. Sources

Read on 2026-10-12: spec v3 §6, §7, §8, §11, §21, §24, §25, §26, §28; ADR 0014, ADR 0019 and
`docs/design/library.md`, ADR 0020; `docs/design/render-api.md`; `docs/design/ux/shell.md`;
`crates/omuzaki-stage` (`recipe/`, `library/light-rigs/`), `crates/omuzaki-render-api/src/scene.rs`,
`crates/omuzaki-doc/src/{lib,layout}.rs`, `crates/omuzaki-char-api/src/{character,stature}.rs`,
`crates/omuzaki-char/src/palette/skin_tone.rs`; the prototype's `js/viewer.js` (`VIEWS`, `LIGHTS`) and
`js/perform/perform.js` (close-up framing and lens).
