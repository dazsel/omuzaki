---
title: "Particles and reactions: particle systems, the particle pass, weather and bursts"
summary: "particle systems, the particle pass, weather and bursts"
area: design
---

# Particles and reactions: particle systems, the particle pass, weather and bursts

**Status:** accepted (2026-10-13; the review decisions are in §6). It changes `omuzaki-render-api` and
`omuzaki-stage-api`.
**Feature:** ST-13; BR-12 item throws reuse it for hit effects.
**Decision record:** `docs/decisions/0028-particles.md`.
**Spec:** v3 §8 (runtime model, scheduler, determinism), §10 (budgets), §21 (LK-01, LK-02, LK-08), §24 (ST-13), §25
(BR-01, BR-10 to BR-12, BR-14), §26 (SH-01, SH-05), §31 (SE-02, SE-04), §39.
**Builds on:** `docs/design/render-api.md` (D1 retained resources and an immediate scene, D2 the fixed frame graph, D5,
D6, D7 frame-exact offline, D12 conformance in the mock); the lighting work (LK-08: `Light::Spot`, `Scene::fog`, environments);
the `Transparent` pass; `docs/design/scenes.md` (D1 the scene folder, D3 materialised values, D4 anchors and
subject units, D8 ops, D10 resolve, apply and fill); `docs/design/automation.md` (D7 live actions and
`op.run`, D9 `ChatMessage` and `EmoteSpan`); `docs/design/library.md` (D1 items are recipes, D14 seed data);
ADR 0009 (determinism); ADR 0026 (settings).

This page explains the decisions so that an implementer or a reviewer can disagree with a reason. It gives the Rust
shapes and the file formats; the code will document each field.

## In one minute

A particle is **not simulated**. It is a pure function of the batch it was born in, its index in that batch and the
scene time: born at `start + i × interval`, it flies a closed-form path (gravity, linear drag toward a wind, a sideways
flutter), changes size along a curve and is gone after its lifetime. So the renderer keeps no particle state. Each
frame the stage lists the **spawn batches** that still have live particles on the `Scene` (a burst is one batch,
steady weather one batch per segment), and the GPU evaluates every particle in the vertex shader.

Statelessness is what keeps offline renders frame-exact (render-api D7). It also lets any frame render on its own, so
scrubbing, chunked renders and two targets per frame all work. It needs no compute shaders, so WebGL2 and lavapipe run
the same shader, and particles survive a lost device.

Particles draw in a new ninth pass, `Particles`, between `Outlines` and `Transparent`, as alpha-to-coverage sprites
that write depth. Nothing is sorted, the result doesn't depend on draw order, and blended instances drawn after them
still layer correctly. Weather is scene state (`scenes/<id>/weather.toml`, `stage.set_weather`, undoable). Bursts and a
live weather override are live actions (`stage.effect.burst`, `stage.effect.weather`, scope `perform`, never in undo)
that the automation graph calls through `op.run`. The presets are data files in `omuzaki-stage`. The proposed budget is
`gpu.particles` ≤ 0.2 ms p95 on `R` for 5,000 live particles at 1080p. A live cap of 5,000 enforces it, and a density
knob that the scheduler turns down first backs it up.

```text
 triggers                                 omuzaki-stage, render thread                       renderer (any)
 ────────                                 ──────────────────────────                       ──────────────
 automation graph (op.run) ─┐
 agent, MCP, CLI, hotkey ───┼─► stage.effect.burst ──► EffectService queue (lock-free, 64)
                            │   stage.effect.weather        │ drained once per frame
 BR-12 hit (in process) ────┘   stage.effect.clear          ▼
                                                  resolve anchors (bones, props, camera)
 scenes/<id>/weather.toml ── resolve on change ─► EmissionLog: ring of ≤ 1,024 batches,
                                                  live cap, prune, pre-warm, clear fade
                                                            │ fill, no allocation
                                                            ▼
 once: create_particle_system(desc) ──► ParticleSystemId    Scene.particles: [SpawnBatch] ──► Pass::Particles:
       (sprite + ParticleParams)                                                            the vertex shader
                                                                                            evaluates each particle
                                                                                            (= particles::evaluate),
                                                                                            alpha-to-coverage,
                                                                                            depth written
```

## 1. What this covers

| Asked for | Where |
|---|---|
| The contract: types uploaded once, per-frame state on `Scene` | D1, D2 |
| Where particles draw: inside `Transparent` or a new pass, and why | D4 |
| Simulation: GPU, the WebGL2 path, determinism, offline frame-exactness | D1, D3, D6, D7 |
| The `gpu.particles` budget row, the live cap, how the scheduler degrades particles first | D8 |
| Look: toon-consistent shading, outlines, premultiplied alpha (BR-01) | D5 |
| The preset format and where presets live | D9 |
| Weather as scene state against bursts as transient performance events | D10 |
| The ops, their scopes, and how the automation graph calls them | D11, D12 |
| Chat emote images: source, caching, never stored | D13 |
| VRM and export implications; takes and collab | D14 |
| Failure modes | D15 |
| The ST-13 tasks re-cut | §5 |

**Not covered, on purpose:**
- **Particles that react after birth**: collisions with the character or each other, turbulence fields, attractors.
  No spec row needs them. D1 says how a stepped variant would arrive.
- **Particle shadows**, cast or received (D5).
- **Person-made named presets** beyond per-burst overrides and the scene's own weather (§6, Q3: not now).
- **The SH-01 take recorder.** This design defines the events it records (D14).
- **Unicode emoji as emote pictures** (§6, Q4: not in ST-13).

## 2. Decisions

### D1. Particles are stateless: each is a pure function of its batch, its index and the time

Particle `g` of a batch is born at `t_g = start + (g − first) × interval`. At scene time `t` its age is `a = t − t_g`.
Every random choice about it is `particle_random(seed, g, stream)`. It is drawn while `0 ≤ a < lifetime` and while
the density test keeps it. Position, size, colour and orientation follow from D3's closed forms. Nothing about a
particle is stored anywhere, on the CPU or the GPU.

Why:
- **Render-api D7 holds unchanged.** Offline output depends on the resources, the scene and the quality, never on
  earlier frames. A stepped simulation is history by definition. A frame-exact render of frame 9,000 would then need
  9,000 steps, or a warm-up window with a fixed step, and live and offline renders would differ in step size.
- **Any frame on its own.** Timeline scrubbing (SH-02), chunked parallel offline renders (SH-05), and thumbnails and
  previews at a chosen time all need it. So does one renderer drawing two targets per frame: the viewport and the
  stream output render the same `Scene`, and a stateful simulation would step twice or need state per target.
- **One code path on every backend.** The evaluation is a vertex-shader function, so there is no compute pass. The
  WebGL2 profile (no compute and no vertex-stage storage buffers) and lavapipe run the same shader. The mock runs
  the same function in Rust (`particles::evaluate`), one definition shared like `Fog::amount`.
- **Deterministic by construction.** Randomness is an integer hash of `(seed, index, stream)`, which is bit-identical
  on the CPU and the GPU. There are no atomics, free lists or slot allocation whose order depends on thread scheduling.
- **Cheap where the budget is tight.** CPU work is per batch (at most 1,024 records), not per particle. That matters
  because `cpu.total` leaves about 3 ms per frame once the webcam tracker has run (§10). GPU memory holds no particle
  buffers.
- **Device loss and the watchdog (BR-23).** A new renderer re-creates its systems from the stage's descriptions, and
  every particle reappears where it was, because the batches live in the stage.
- **Enough for every look asked for.** Confetti, emote rain, rain, snow, petals and hit stars are all ballistic.
  Gravity, drag toward a wind, flutter, spin, tumble and size curves all have closed forms (D3).

**Cost:** a particle can't react to the world after it is born. If a future look needs that, a `Motion::Stepped` variant
of `ParticleParams` can add a fixed-step compute simulation behind the same `ParticleSystemDesc`, kept deterministic by
re-simulating each batch's lifetime window. No spec row asks for it, so it is not built.

**Rejected:**
- **A stateful GPU simulation** (a particle buffer, a free list, an indirect draw). It breaks D7 and orders slots
  nondeterministically through atomics. WebGL2 would need a second path, and the state is lost with the device.
