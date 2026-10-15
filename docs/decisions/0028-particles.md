---
title: "0028: Stateless particles, evaluated on the GPU from spawn batches and drawn by coverage in their own pass (2026-10-13)"
summary: "Stateless particles, evaluated on the GPU from spawn batches and drawn by coverage in their own pass"
area: decisions
---

# 0028: Stateless particles, evaluated on the GPU from spawn batches and drawn by coverage in their own pass (2026-10-13)

**Status:** accepted (2026-10-13). It changes `omuzaki-render-api` (a new resource,
a `Scene` field, a `Quality` knob and a ninth `Pass`) and `omuzaki-stage-api`. **Context:** ST-13, BR-12;
spec v3 §8, §10, §21, §24, §25, §26; render-api D1, D2, D5, D7 and D12; the LK-08 work, the
`Transparent` pass, capability profiles and the scheduler; ADR 0009 (determinism). Design:
`docs/design/particles.md`.

## Context

ST-13 asks for confetti, emote rain, rain, snow and petals, triggered live from hotkeys, chat and the automation graph,
"budgeted, GPU-driven". BR-12's hit effects reuse them. The render contract had no particles. Six facts shape the
answer:

- **Render-api D7.** `Offline` output depends only on the resources, the scene and the quality, never on earlier
  frames. SH-05, goldens, thumbnails and timeline scrubbing rely on it.
- **One renderer, several targets.** The viewport and the stream output render the same `Scene` in one frame.
- **Three capability profiles**. `WebGl2` has no compute shaders and no storage buffers in the vertex stage.
  Lavapipe runs CI and the goldens.
- **The CPU budget is small.** `cpu.total` leaves about 3 ms a frame once a webcam tracker has run (§10).
- **Translucent surfaces are common.** Imported VRMs have them (hair tips, lenses), and so does the shadow-catcher
  ground. They draw in `Transparent`, sorted among themselves.
- **The scheduler degrades by pass.** It reads per-pass times from `FrameReport`, and must thin decoration before it
  touches the character's look.

## Decision

1. **Particles are stateless.** A particle is a pure function of its spawn batch, its index and the scene time: a
   closed-form path (gravity, linear drag toward a wind, flutter), a size curve, a lifetime. Its random numbers are an
   integer hash of the seed, the index and a fixed stream number. Nothing about a particle is stored.
2. **Spawn batches go on the scene; particle systems are resources.** `create_particle_system` uploads the look and
   motion parameters once and returns a `ParticleSystemId`. Each frame the stage fills `Scene::particles` with the
   batches that still have live particles: a burst is one batch, steady emission one batch per segment. The stage keeps
   them in a fixed-capacity log, with no allocation per frame.
3. **The GPU evaluates every particle in the vertex shader.** There is no compute simulation and no CPU simulation.
   Batch records travel in 16 KiB uniform blocks, so the `Native`, `WebGpu` and `WebGl2` profiles and lavapipe run
   one shader. The mock evaluates the same reference function (`particles::evaluate`), and a parity test holds the
   WGSL to it.
4. **A ninth pass, `Particles`, between `Outlines` and `Transparent`.** Particles draw with alpha-to-coverage into the
   multisampled scene colour, and write depth. A second, alpha-only draw (depth `Equal`) sets alpha 1 in the
   covered samples, because the coverage draw keeps the shader alpha. Clip depth is clamped to `z ≤ (1 − 2⁻²⁰) × w`,
   so a particle never matches the cleared depth. Nothing is sorted, and the result doesn't depend on draw order.
   Blended instances drawn after them depth-test against them, so both orders between a particle and a translucent
   surface come out right. Particles fade by size, not alpha.
5. **Budget, cap and degrade.** `gpu.particles` ≤ 0.2 ms p95 on `R` for 5,000 live particles at 1080p, measured on
   the new pass and kept on top of `gpu.frame`, as face paint is. A live cap of 5,000 slots (weather at most 70 % of
   it) holds the budget. `Quality::particle_density` is the scheduler's first knob, thinning by the same per-particle
   hash, so stepping down never restarts an effect.
6. **Weather is scene state; bursts are performance.** Weather is a materialised `weather` key in the scene document,
   edited by `stage.set_weather` (undoable). Bursts and a live weather override are live actions (scope `perform`,
   empty patch, never in undo) behind an `EffectService` that the automation graph reaches through `op.run`. The
   presets are typed data files in `omuzaki-stage`, not library items.
7. **The extension point.** If a feature ever needs collisions with the avatar or stateful fields, a compute path can
   sit behind the same contract (`ParticleSystemDesc` and `SpawnBatch`) as a stepped variant, kept deterministic by
   re-simulating each batch's lifetime window, with nothing else changing.

## Consequences

- **Contract changes.**
  - `omuzaki-render-api`: `particles.rs`, `ParticleSystemId`, `ResourceRef::ParticleSystem`, `Scene::particles`,
    `Quality::particle_density`, `Pass::Particles` (`Pass::COUNT` 9), `FrameReport::particles`, two `Renderer` methods
    with `Unsupported` defaults, and four limits.
  - `omuzaki-render`: `PassSet` widens from `u8` to `u16`.
  - `omuzaki-stage-api`: the effect types, the `EffectService` trait and `SceneDef::weather`.
  - `omuzaki-live-api`: `EmoteSpan::image`.
  - No new external dependency.
- **What render-api keeps.** D7 holds unchanged. Any frame renders on its own. Two targets per frame cost nothing
  extra. A lost device or a renderer restart loses no particles, because the batches live in the stage. Takes and
  collab replay effects from their events and seeds.
- **The cost.** A particle can't react to the world after its birth: no collisions with the character, no turbulence
  fields. If a look needs that, a stepped variant of `ParticleParams` can add a fixed-step compute simulation behind
  the same descriptor, kept deterministic by re-simulating each batch's lifetime window. No spec row needs it today.
- **Coverage has limits.** Translucency has `samples + 1` levels (five at the live preset). At one sample a particle
  is alpha-tested. Mock and GPU are compared by measurements, never pixel for pixel, because sample patterns are the
  implementation's own.
- **The spec.** §10 gains the `gpu.particles` row, accepted; the measurement task reports it,
  and a different number would be a spec change. `gpu.frame` sums every pass except `PaintTextures` and `Particles`.

**Rejected:**
- **A stateful GPU compute simulation** (particle buffer, free list, indirect draw). It breaks D7, orders slots
  nondeterministically through atomics, needs a second path for WebGL2, loses state with the device, and steps twice
  with two targets.
- **A CPU simulation uploading instances.** It costs CPU per particle inside `cpu.total`, uploads per particle, and is
  still history.
- **Per-frame emitter state on `Scene`, with particles kept in the renderer.** That is the hidden retained state
  render-api D1 rejects.
- **Particles inside `Transparent`, sorted per particle with the blended instances.** Sorting needs compute or CPU
  positions, and the scheduler couldn't see the particles' cost.
- **Blended particles sorted per batch.** A weather volume is in front of and behind the character at once.
- **Blended particles after `Transparent`.** They would draw over translucent hair they are behind.
- **Weighted blended order-independent transparency.** Two more targets, and approximate alpha that breaks
  premultiplied output (BR-01).
- **Additive blending.** Invisible over a transparent background in straight-alpha consumers. Glow through bloom
  gives sparkles instead.
- **Presets as library items.** An item is a recipe of document edits, and a burst edits nothing. `stages` items can
  still set weather.
- **Weather toggled live as a document edit.** A chat command would write the person's scene and fill its history, and
  an automation would need `edit:scene` approved.