- **A CPU simulation that uploads instances.** It costs CPU time and upload bandwidth per particle inside `cpu.total`,
  and it is still history.
- **Per-frame emitter state on `Scene`, with the renderer keeping the particles.** The
  renderer would hold hidden state, which is the retained scene graph that render-api D1 rejected.

### D2. The contract: particle systems are resources, spawn batches are scene values

`omuzaki-render-api` gains `particles.rs`. Parameter types serialise and have JSON Schemas; run-time types hold handles
and don't (render-api D5):

```rust
handle!(ParticleSystemId);   // handle.rs; with ResourceRef::ParticleSystem and ResourceStats::particle_systems

/// How a system's particles look and move. Created once, referenced by handle.
pub struct ParticleSystemDesc {
    pub sprite: Sprite,
    pub params: ParticleParams,
}
pub enum Sprite {
    /// Drawn by the shader from a signed distance: crisp at any size, no texture, no asset.
    Shape(SpriteShape),
    /// Cells of a texture atlas, row-major from the top left (chat emotes, a sprite sheet). The texture is the
    /// caller's; releasing it while a system reads it is `InUse`.
    Atlas { texture: TextureId, columns: u16, rows: u16 },
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum SpriteShape {
    Square,
    Rect { aspect: f32 },        // width / height, 0.1..=10
    Disc,
    Petal,                       // a pointed ellipse
    Star { points: u8 },         // 4..=8
    Heart,
    Streak,                      // a capsule along the velocity; needs `Orientation::Velocity`
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParticleParams {
    pub lifetime: Span,              // s; 0 < min <= max <= MAX_PARTICLE_LIFETIME_S (20)
    pub speed: Span,                 // m/s along the emission direction; >= 0
    pub direction: [f32; 3],         // the emission axis in emitter space; normalised on create
    pub spread_deg: f32,             // cone half-angle, 0..=180 (180: every direction)
    pub gravity: [f32; 3],           // m/s², world space
    pub drag: f32,                   // 1/s, >= 0: velocity relaxes toward `wind` at this rate
    pub wind: [f32; 3],              // m/s, world space; only felt through drag
    pub flutter: Option<Flutter>,    // a horizontal sway: amplitude_m (0..=1), frequency_hz (0..=10)
    pub size: Span,                  // m: a sprite's height (a streak's width); > 0
    pub size_curve: [f32; 4],        // size multiplier at life 0, 1/3, 2/3 and 1, piecewise linear, >= 0
    pub orientation: Orientation,    // Billboard { spin_deg } | Velocity { stretch } | Tumble { rate_deg }
    pub colours: ColourSet,          // 1..=8 colours, one per particle; for an atlas, a multiplier (white: as is)
    pub coverage: f32,               // 0..=1: the share of samples a particle covers (D4); rain uses about 0.55
    pub shading: ParticleShading,    // Unlit | Toon { shade: Colour, shift: f32, softness: f32 } (D5)
    pub edge: Option<Edge>,          // shapes only: a line at the rim, shade (0..=1 of the colour), width_px (0.5..=4)
    pub glow: f32,                   // >= 0: the colour is multiplied by 1 + glow, so bloom picks it up
    pub floor: Option<f32>,          // world height below which a particle is not drawn (rain stops at the floor)
}
pub struct Span { pub min: f32, pub max: f32 }           // serialised as [min, max]
pub struct ColourSet { colours: [Colour; 8], len: u8 }   // serialised as a list of 1 to 8 colours; Copy

/// Particles born together: a burst, or one segment of a steady stream.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SpawnBatch {
    pub system: ParticleSystemId,
    pub emitter: Mat4,               // emitter-to-world, fixed for the batch
    pub shape: EmitterShape,         // Point | Sphere { radius } | Box { half: Vec3 } | Rect { half: Vec2 } (local XZ)
    pub velocity: Vec3,              // m/s added to every particle: the anchor's own motion
    pub start: Seconds,              // scene time at which particle `first` was born
    pub interval: f32,               // s between births; 0 = all at `start` (a burst)
    pub first: u32,                  // index of the batch's first particle; earlier ones have been dropped
    pub count: u32,                  // particles `first .. first + count`
    pub seed: u32,
    pub cells: CellPick,             // atlas sprites: All | List { cells: [u16; 8], len: u8 }; ignored for shapes
    pub colours: Option<ColourSet>,  // replaces the system's colours (a burst in the subject's palette)
    pub scale: f32,                  // size multiplier, > 0; a clear shrinks it to nothing (D12)
}
```

On the existing types:
- `Scene::particles: Vec<SpawnBatch>`. It is cleared by `Scene::clear`, which keeps its capacity, so a warm frame
  allocates nothing (the `hot_loop` test grows a case).
- `Quality::particle_density: f32`, in `0..=1`, `#[serde(default)]` 1.0. It is the share of particles drawn, and the
  scheduler's first knob (D8). All three presets use 1.0.
- `Pass::Particles` (D4). `FrameReport::particles: u32` is the number of particle slots submitted (the sum of the
  drawn batches' `count`), so tests and the scheduler can see the load. It stays `Copy`.
- `Renderer::create_particle_system(&ParticleSystemDesc) -> Result<ParticleSystemId, RenderError>` and
  `update_particle_system(id, &desc)`. Their defaults return `Unsupported`, as `create_environment` does, so a
  renderer without particles still compiles.
- `limits`: `MAX_PARTICLES = 20_000` (slots per scene), `MAX_SPAWN_BATCHES = 1_024`, `MAX_PARTICLE_LIFETIME_S = 20.0`,
  and `MAX_ATLAS_CELLS = 4_096`. Every renderer guarantees them, including the WebGL2 profile.
- The reference functions (D3): `particle_random`, `evaluate -> Option<ParticleState>`, and the constants of `Stream`.
  The mock draws with them, and the GPU shader is tested against them.

**Lifetimes.**
- **Created once.** The stage uploads each preset's system on its first use, and the scene weather's system when it
  applies the scene (scenes D10). Uploads happen between frames, never in the fill.
- **Updated in place, for tuning only.** `update_particle_system` changes any field, sprite included, and live
  particles take the new values at once. The stage uses it only for a person's live edit of the same preset's
  values, never to switch weather (below).
- **Released like any resource.** The handle dies at once, and a scene that still names it fails at `render` with
  `UnknownHandle`. The atlas texture is checked on release (`InUse`), as a material's textures are.
- **Small.** A system is about 256 bytes of uniforms. A particle has no memory at all.

**Validation.** `create_particle_system` refuses the following with `Invalid` and the rule broken: a non-finite number,
a span with `min > max`, a value out of the ranges above, a `Streak` without `Orientation::Velocity`, an empty
`ColourSet`, or an atlas with zero cells or more than `MAX_ATLAS_CELLS`. A dead texture is `UnknownHandle`.
`Scene::validate` refuses more than `MAX_SPAWN_BATCHES` batches or a total `count` above `MAX_PARTICLES`
(`LimitExceeded`), a non-finite `emitter` or `velocity`, a negative `interval`, a `scale` that isn't above 0, negative
shape sizes, and `first + count` overflowing `u32`. At `render`, a batch naming a dead system is `UnknownHandle`, and a
cell outside its system's atlas is `Invalid`.

### D3. Motion, life and randomness: one reference definition

Everything below is `omuzaki_render_api::particles::evaluate`. The WGSL port is tested against it (D6), and the mock
calls it directly. Notation: `g` is the particle's global index, `b` its batch, `P` the system's params, `t` the
scene time, and `u_k = particle_random(b.seed, g, k)` a uniform number in `[0, 1)`.

- **Age.** `a = (t − b.start) − (g − b.first) × b.interval`. The subtraction `t − b.start` is done in `f64` on the
  CPU and handed over as an `f32`, so a long stream never loses precision. A two-hour stream's 7,200 s would lose
  0.5 ms in an `f32` subtraction on the GPU.
- **Lifetime** `l = lerp(P.lifetime.min, P.lifetime.max, u_LIFE)`. The particle is drawn only while `0 ≤ a < l`,
  `u_KEEP < Quality::particle_density`, and, when `P.floor` is set, `position.y ≥ P.floor`.
- **Birth point** `p0 = b.emitter × s`. `Point` is the origin. `Sphere r` is uniform in the ball:
  `r × cbrt(u_POS0) × dir(u_POS1, u_POS2)`. `Box h` is `((2u_POS0 − 1) h.x, (2u_POS1 − 1) h.y, (2u_POS2 − 1) h.z)`.
  `Rect h` is `((2u_POS0 − 1) h.x, 0, (2u_POS1 − 1) h.y)`.
- **Direction.** `axis = normalize(mat3(b.emitter) × P.direction)`. The angle off the axis is uniform in solid angle:
  `cos θ = 1 − u_DIR0 × (1 − cos spread)`, `φ = 2π u_DIR1`, in the basis `onb(axis)`. Here
  `dir(u0, u1) = (r cos 2πu1, r sin 2πu1, z)` with `z = 1 − 2u0` and `r = sqrt(max(0, 1 − z²))`. `onb(n)` is the
  branchless basis of Duff et al. 2017: `s = n.z >= 0 ? 1 : −1`, `a = −1 / (s + n.z)`, `b = n.x n.y a`,
  `t1 = (1 + s n.x² a, s b, −s n.x)`, `t2 = (b, s + n.y² a, −n.y)`.
- **Initial velocity** `v0 = dir × lerp(P.speed, u_SPEED) + b.velocity`.
- **Path.** With `k = P.drag`, `w = P.wind` and `G = P.gravity`:
  - `k > 0`: the terminal velocity is `u = w + G / k`, so `p = p0 + u a + (v0 − u)(1 − e^(−k a)) / k` and
    `v = u + (v0 − u) e^(−k a)`.
  - `k = 0`: `p = p0 + v0 a + G a² / 2` and `v = v0 + G a`. The wind has no effect without drag.
- **Flutter.** `p += A sin(2π f a + 2π u_PHASE) × (cos 2π u_SIDE, 0, sin 2π u_SIDE)`.
- **Size.** `size = lerp(P.size, u_SIZE) × curve(a / l) × b.scale`. `curve` is piecewise linear through
  `(0, c0), (1/3, c1), (2/3, c2), (1, c3)`. Particles pop in and shrink away with it, because a coverage particle
  can't fade (D4).
- **Colour.** `colours[min(floor(u_COLOUR × n), n − 1)]`, where `colours` is `b.colours` if set, else `P.colours`.
- **Orientation.**
  - `Billboard`: the quad faces the camera, rolled by `2π u_ROLL + ω a`, with `ω = lerp(spin_deg, u_SPIN)` in
    radians.
  - `Velocity`: the quad's long side follows the screen projection of `v`. Its length is `size + stretch × |v|` and
    its width is `size`.
  - `Tumble`: a world-space quad that flips like paper. The spin axis is `n_t = dir(u_AX0, u_AX1)`, with
    `(t1, t2) = onb(n_t)`. The angle is `θ = 2π u_ROLL + ω a`. The edges are `e1 = t1 × size × aspect` and
    `e2 = (cos θ t2 + sin θ n_t) × size`, and the normal is `cos θ n_t − sin θ t2`.
- **Atlas cell.** `List`: `cells[min(floor(u_CELL × len), len − 1)]`. `All`: `floor(u_CELL × columns × rows)`.
- **Randomness.** The function is part of the contract, because changing it changes every golden:
  ```text
  pcg(x)  = { s = x * 747796405 + 2891336453;  w = ((s >> ((s >> 28) + 4)) ^ s) * 277803737;  (w >> 22) ^ w }
  particle_random(seed, g, k) = (pcg(pcg(pcg(g) ^ seed) ^ (k * 0x9E3779B9)) >> 8) as f32 / 2^24
  ```
  This is the PCG hash of Jarzynski and Olano, "Hash Functions for GPU Rendering" (JCGT, 2020). It is an algorithm
  restated here, so no code is copied. All arithmetic wraps on `u32`. Every value is exactly representable, so the
  CPU and the GPU agree bit for bit.
- **Streams** (`Stream`, fixed numbers): `LIFE` 0, `KEEP` 1, `POS0` 2, `POS1` 3, `POS2` 4, `DIR0` 5, `DIR1` 6,
  `SPEED` 7, `PHASE` 8, `SIDE` 9, `SIZE` 10, `COLOUR` 11, `ROLL` 12, `SPIN` 13, `AX0` 14, `AX1` 15, `CELL` 16. New
  streams take new numbers, and the numbers never move.

### D4. Drawing: a ninth pass, `Particles`, with coverage instead of sorting

**Where.** `Pass::Particles` runs between `Outlines` and `Transparent`. `Pass::ALL` becomes `PaintTextures, Shadows,
Opaque, Outlines, Particles, Transparent, Bloom, DepthOfField, Resolve`.
- **After `Opaque` and `Outlines`**, so particles depth-test against the characters, the props and their lines.
- **Before `Transparent`.** Particles write depth (below), so blended instances drawn in `Transparent` depth-test
  against them. Examples are the transparent hair tips and lenses common in imported VRMs, the shadow-catcher ground
  and glass props. A translucent lens in front of confetti tints it, and confetti in front of the lens hides it,
  both right without sorting.
- **Before `Bloom` and `DepthOfField`.** Glowing particles bloom, and particles have depth, so depth of field blurs
  them like everything else.

**Coverage, not blending.** Every particle draws with alpha-to-coverage into the multisampled scene colour, with depth
test and depth write on. A particle's alpha (shape × texel × `coverage`) decides how many of a pixel's samples it
covers. Each covered sample gets an opaque colour, and the hardware resolve averages them. Alpha-to-coverage masks
samples but keeps the shader's alpha in the ones it lets through, so the alpha channel is written by a second, cheap
draw (below). The result doesn't depend
on draw order, so there is no per-particle sort and no per-batch sort, and the output is deterministic. The cost is
that translucency has `samples + 1` levels: five at the live preset's 4 samples. At `samples = 1`, which only a person
choosing it gets, coverage becomes a 0.5 alpha test. That is why particles fade by size, not alpha (D3). Every preset
is opaque or nearly, with soft edges. Rain at `coverage` 0.55 reads as translucent streaks.

**The near fade (#1171).** A particle's coverage is also scaled by a smoothstep of its centre's eye-space depth over
`NEAR_FADE_M` (0.15 m to 0.6 m; `omuzaki_render_api::particles`): none nearer, all of it beyond. Weather falls around
the camera too (its auto area includes the camera), and on renders a snowflake a few centimetres from the lens covered a
fifth of the frame. A burst at a bust shot's subject is a metre or more away and never fades. The check is
`particle_near_fade`.

**Why a new pass, not a place inside `Transparent`.**
1. The scheduler must see particle cost on its own to degrade particles first (D8). The pass is the frame report's unit.
2. The budget row needs its own measured line, as face paint has.
3. Inside `Transparent` means sorted blending, with the problems listed under "Rejected" below.

**What the change costs.** `Pass::COUNT` goes from 8 to 9. `PassTimes` grows by one slot and stays `Copy`. Every
exhaustive match on `Pass` gains an arm: `omuzaki-render`'s `pass/mod.rs` build list and the mock. `omuzaki-render`'s
`PassSet(u8)` (`frame/draws.rs`, "eight passes fit a byte") becomes `u16`. The scheduler isn't built yet.
Render-api D2 stands: the graph is still fixed and linear, with one more named pass.

**Rejected:**
- **Particles inside `Transparent`, sorted per particle with the blended instances.** That needs a sort every frame:
  on the GPU it needs compute, which WebGL2 lacks, and on the CPU it needs positions, which means a CPU simulation.
- **Blended particles sorted per batch.** A weather volume is in front of and behind the character at once, so no
  single depth places it.
- **Blended particles after `Transparent`, unsorted.** They would draw wrongly over translucent hair they are behind.
- **Weighted blended order-independent transparency.** It needs two more targets, and its alpha is approximate, which
  breaks premultiplied transparent output (BR-01).
- **Additive blending.** It adds colour with no alpha, so it disappears over a transparent background in straight-alpha
  consumers (PNG, the green-screen fallback). `glow` through bloom gives sparkles instead (D5).

### D5. Look: toon-consistent sprites with premultiplied output

- **Shading.** `Unlit` draws the colour as it is, like `Shading::Unlit` on the backdrop; it suits emotes, rain and
  sparkles. `Toon { shade, shift, softness }` runs the surfaces' toon model (`toon_colour` in `shader/toon.wgsl`,
  `shade.rs` in the mock), so a lit particle has the same terminator and tinted shade (LK-01: never grey) as the
  character. It has no rim, no skin treatment and no shadow lookup. It is evaluated once per particle, at its centre:
  - **Lights:** the hemisphere, the directional, point and spot lights with render-api D10's falloff and cone,
    and the environment's diffuse irradiance with its `toon_ramp` rule when `Scene::environment` is set.
  - **Normal:** a billboard or velocity quad faces the camera. A tumbling quad uses its own normal, flipped toward the
    camera (double-sided, as the toon shader does), so confetti glints as it flips.
- **Fog.** `Fog::amount` at the particle's view depth blends its lit colour, and its edge line, toward the fog
  colour in linear light before exposure. That is the same rule as surfaces, so rain thins into the fog.
- **Outlines (LK-02).** Particles have no hull outlines: a 15-pixel confetti square with an inverted-hull line becomes
  noise. A shape sprite may have an `edge` instead, a line inside its signed-distance rim, `width_px` wide. Its colour
  is the particle's colour × `shade`: a darker shade of its own colour, LK-02's rule, never black unless asked. It is
  scaled by `Scene::outline_light`. Atlas sprites have no edge.
- **Glow.** The colour is multiplied by `1 + glow` after lighting, in linear light. The scene colour is RGBA16F, so
  values above bloom's threshold glow (hit stars).
- **Compositing tests.** The renderer's tests render a particle over a transparent target and read back the resolved
  pixels: at coverage 0.5 with four samples the pixel is `(c/2, 1/2)`, at coverage 1 it is `(c, 1)`, and at coverage 0
  the target is unchanged. They repeat it over a grey target, with the particle at the far plane (depth 1), and with a
  translucent lens in front of and behind the particle, on lavapipe and the mock. Perspective cases use cameras with
  `w < 1` and `w > 1` at the particle (a far plane at 0.5 m, and the default camera): a zero-coverage particle at the far
  plane leaves the target unchanged, and a particle beyond the far plane draws nothing and never occludes nearer
  geometry.
- **Tiny particles don't shimmer.** A quad smaller than 1.5 pixels is drawn at 1.5 pixels, with its coverage scaled by
  the area ratio, so far rain doesn't crawl as it crosses sample positions.
- **Premultiplied alpha, no fringes (BR-01).** Alpha comes only from coverage. Covered samples hold an opaque colour
  and alpha 1 (written by the alpha draw, D6), so the resolved pixel is premultiplied by construction. A pixel half covered over a transparent background resolves
  to `(c/2, 1/2)`, which is correct in both encodings. Atlas texels are premultiplied in the texture and sampled by the
  existing texture convention (divide after filtering), so emote edges have neither dark nor white halos.
- **Shadows.** Particles neither cast nor receive them. Thousands of small casters would cost a shadow-map pass and
  flicker, and snow darkening under a character is not worth a lookup per particle. Rejected for now, not deferred:
  no spec row asks for it.

### D6. On the GPU (`omuzaki-render`)

- **Data.**
  - Each frame the renderer packs `Scene::particles`, grouped by system, into 16 KiB uniform blocks (WebGL2's
    minimum block size) of about 100 batch records each, bound with dynamic offsets. A record (at most 160 bytes)
    holds the emitter matrix, the shape, the velocity, `age0 = t − start` (computed in `f64` on the CPU), the
    interval, `first`, `count`, the seed, the cells, the colour override and the scale.
  - A system's params are one 256-byte uniform block, written at create and update.
  - Nothing is per particle.
- **Draw.**
  - One instanced draw per block: `draw(0..4, 0..slots)`, a quad as a triangle strip. A frame with weather and a few
    bursts across three systems is three to five draws.
  - The vertex shader finds its batch by binary search over the block's prefix sums (at most 7 steps), evaluates D3
    for the particle's centre, and builds the corner. An unborn, dead, culled or thinned particle becomes a degenerate
    triangle.
  - The fragment shader computes the shape's signed distance (anti-aliased over one pixel with `fwidth`), the edge
    line, the atlas texel, the coverage, then the colour with fog and glow.
  - **Two draws per block, because wgpu's alpha-to-coverage keeps the shader alpha.** With alpha 0.5 covering two of
    four samples, one draw would store `(c, 0.5)` in those samples and resolve to `(c/2, 1/4)`, not `(c/2, 1/2)`.
    So each block is drawn twice with the same vertex shader and the position marked `@invariant`:
    1. **Colour draw:** alpha-to-coverage on, depth test `LessEqual`, depth write on, colour writes `RED|GREEN|BLUE`.
       The fragment shader outputs the lit colour and the coverage alpha.
    2. **Alpha draw:** alpha-to-coverage off, depth test `Equal`, no depth write, colour writes `ALPHA` only. Its
       fragment shader outputs 1 and nothing else. It passes in the samples the colour draw covered and kept, so
       those samples reach alpha 1 and the resolve gives `(c/2, 1/2)`. A sample a nearer particle later takes is
       overwritten by that particle's own two draws.
    **The cleared depth must never match.** An untouched sample holds the cleared depth 1, and a zero-coverage particle
    writes no depth, so a particle at depth 1 would pass `Equal` there and write alpha 1 over nothing (opaque black).
    The vertex shader therefore clamps each vertex in clip space with `z = min(z, (1 − 2⁻²⁰) × w)`, so the depth
    buffer's `z/w` is at most `1 − 2⁻²⁰` whatever `w` is, in a perspective or an orthographic camera (a clamp of `z`
    alone fails when `w < 1`: at `z = w = 0.5` the depth is still 1, and for `w > 1` it pulls a far particle in front
    of nearer geometry). Far-plane clipping is kept by the particle's centre test: a particle whose centre has
    `z > w` becomes a degenerate triangle (like an unborn one, D6), so the clamp never pulls anything from beyond
    the far plane into view. The other matches are harmless: where the depth buffer holds opaque geometry or an
    earlier particle at the same depth, alpha is already 1. This keeps the scene depth format (`Depth32Float`, which depth of field reads) unchanged, where a
    stencil mark would force a depth-stencil target through every pass.
    This uses only core WebGL2, WebGPU and lavapipe features (no `sample_mask`, no dual-source blending). The alpha
    draw costs vertex work and a constant fragment cost; the `gpu.particles` row (D8) measures both.
  - Pipelines are keyed by sprite kind and orientation, in a colour and an alpha variant, and both are created
    with the same vertex stage. Depth write is on in the colour draw only.
- **No compute, no vertex-stage storage buffers.** That puts the `Native`, `WebGpu` and `WebGl2` capability profiles
  and lavapipe on the same shader. Instancing, alpha-to-coverage and vertex-stage uniform blocks all exist in
  WebGL2. The WebGL2 path is not a second implementation, only a profile the conformance suite runs (§5).
- **No allocation.** Buffers are sized at creation for `MAX_SPAWN_BATCHES` and never grow. Packing goes through a
  persistent staging array. The renderer's own code allocates nothing per frame (`tests/hot_loop.rs` grows a case),
  while wgpu's internal allocations stay as render-api documents.
- **The parity test.** A test-only compute shader includes the same WGSL `particle_state` and writes the positions of
  10,000 particles across all shapes and orientations. They must match `particles::evaluate` within 1e-4 m plus 1e-5
  of the distance from the origin. The test runs on lavapipe and on the RTX 4090; compute is used in tests only.

### D7. Determinism, offline renders and goldens

- **Same machine, same build: identical** (ADR 0009 rule 1). The hash and the index arithmetic are exact. The batches
  come from the stage's deterministic log (D12). Two renders of one frame on one device are identical bytes.
- **CPU against GPU: within tolerance** (ADR 0009 rule 2). `exp`, `sin`, `cos` and `cbrt` differ in the last bits
  between Rust and WGSL, and alpha-to-coverage sample patterns are the implementation's own. So the mock and a GPU are
  compared by the conformance checks' measurements (centroids, coverage sums), never pixel for pixel. GPU goldens on
  lavapipe state their tolerance per scene.
- **Live and offline use the same formulas.** Offline always draws at density 1 with its own sample count.
- **Births don't depend on the frame rate.** They fall at exact multiples of `interval` from `start`. A burst's
  `start` is the frame time it spawned at, or its recorded time on replay (D14).
- **Seeds.** A burst's seed is the caller's, or else the effect service's next draw: a hash of the session or take
  seed and a burst counter, through `omuzaki_core::rng`. The op returns the seed, and the effect event records it (D14).
  Weather streams are seeded from a hash of the scene id and the stream key. A segment that changes intensity keeps
  the seed and continues the index, so the same flakes keep falling.
- **Pre-warm.** Weather that starts with a scene opens its segment `lifetime.max` in the past. The first frame, a
  thumbnail or a library preview therefore shows full weather, not an empty sky filling up, and the same every time.

### D8. Budget, cap and the scheduler

**The row for §10** (added to the spec with this design; accepted, §6):

| Key | Item | Budget | Tier | Statistic | Gate |
|---|---|---|---|---|---|
| `gpu.particles` | The `Particles` pass, live preset, the particle budget scene | ≤ 0.2 ms | `R`, `U` | p95 | per-pass GPU timestamps on `Dev`, converted to each tier; nightly, and a same-session A/B on render PRs |

- **It is on top of `gpu.frame`, like face paint.** `gpu.frame` sums every pass except `PaintTextures` and
  `Particles`, so the §10 budget scene, which has no particles, reads the same as before. A frame with effects may
  take 1.5 + 0.2 ms.
- **The particle budget scene** is the §10 budget scene plus 5,000 live particles held steady: snow at the rate that
  keeps 3,500 alive, and a confetti fountain at 300 per second with a 5 s lifetime (1,500 alive). It runs at 1080p30
  with the live preset, paced and warm, as every row is read.
- **Why 0.2 ms is about right.** Particle cost is fill, not geometry. At the face-and-upper-body framing, a 1.5 cm
  confetti square is about 15 pixels on a side, so 5,000 particles shade about 1.25 Mpx, about 0.6 of a 1080p screen
  of one cheap fragment each. The R1 frame (130k triangles plus a full-screen resolve) measured 0.084 ms on the 4090
  and estimates at 1.4 ms on `R` (render-api, post-effects table). By the same model, a 0.01 to 0.015 ms pass on the
  4090 estimates at 0.15 to 0.25 ms on `R`. The `gpu.particles` measurement checks it. If the number doesn't hold, it proposes one with
  the evidence, and the maintainers decide, because a budget is a spec change.
- **The live cap.** The stage holds the live sum of `count` (slots, an upper bound on live particles) to 5,000
  (`EffectLimits::live()`). Weather may take at most 70 % of it (3,500), so reactions always have room. Offline
  renders take the contract's 20,000 (`EffectLimits::offline()`). A burst that doesn't fit is thinned to what does,
  and says so (D12). The cap is what makes the budget hold at density 1.
- **The density knob.** `Quality::particle_density` thins by `u_KEEP < density`. The same particles survive at every
  step, so stepping down removes some and never restarts an effect.
- **The scheduler's first rung**. When the `Particles` pass's smoothed p95 is above 0.2 ms, or the frame is
  over budget while particles run, it steps density 1 → 0.5 → 0.25. It steps back up with hysteresis (2 s below 60 %
  of budget). Like every step, it tells the user ("Effects thinned to half to hold the frame rate"). Particles go
  first because they decorate the scene, while the character's look is the product. The rest of the ladder is
  unchanged.
- **CPU.** Stage fill ≤ 0.02 ms and renderer packing ≤ 0.03 ms for 1,024 batches on `R`, each with a benchmark (the
  E-core rule of §10).
- **Memory.** About 200 KiB of batch uniforms at most. The chat-emote atlas is 1024 × 1024 RGBA8 with mips, about
  5.6 MB, and only exists once a chat emote has been shown (D13).
- **Other tiers.** `U` must pass with no step-down. `F` may miss while the scheduler is stepping density down and
  saying so.

### D9. Presets are typed data in `omuzaki-stage`, not library items

- **One file each,** `crates/omuzaki-stage/library/effects/<name>.toml`, embedded as the light rigs are (one line per
  file until `omuzaki-embed` can embed the folder, scenes D12). A file holds:
  - metadata: `name`, `title`, `summary`, `tags`, `version` and `origin`;
  - `kind`: `burst` or `weather`;
  - that kind's block. `[burst]` has `count`, `at` (D12) and `colours = "palette" | "preset"`. `[weather]` has
    `density` (births per m² per second at intensity 1), `height` (subject units above the floor where particles
    appear) and `area`;
  - `sprite` (`SpriteDef`: a shape, or `atlas = "chat-emotes"`);
  - `[particles]`: the `ParticleParams` fields.

  §3 shows a whole file.
- **Six first-party presets:** `confetti`, `emote-rain`, `rain`, `snow` and `petals` (ST-13), and `hit-stars` (BR-12).
  - Their values are designed from physical targets, as library D15 asks of seed data. `origin` states the target, for
    example "paper squares 12 to 18 mm, terminal speed 1 m/s".
  - They carry no binary assets: shapes are procedural, and the emote atlas fills at run time.
  - Each passes a flash rule before it ships, so no preset makes a large area flash more than three times a second
    (WCAG 2.3.1; SE-02).
- **Why not library items.**
  - An item is a recipe of document edits applied to a target (library D1). A burst edits nothing, so it can't be one.
  - Weather is reached from the library anyway: a `stages` item (LB-14) may call `stage.set_weather` in its
    recipe ("the cafe in the rain").
  - A new library section would change the library API for no gain.
- **Palette colours.** With `colours = "palette"`, at spawn the stage takes the anchor member's palette slots (LK-03:
  `hair_mid`, `outfit_a`, `outfit_b`, `trim`, `eyes`) plus white as the batch's colour override. Confetti then matches
  the character. Without a member (a stage or camera anchor), the preset's own colours apply.
- **Checked.** The loader refuses unknown keys with the nearest name, out-of-range values and a `weather` preset
  without a `floor`. A test loads every file.

### D10. Weather is scene state; bursts and the live weather are performance events

- **Weather is part of the scene document.**
  - It is a new key, `weather`, in `SceneDef::NODE_KEYS` (scenes D1), stored in `scenes/<id>/weather.toml`, and
    optional: absent means no weather.
  - Its values are **materialised**: the preset's values are copied in, and `preset` is kept as information, like the
    lighting's `rig` (scenes D3). A retuned preset never changes a saved scene, and a person can edit any value.
  - It is edited by `stage.set_weather` (one patch, undoable, scenes D8).
- **The area.** With `area = "auto"`, the stage computes the birth rectangle at resolve: the floor bounds of the cast
  members and the active camera's position, grown by one subject unit, at `height` subject units above the floor.
  The rate is `density × area × intensity`, held under the weather share of the live cap. Rain therefore fills a
  close-up and a full shot alike, without spawning over the whole stage. An explicit `area = { anchor, min, max }`
  fixes the volume instead.
- **Bursts and the live weather override are performance, not document edits.**
  - They are live actions (automation D7): edit kind, an empty patch, `Undo::None`. They are logged and
    announced, never enter an undo history and never bump the document version.
  - The override (`stage.effect.weather`) replaces the scene's weather until `for_s` ends, until `"scene"` is asked
    for, or until the scene is switched (ST-04). Saving the project never saves it.
  - This mirrors the scene contract's rule that the live scene and the live camera are runtime state of a show. A
    chat command that makes it snow for a minute is not an edit to the person's scene.
- **Changes cross-fade by themselves.** A weather change opens a new segment, and the particles already born finish
  their lives. `"none"` stops births and the sky empties as they land.

### D11. The ops

| Op | Scope and kind | Arguments | Does |
|---|---|---|---|
| `stage.set_weather` | `edit:scene`, edit, `Undo::Auto` | `scene?`, `preset` or `"none"`, `intensity?` (0..=1, default 1), `area?` | copies the preset's values into `weather`, or removes it; one patch; did-you-mean on the preset |
| `stage.effect.list` | `read`, query | `kind?` | the presets: name, title, summary, kind, default count |
| `stage.effect.burst` | `perform`, live action | `preset`, `at?` (D12), `count?` (1..=1,000), `colours?` (1 to 8), `scale?` (0.25..=4), `emotes?` (up to 8 `ChatEmoteRef`, D13), `only_with_emotes?` (spawn nothing when `emotes` is empty), `seed?` | queues a burst for the next frame; returns `{preset, count, seed}`; any thinning is reported in the effect event and the status |
| `stage.effect.weather` | `perform`, live action | `preset`, `"none"` or `"scene"`; `intensity?`; `for_s?` | the live override (D10) |
| `stage.effect.clear` | `perform`, live action | `fade_s?` (0..=5, default 0.3) | shrinks every live particle away and ends the override; BR-14's panic button calls it |
| `stage.effect.status` | `read`, query | | live slots and the cap, the weather's source (scene, override, none), the density the scheduler set, pending bursts, whether the renderer draws particles |

- **Text.** Every op has `op-` messages for its summary and errors (`docs/design/l10n-ops.md`).
- **Who may call them.** An automation session's base grants are `read` and `perform` (automation D7), so a show can
  burst confetti or switch the live weather without an approval. `stage.set_weather` needs `edit:scene`, which a show
  only holds if the person approved it (automation D11). That is the second reason chat-driven weather uses the live
  override.
- **The graph needs no new nodes.** Hotkeys, chat commands, redeems and timers reach these ops through `op.run`. For example:
  `{ kind = "op.run", params = { op = "stage.effect.burst", args = { preset = "confetti", at = { from = "subject",
  point = "chest" } } } }`.
- **Flood control has three layers.** The automation session's limits (1,200 edits a minute, bursts of 200) come
  first. The effect queue holds 64 pending requests and refuses more with `Limited`. The live cap thins what's left.

### D12. From requests to batches: the effect service, anchors and the emission log

- **The service.** `EffectService` is a trait in `omuzaki-stage-api`, `Send + Sync`, injected as an ops-api service:
  ```rust
  pub trait EffectService: Send + Sync {
      fn presets(&self) -> &[PresetInfo];                                        // list, did-you-mean
      fn burst(&self, request: &BurstRequest) -> Result<BurstAccepted, EffectError>;
      fn live_weather(&self, request: &LiveWeather) -> Result<(), EffectError>;
      fn clear(&self, fade_s: f32) -> Result<(), EffectError>;
      fn status(&self) -> EffectStatus;                                          // the latest published snapshot
      fn replay(&self, event: &EffectEvent) -> Result<(), EffectError>;          // takes (D14)
  }
  ```
  - `omuzaki-stage` implements it with a bounded lock-free queue into the render thread that never blocks, so a live
    action may call it from `run`. `status` returns an atomically swapped snapshot.
  - Requests are resolved to fixed-size `Copy` records before they are queued. Emote URLs become 64-bit keys (D13), so
    the render thread never allocates or frees a string.
  - A dry run validates and queues nothing.
  - `EffectError` has `UnknownPreset { name, nearest }`, `Limited`, `Invalid` and `Unsupported`, mapped onto ops
    error codes.
- **Anchors.** `EffectAt { from, point?, offset }`, in TOML `{ from = "member:kiri", point = "rightHand", offset =
  [0, 0.05, 0] }`:

  | `from` | `point` |
  |---|---|
  | `subject`, `member:<id>` | a VRM humanoid bone (`head`, `chest`, `rightHand`, ...) or `top` (just above the head) |
  | `prop:<id>` | the prop's origin |
  | `camera` | `top` (a strip just above the frame's top edge at the subject's distance) or `centre` |
  | `stage` | the stage origin |
  | `world` | the world origin; BR-12 gives a hit point this way, as an offset in metres |

  Offsets are in subject units for members and in metres otherwise, the scenes D4 rule. Bones are read from this
  frame's pose, not the bind pose, so confetti leaves the hand where it is now. The anchor's velocity comes from its
  last two frames. A member or prop that is gone falls back to the subject, with a finding in the status.
- **Each frame,** on the render thread, before the scene fill:
  1. drain the queue (at most 64);
  2. resolve anchors and palette colours;
  3. pin the emote cells (D13);
  4. append bursts and extend or prune streams in the `EmissionLog`;
  5. `fill` copies the live batches into `Scene::particles` with `extend_from_slice`.
- **`EmissionLog`** (`omuzaki-stage`, pure, property-tested) is a ring of `MAX_SPAWN_BATCHES` entries:
  - **A burst** is one batch with `interval = 0`. Its `count` is thinned to what fits under the cap.
  - **A stream** (weather, a fountain) is one open segment per stream key. Each frame its `count` is the number of
    births up to now, `floor((t − start) / interval) + 1`. A change of rate, emitter or area closes the segment and
    opens the next one, which continues the global index with the same seed.
  - **Prune** drops each batch's dead prefix: the particles with `t_g + lifetime.max < t`. It advances `first` and
    `start` by that many particles and removes batches that are empty and closed. A two-hour weather stream therefore
    stays about `rate × lifetime.max` slots.
  - **Pre-warm** opens a stream `lifetime.max` in the past (D7).
  - **Clear** sets every batch's `scale` from 1 down to 0 over `fade_s`, then empties the log.
  - **The cap and the weather share** are as in D8.
  - It allocates nothing after construction. Its output is a function of its calls and their times only.
- **Systems.** The stage uploads a preset's system on its first use, during the apply step between frames, and keeps
  it until the stage is dropped. Weather systems are immutable once in use:
  - A weather change (`stage.set_weather` or the live override) creates a new system for the new description and
    closes the old stream segment. The old segment keeps its system id, so its batches keep their old parameters
    until they expire, which is the D10 cross-fade.
  - The stage counts live batches per system and releases a weather system when its last batch is pruned. At most
    `MAX_SPAWN_BATCHES` systems can be alive, and a change to the same description reuses the live system.
  - A test switches snow to rain with snow particles alive and checks that their evaluated states are unchanged
    after the switch, and that rain particles use the rain values.
- **BR-12.** Physics hits run inside the stage's frame (ST-03), so a hit pushes its `hit-stars` burst into the same
  log directly, with the world hit point and no op. The throw op that caused it is what the log and the take record,
  and replaying the throw reproduces the hit.

### D13. Chat emote images: fetched, cached, shown live, never stored

- **Source.**
  - `ChatMessage.emotes` holds `EmoteSpan { start, end, id, name }` (automation D9). BR-10 adds one field, which its
    contract allows: `image: Option<String>`, an `https` URL of a static picture at about twice chat size.
  - The adapter fills it. On Twitch it is the CDN's static PNG for the emote id. On YouTube it is a custom emoji's
    image when the API supplies one, else nothing.
  - Nothing else builds URLs from ids. A graph passes `$in.emotes` from `on.event` straight to
    `stage.effect.burst { preset = "emote-rain", emotes = "$in.emotes" }`.
  - `ChatEmoteRef { id, name, image }` ignores the span offsets. A burst takes up to 8 distinct emotes, with 12
    particles each by default.
- **Fetch,** off the frame thread, in `omuzaki-stage`, through the shared HTTPS `Fetcher`:
  - HTTPS only, to hosts on an allowlist. First-party adapters declare theirs (`static-cdn.jtvnw.net`,
    `yt3.ggpht.com`), and a PL-03 adapter plugin declares its hosts in its manifest.
  - At most 256 KiB, a 5 s timeout, and no redirect off the list.
  - PNG only, through the `image` crate already in the tree with its `png` feature, so there is no new dependency.
    Other formats draw the placeholder.
- **Decode.** Limited to 512 × 512 and 4 MiB of allocation. The image is fitted into a 128 × 128 cell, premultiplied,
  and written into one of 8 buffers pooled at start.
- **Upload.** The render thread uploads at most 2 cells a frame with `update_texture` (64 KiB each) and never waits.
  Until an emote is ready, its particles draw the placeholder: a heart drawn by code at start.
- **The atlas.** One dynamic 1024 × 1024 sRGB, premultiplied, mip-mapped texture owned by the stage, holding 64 cells.
  Cell 0 is the placeholder.
  - Cells are reused least recently used first.
  - A cell is pinned while a live batch uses it, so an emote never changes in mid-air.
  - When every cell is pinned, new emotes draw the placeholder.
- **The cache.** A bounded folder `chat-emotes/` in the app's cache directory: at most 64 MiB, entries older than 7
  days removed, keyed by a hash of the URL.
  - It is never in the project, the store, takes, library items or exports. Takes record `ChatEmoteRef`, which is
    text only.
  - A test saves a project and an export after an emote rain and scans them for emote bytes.
- **Offline renders never use the network.** A take re-render draws an emote from the cache only if the job opts in
  (`chat_emotes = "cache"`), else the placeholder, and the render report lists the misses (§6, Q2).
- **The setting.** `effects.chat_emotes` has `User` scope, agent rule `Confirm` (network, ADR 0026 D2) and default on.
  Off draws the placeholder only. BR-14's blocklist applies to emote names and ids.
- **Rights.** These are third-party images, shown live and never redistributed: the same use chat itself makes. They
  go on the legal-review list (§40.3) with BR-10's platform terms.

### D14. Export, VRM, takes, collab and the web

- **Model exports carry nothing.** VRM, glTF, VRChat and PNGTuber exports describe a character. Particles are scene
  content, and those formats have no particle model worth mapping to. There is nothing to bake.
- **Pictures carry them.** Renders (SH-05), clips, thumbnails (SH-08) and sticker sheets (SH-07) draw particles into
  their pixels when the scene has them. A thumbnail renders at a fixed scene time with pre-warmed weather (D7).
- **Takes (SH-01).**
  - The stage emits an `EffectEvent` for each burst, live weather change and clear. It holds the frame time, the
    resolved anchor transform, the count after thinning, the seed and the emote refs.
  - The take recorder (its own task) stores the events and replays them through `EffectService::replay` at their
    times.
  - The scene's weather is already in the document.
  - **Seeking restores state, not just a window.** The log holds more than the last `lifetime.max` of births: a
    weather override started 60 s ago is still active, and a stream's global index counts every birth since it
    opened. So the recorder writes an `EffectCheckpoint` at the take's start and every 10 s: the whole `EmissionLog`
    (at most `MAX_SPAWN_BATCHES` entries), the active override with its expiry, and each open stream's key, phase
    and next global index. To render from frame `t`, restore the newest checkpoint at or before `t`, then replay
    the events after it up to `t`. No simulation runs.
  - **Checkpoints hold descriptions, not handles.** A `ParticleSystemId` belongs to one renderer and one run, and
    D12 releases weather systems once their batches expire. So a checkpoint stores each batch's system as a stable
    key (the preset name plus the full `ParticleSystemDesc` values, hashed) and the serialisable description itself.
    Restoring creates any missing system and maps keys to the new ids before the log is used. The same path loads a
    take into a fresh renderer. A test seeks backward past a release, and loads a take into a new renderer.
  - **Atlas systems store meaning, not handles or cells.** An atlas sprite holds a renderer-local `TextureId`, and a
    batch's `cells` are slots the LRU allocator (D13) chose, which a later eviction may give to another emote. So a
    checkpoint stores the sprite symbolically (`atlas = "chat-emotes"`, or a sprite sheet's asset key) and, for each
    atlas batch, a table of `ChatEmoteRef`s in place of cell numbers. Restore binds the symbolic atlas to the stage's
    current atlas texture (recreating it in a fresh renderer), re-resolves each emote ref to a cell through the
    allocator (pinning it as a live batch does), and writes those cells into the restored batches. An emote that is
    no longer cached, or that the job didn't opt into (D13), restores as the placeholder cell. A test evicts the
    cells a batch used, seeks back to it, and checks that the batch shows the same emotes (or the placeholder),
    never another one; another loads the checkpoint into a fresh renderer and stage.
  - **Tested against sequential playback.** A test records a take with an override longer than a lifetime window,
    a rate change and a clear, then seeks to many times, directly and from each checkpoint. The restored
    `EmissionLog` at each `t` must equal the one built by playing from the start, and the rendered frames must be
    byte-identical on one device.
- **Collab (ST-10).** Weather replicates with the scene document. Effect events go to every participant with their
  seed and time, so every screen draws the same particles.
- **The web player.** It uses the same contract and the same shader on WebGPU and WebGL2 (D6).
- **The library.** `stages` items may set weather. No item carries chat emotes.

### D15. Failure modes

| Failure | Behaviour |
|---|---|
| Unknown preset | the op fails with the nearest names (did-you-mean) |
| A burst over the live cap | thinned to fit; the effect event and `stage.effect.status` say by how much |
| A chat flood | the session's rate limits, then the queue of 64 (`Limited`), then the cap |
| A renderer without particles (`Unsupported`) | the stage logs it once; the ops succeed and the status says nothing is drawn |
| A scene naming a released system | `UnknownHandle` at `render`: a stage bug, caught by the stage's tests |
| An anchor that is gone (member removed, prop hidden) | the subject, or the stage origin, with a finding |
| An emote fetch that fails, is slow, or is refused (host, size, format) | the placeholder; logged at debug; never blocks a frame |
| A crafted URL in an event (`automation.fire`, a plugin) | the allowlist, HTTPS, size and decode limits; nothing else is fetched |
| Every atlas cell pinned | the placeholder for new emotes |
| A lost device or a renderer restart (BR-23) | systems are re-created; the batches live in the stage, so particles continue where they were |
| Over budget | the scheduler steps density down first, and says so |
| Panic (BR-14) | `stage.effect.clear` |
| A very long stream | indices are `u32` per stream (49 days at 1,000 a second); ages are computed per batch, so time doesn't drift in `f32` |
| A bad preset file in a development build | refused at load with the path and the rule; a test loads every file |

## 3. Worked example: a preset, a rainy scene and a chat trigger

`crates/omuzaki-stage/library/effects/confetti.toml` (starting values, to be tuned on renders):

```toml
name = "confetti"
title = "Confetti"
summary = "A burst of paper confetti in the subject's palette that tumbles and flutters down."
tags = ["celebration", "burst"]
version = "1.0.0"
origin = "designed for ST-13: paper rectangles 12 to 18 mm, launched at 3 to 5 m/s, terminal speed 1 m/s"
kind = "burst"
sprite = { shape = "rect", aspect = 0.6 }

[burst]
count = 150
at = { from = "subject", point = "chest", offset = [0.0, 0.0, 0.15] }
colours = "palette"

[particles]
lifetime = [3.5, 5.0]
speed = [3.0, 5.0]
direction = [0.0, 1.0, 0.0]
spread_deg = 35.0
gravity = [0.0, -3.0, 0.0]
drag = 3.0
wind = [0.0, 0.0, 0.0]
flutter = { amplitude_m = 0.05, frequency_hz = 1.5 }
size = [0.012, 0.018]
size_curve = [0.4, 1.0, 1.0, 0.0]
orientation = { kind = "tumble", rate_deg = [360.0, 900.0] }
colours = ["#ff5a7a", "#ffc94d", "#5ad1ff", "#9b7bff", "#6ee7a0", "#ffffff"]
coverage = 1.0
shading = { kind = "toon", shade = "#b8a8c8", shift = 0.0, softness = 0.1 }
glow = 0.0
floor = 0.0
```

With `v0 = 5` m/s up, `k = 3` and `u = −1` m/s, the velocity crosses zero at `e^(−3a) = 1/6`, so `a = 0.6` s and
`p = −0.6 + 6 × (5/6) / 3 = 1.07` m above the launch. A bright handful bursts from the chest and lands about three
seconds later.

`scenes/cafe/weather.toml`, after `stage.set_weather {scene: "cafe", preset: "rain", intensity: 0.6}`:

```toml
preset = "rain"          # informational: the values below came from this preset
intensity = 0.6
area = "auto"            # around the cast and the active camera
density = 160.0          # births per m² per second at intensity 1
height = 2.6             # subject units above the floor
sprite = { shape = "streak" }

[particles]
lifetime = [0.55, 0.75]
speed = [6.5, 7.5]
direction = [0.05, -1.0, 0.0]
spread_deg = 2.0
gravity = [0.0, 0.0, 0.0]
drag = 0.0
wind = [0.0, 0.0, 0.0]
size = [0.005, 0.007]
size_curve = [1.0, 1.0, 1.0, 1.0]
orientation = { kind = "velocity", stretch = 0.035 }
colours = ["#c8d6ec"]
coverage = 0.55
shading = { kind = "unlit" }
glow = 0.0
floor = 0.0
```

For a 1.6 m adult framed in a bust shot from 1.8 m, the auto area is about 3.7 m × 5.2 m (19 m²). At intensity 0.6
that is about 1,830 births a second and about 1,380 slots alive, well under the weather share of 3,500.

A chat command in a show, through `op.run` (automation D7). A redeem works the same way with `on.event`:

```toml
# shows/main/automations/rain/nodes/cmd.toml
kind = "chat.command"
[params]
name = "rain"            # matched with the default "!" prefix, so chat types !rain
cooldown_s = 60

# shows/main/automations/rain/nodes/go.toml
kind = "op.run"
[in]
in = "cmd.fired"
[params]
op = "stage.effect.weather"
args = { preset = "rain", intensity = 0.8, for_s = 60 }
```

And emote rain on every chat message that has emotes, passing the message's emote list through:

```toml
# shows/main/automations/emote-rain/nodes/drop.toml
kind = "op.run"
[in]
in = "msg.fired"
emotes = "msg.emotes"
[params]
op = "stage.effect.burst"
args = { preset = "emote-rain", emotes = "$in.emotes", only_with_emotes = true }
```

## 4. Crates and contract changes

| Crate | Kind | Change |
|---|---|---|
| `omuzaki-render-api` | API | `particles.rs` (D2, D3); `ParticleSystemId`, `ResourceRef::ParticleSystem`, `ResourceStats::particle_systems`; `Scene::particles`; `Quality::particle_density`; `Pass::Particles`; `FrameReport::particles`; `create_particle_system` and `update_particle_system` with defaults; `limits` |
| `omuzaki-render-mock` | Mock | particles drawn on the CPU as the reference; `conformance::PARTICLE_CHECKS`, kept apart from `CHECKS` until the GPU pass lands, as did with `LIGHTING_CHECKS` |
| `omuzaki-render` | Implementation | `pass/particles.rs` and `shader/particles.wgsl`; `PassSet` becomes `u16` |
| `omuzaki-stage-api` | API | `effects.rs`: `WeatherDef`, `EffectPreset`, `SpriteDef`, `EffectAt`, `EffectOrigin`, `BurstRequest`, `ChatEmoteRef`, `LiveWeather`, `EffectEvent`, `EffectStatus`, `PresetInfo`, `EffectError`, the `EffectService` trait; `SceneDef::weather` and `NODE_KEYS` gains `weather`; the weather check rules |
| `omuzaki-stage` | App | `effects/` (the log, the service, anchors, the emote atlas and pipeline, the frame step); `ops/effects/`; `library/effects/*.toml` |
| `omuzaki-live-api` | API | `EmoteSpan::image` (additive) |
| `omuzaki-live` | Implementation | two seed automation templates (follower confetti, chat emote rain) |
| spec v3 | | §10: the `gpu.particles` row and its definition  |
| `docs/design/scenes.md` | | D1: the `weather` row; D8: `stage.set_weather`  |

There are no new external dependencies.

## 5. The task cut

The whole cut is blocked by this design through its chains. Milestone: Phase 2. Priority: P1, as ST-13, except
the critical path of LB-14's follower confetti, which is P0 (§6, Q6). The render contract, the mock, the conformance
checks, the `Particles` pass, the stage's effect service and the burst op are on that path.
Rendering tasks need renders or a GPU to check; the others have no visual or GPU part. The original eight
were re-cut: the contract lost the mock, the GPU check shrank to a profile check, the live ops lost the log, and the
service gained it.

- `omuzaki-render-api` contract (D2, D3, the reference functions and their unit tests); the stopgaps the new `Pass` and resource force on `omuzaki-render` and the mock
- `omuzaki-render-mock`: particles drawn on the CPU (shapes, edges, coverage by ordered thresholds, toon, fog, environment, atlas)
- conformance: `PARTICLE_CHECKS` (the 16 checks)
- `omuzaki-render`: the `Particles` pass on the GPU (D4 to D6): colour and alpha draws, depth clamp; the parity test; goldens and the compositing tests (D6); the checks moved into `CHECKS`
- measure `gpu.particles`: the particle budget scene in `frame_times`, four resolutions, Vulkan and DX12, the `R` estimates
- the particle conformance under the `WebGl2` profile on lavapipe; the wasm check
- `omuzaki-stage-api`: `effects.rs` types and `EffectService`; `SceneDef::weather` and its checks
- the preset loader, `confetti` and `snow` at §3's values; `stage.set_weather`, `stage.effect.list`
- the live-action ops `stage.effect.burst`, `.weather`, `.clear`, `.status` against a fake `EffectService`
- `omuzaki-stage`: the `EmissionLog` (D12): bursts, streams, prune, pre-warm, clear, cap, weather share
- `omuzaki-stage`: the effect service, anchors, the weather area, immutable weather systems released with their last batch (D12), the frame step and fill, and the checkpoint save and restore with key-to-handle remapping (D14)
- the six presets tuned on renders, contact sheets, cost per preset, the flash rule
- `omuzaki-stage`: `EmoteAtlas`, the cell allocator (LRU, pins, placeholder cell), and re-resolving `ChatEmoteRef`s to cells for checkpoint restore (D14)
- the chat-emote pipeline (D13): allowlist, fetch, decode, pool, upload, disk cache, never stored
- the `effects.chat_emotes` setting, read by the pipeline
- seed automation templates: follower confetti and chat emote rain

**Effects on other features** (no new tasks):
- Scheduler: particle density is its first rung (D8).
- Live: `EmoteSpan::image` (D13).
- BR-14: the panic button calls `stage.effect.clear`.
- BR-12: the `hit-stars` preset and the in-process burst path (D12).
- LB-14: `stages` items may set weather.
- The `Transparent` pass: particles write depth before it, and nothing changes there.
- Budgets: the `gpu.particles` row.

## 6. Decisions taken on review (2026-10-13)

- **Q1. Stateless particles and the coverage pass: accepted** (ADR 0028). Closed forms evaluated from the spawn batch
  and the time, with no compute pass, instead of a compute simulation; alpha-to-coverage instead of
  sorted blending. If a feature ever needs collisions with the avatar or stateful fields, a compute path sits behind
  the same contract (D1, ADR 0028).
- **Q2. Chat emotes in offline re-renders of takes: the placeholder by default.** The local cache is used only when the
  render job asks (`chat_emotes = "cache"`), and the render report lists the misses (D13).
- **Q3. Person-made named effect presets: not now.** Per-burst overrides and the scene's own weather cover ST-13. A
  project `effects/` node with save and edit ops is added if people ask for it.
- **Q4. Unicode emoji in emote rain: not in ST-13.** It would need a colour emoji font (Noto Color Emoji, OFL-1.1, which
  the policy allows) and a CPU glyph rasteriser; a follow-up if wanted.
- **Q5. The budget: accepted.** `gpu.particles` ≤ 0.2 ms p95 on `R` for 5,000 live particles at 1080p is in spec §10.
  The `gpu.particles` measurement checks it; a different number would be a spec change for the maintainers.
- **Q6. LB-14's critical path is P0.** Follower confetti (LB-14, P0) needs the render contract, the
  `Particles` pass, the stage's effect service and the burst op, so
  those tasks are P0. The rest of ST-13 stays P1.

## 7. Sources

Read on 2026-10-13:
- **Spec v3:** §8, §9, §10, §21, §24, §25, §26, §28, §31.
- **Docs:** `docs/design/render-api.md`, `scenes.md`, `library.md` and `settings.md`; ADR 0009, ADR 0019, ADR 0024
  and ADR 0026.
- **Code:** `crates/omuzaki-render-api/src/{pass,scene,quality,renderer,resource}.rs`;
  `crates/omuzaki-render/src/{pass/mod.rs,pass/transparent.rs,frame/draws.rs,frame/graph.rs,frame/timing.rs}`;
  `crates/omuzaki-render-mock/src/conformance.rs`; `crates/omuzaki-stage-api/src/{lib,scene,anchor}.rs`;
  `crates/omuzaki-stage/src/recipe/ground.rs`; `crates/omuzaki-core/src/rng.rs`; `crates/omuzaki-ops-api/src/services.rs`.
- **Algorithms:**
  - M. Jarzynski and M. Olano, "Hash Functions for GPU Rendering", JCGT 9(3), 2020 (the PCG hash).
  - T. Duff et al., "Building an Orthonormal Basis, Revisited", JCGT 6(1), 2017.

  Both are algorithms, restated here; no code is copied.
