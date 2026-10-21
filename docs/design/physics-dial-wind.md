---
title: "The physics dial, scene wind and spring presets"
summary: "One dial from rigid to as-authored for springs, hair and cloth; a deterministic scene wind every runtime samples; per-chain spring presets; their contracts, costs and the implementation split."
area: design
---

# The physics dial, scene wind and spring presets

**Status:** proposed. The Character lead approves the whole design. The Director signs §3 (the wind and scene
contract), which crosses into the Render, stage and V0 area.
**Feature:** RG-03 (spec v3 §20), parent #36. This design is #1961, part of change C7 (#2100).
**Builds on:** decision 0009 (fixed 120 Hz sub-steps, determinism), `docs/design/scenes.md` (D1, D10; its §1 says
wind "would be one more `lighting`-like key, additive"), C2 (#2082, the cloth runtime and its `StepCtx` LOD field).

## 1. Today's inputs

| System | Where | What moves it | Wind today |
|---|---|---|---|
| Spring bones | `omuzaki-anim-api/src/spring.rs` (`SpringParams`, `SpringChain`, `SpringSet`, trait `SpringSolver`), solved by `omuzaki-rig/src/spring/` (`VrmSpringSolver`) | per joint: `stiffness`, `drag_force`, `gravity_power`, `gravity_dir`, `hit_radius` (VRM 1.0's names and units); the animated pose, interpolated across sub-steps (#79) | none: the trait has no wind input |
| Hair (Verlet chains, #182) | `omuzaki-char-api/src/hair.rs` (`HairPhysics`), core in `omuzaki-char` | `damping`, `gravity_scale`, `iterations`, `collider_margin`, `wind_scale` (0 to 5) | `StepCtx::wind` times `wind_scale` |
| Cloth (#197, C2) | `omuzaki-char/src/cloth/` (`solver.rs`: `stretch`, `shear`, `bend`, `damping`, `gravity`; `runtime.rs`) | XPBD sheet pinned to the skeleton | `StepCtx::wind` (C2 decides how it is applied) |
| The frame | `omuzaki-stage/src/frame/mod.rs` | `FrameInput::wind: Vec3` is copied into `StepCtx { dt, time, pose, wind }` | whatever the caller passes; nothing fills it from the scene |

The springs run inside `FullRig` (`omuzaki-stage/src/frame/rig.rs`); hair and cloth run as `PartRuntime`s through
`StepCtx` (`omuzaki-char-api/src/runtime.rs`). So the dial and the wind need two doors: the `SpringSolver` trait and
`StepCtx`.

## 2. The master physics dial

**One scalar `physics` in `0..=1`: 0 is rigid, 1 is as authored.** It is an **output blend**, not a parameter
change. Each runtime simulates exactly as authored and then writes `lerp(rest, simulated, physics)`:

| System | "rest" (what 0 shows) | Blend |
|---|---|---|
| Spring bones | each swinging bone's rest rotation under the animated pose (its `initialLocalRotation`) | `slerp(rest, solved, physics)` per swinging joint, written into the pose |
| Hair | each particle's skinned rest position | `lerp` per particle, before the mesh is rebuilt |
| Cloth | each sheet particle's skinned rest position | `lerp` per particle, before the render mesh is driven |

**Why a blend and not scaled stiffness, drag or gravity:**
- **Exact at both ends.** At 1 the code path is skipped, so the output is bit-identical to today's (a test pins it). At
  0 the character is rigid at once. Scaling forces only tends to rigid: a tail still drifts back at the stiffness rate.
- **The simulation never changes**, so moving the dial never pops, and a recorded take replays the same whatever the
  dial was while recording. The dial can be animated (a calm shot, then a gust) without resetting anything.
- **One rule for three systems** with different parameters (VRM per-step forces, Verlet damping, XPBD compliance).
  A mapping per system would differ in feel at the same dial value.
- **Accepted cost:** between 0 and 1, a blended hair tail can cut the chord of a collider sphere it was pushed around.
  The depth is at most the sagitta of the swing, a few millimetres at 0.5 for typical hair. A dial below 1 is a
  stylistic choice, and the colliders still hold at 1.

**Where it lives:** two factors, multiplied.
- **Per character**, in the character document: `physics = 1.0`. This is the user's "how floppy is this character"
  setting.
- **Per scene**, in `scenes/<id>/scene.toml`: `physics = 1.0`. This is the director's "calm shot" setting, applied to
  the whole cast.
- The stage resolves `character × scene` on a document change (scenes D10, step 1), clamped to `0..=1`, with a value
  that is not finite read as 1. Each frame it hands the value to the springs and to `StepCtx`. A timeline track can
  drive the scene factor the way it drives a light (scenes §1, SH-02).

**How a runtime reads it:** springs through the `SpringSolver` environment call (§5), part runtimes through
`StepCtx::physics`. Both are plain `f32` copies: no allocation, no lock, deterministic.

## 3. Scene wind (the Director signs this section)

**Ownership.** Wind is the scene's, like the light rig: a new node key `wind` in `scenes/<id>/wind.toml`, typed as
`WindDef` in `omuzaki-stage-api`. It is additive to `SceneDef::NODE_KEYS`, and a scene without the file has no wind.
The `stage.*` ops edit it, one patch per intent, with undo, as for lighting (scenes D1).

```toml
# scenes/<id>/wind.toml
direction = [1.0, 0.0, -0.3]   # world space, normalised on resolve; [0, 0, 0] means no wind
speed = 3.0                    # metres per second, 0..=30
gust = 0.4                     # gust strength as a share of speed, 0..=1
gust_period = 2.5              # seconds, 0.2..=20: the slowest gust cycle
wobble = 15.0                  # degrees the direction swings with the gusts, 0..=90
seed = 7                       # u64: two scenes with the same values and seed blow the same
```

Every value is checked where it enters (`WindDef::check`, run by `SceneDef::from_node`). A value that is not finite or
is out of range is an error finding that names the key and the range, never a silent clamp.

**Gusts are a pure function of scene time and the seed.** On resolve, the seed gives three phases `φ1..φ3` and a wobble
phase `φ4` (`omuzaki_core::rng`, fixed draw order). With `T = gust_period` and `t` the frame's `Tick::time` (the
shot's clock, which pinned time and scrubbing also move):

```text
g(t)   = 1 + gust · (0.6 sin(2π t/T + φ1) + 0.3 sin(2π 2.3 t/T + φ2) + 0.1 sin(2π 5.1 t/T + φ3))
yaw(t) = wobble · sin(2π t/(3.7 T) + φ4)                     (about world +Y)
wind(t) = rotate_y(yaw(t)) · direction · speed · max(g(t), 0)
```

The frequency ratios (2.3, 5.1, 3.7) are irrational enough that the pattern doesn't visibly repeat within a minute. The
same scene, seed and time always give the same vector, so offline renders (SH-05) and replays are exact, and a pinned
still sees the wind of its pinned time.

**The one read-only view.** `ResolvedWind` (in `omuzaki-stage-api`, `Copy`) is built on resolve. Its pure method
`ResolvedWind::at(time: Seconds) -> Vec3` is the only way any runtime sees wind. Each frame the stage samples it once
per cast member, at the frame's time. The result fills `StepCtx::wind` (as today, now from the scene) and the springs'
environment (§5). The sample is uniform over a character: a gust front crossing 2 m of character at 3 m/s takes
0.7 s, so the phase lag doesn't show at this scale. A spatial field (`at(time, position)`) can be added to
`ResolvedWind` later without changing any runtime contract. `FrameInput::wind` becomes an override: when set, it
replaces the scene's sample (tests, tracking-driven wind).

**What each runtime does with the vector:**
- **Spring bones: wind as moving air, through the drag VRM already has.** The spec's inertia
  `(current − prev)(1 − drag)` damps a tail's velocity toward still air. With wind, it damps toward the air's
  velocity instead: `(current − prev − w)(1 − drag) + w`, where `w = wind · PHYSICS_STEP` is the air's displacement
  in one sub-step. This adds no parameter. Chains with more drag catch more wind, which is what drag means. At zero
  wind it is the spec's formula, bit for bit, and the oracle (`reference.rs`) adds the same term.
- **Hair:** unchanged (`wind · wind_scale`, #182).
- **Cloth:** C2's choice, read from the same `StepCtx::wind`.
- **The dial** applies after the wind, like every other force: a rigid character ignores wind.

**Contract changes in the stage area (co-signed by the Render, stage and V0 lead):**
- `omuzaki-stage-api`: `WindDef` (`#[non_exhaustive]`, `Default` = no wind, a `new` constructor), `ResolvedWind` and
  its `at`, the `wind` node key, and `physics: f32` on the scene settings (serde default 1).
- `omuzaki-stage`: the resolve step, the per-frame sample, the ops `stage.set_wind {scene, ..fields}` and
  `stage.set_physics {scene, value}`, and `FrameInput::wind: Option<Vec3>` as the override.

## 4. Per-chain spring presets

A preset is a named `SpringParams` pair, root and tip, applied with `SpringChain::tapered`, which chains already use.
First-party presets:

| Preset | Root: stiffness / drag / gravity | Tip: stiffness / drag / gravity | Feel |
|---|---|---|---|
| `stiff` | 4.0 / 0.7 / 0.1 | 2.5 / 0.6 / 0.2 | short hair, ears, antennae: barely swings, snaps back |
| `bouncy` | 1.2 / 0.15 / 0.3 | 0.8 / 0.1 / 0.5 | ponytails, ribbons: overshoots and rebounds |
| `floaty` | 0.4 / 0.35 / 0.05 | 0.2 / 0.4 / 0.0 | long loose hair, capes: slow drift, catches wind (high drag) |
| `heavy` | 1.5 / 0.5 / 1.5 | 1.0 / 0.5 / 2.5 | braids, chains, tails with weight: hangs and swings low |
| `vrm-default` | 1.0 / 0.5 / 0.0 | 1.0 / 0.5 / 0.0 | VRM 1.0's defaults, for resetting |

`gravity_dir` stays −Y and `hit_radius` is never touched: the radius is geometry, fitted to the chain, not feel.
Every value is inside `SpringParams`' validated ranges. The numbers are starting points, tuned by eye on the kit
characters' hair in the implementation change, with renders as evidence.

**Where they live:** a new library kind `spring-presets` (library D1 table: "Spring presets", scope character, setting
on a `$target` spring chain, no slot). The data is in `crates/omuzaki-char/data/spring-presets/<name>.toml`, embedded
by `omuzaki-embed` (library D14), so wasm needs no file system. A user's own presets are ordinary library items.

**The op:** `char.set_spring_preset {character, chain, preset}` (in `omuzaki-char`, registered next to the other char
ops). It writes the chain's tapered params into the character document: one patch, undo, recorded like any
`library.apply` setting. The live stage path calls `VrmSpringSolver::set_params` per joint, so the hair keeps swinging
with no pop (the solver already supports this; `changing_parameters_mid_swing_does_not_pop_the_hair` pins it). VRM
export is unchanged: a preset is just authored params, and it exports as such.

## 5. Contracts

The Character area's contracts go first, alone, in one small change:
- **`omuzaki-anim-api` (`SpringSolver`)**: one additive method with a default, so no implementer breaks:
  `fn set_environment(&mut self, env: SpringEnv) {}`. `SpringEnv` is `#[non_exhaustive]` with `SpringEnv::new(wind:
  Vec3, physics: f32)` and `Default` (no wind, physics 1). The stage calls it before each `step`. It is per frame,
  takes no allocation and copies two values. `omuzaki-anim-mock`'s solver ignores it.
- **`omuzaki-char-api` (`StepCtx`)**: one field, `physics: f32`, after C2's LOD field. `wind` keeps its meaning (world
  m/s), now filled from the scene. The Render, stage and V0 lead co-signs because the stage builds `StepCtx`.
- **No change** to `SpringParams`, `SpringChain` or `SpringSet`: the wind needs no new per-chain parameter (§3), and a
  per-chain wind scale is deferred until a character shows the drag coupling isn't enough.

The stage area's contracts (§3) land in their own change, after the Director's signature.

**Determinism and real time:** the dial and the wind are plain values. Gusts depend only on scene time and the seed,
never on the wall clock or the frame count. Nothing new allocates per frame. The wind term and the blend run in fixed
order (joints in solver order, particles in index order).

## 6. Cost budgets

Spec §10 gives the CPU side under 3% of 8 cores at 1080p30, and physics at most 0.5 ms per character
(`RuntimeCost`). Measured today: 300 spring joints step in about 108 µs at 60 fps and about 222 µs at 30 fps
(`cargo bench -p omuzaki-rig --bench spring`).

| Addition | Work | Budget (300 joints, or 2,000 particles) |
|---|---|---|
| Spring wind term | one vector multiply-add per joint per sub-step; skipped at zero wind | ≤ 2% of the spring bench |
| Spring dial | one slerp per swinging joint per frame; skipped at 1 | ≤ 3 µs |
| Hair or cloth dial | one lerp per particle per frame; skipped at 1 | ≤ 4 µs per 2,000 particles |
| Wind sample | five `sin`s per cast member per frame | ≤ 0.1 µs |
| Resolve | on a document change only | not on the frame path |

Each implementation change adds its bench lines next to the existing ones and an allocation test that covers the new
path (the pattern of `tests/spring_alloc.rs`).

## 7. The split: implementation order

Each task below is about 150 to 300 lines, with its fence and its acceptance. Contracts go first and alone.

1. **Contracts, Character area** (alone, first). Fence: `omuzaki-anim-api/src/spring.rs` (`SpringEnv`,
   `set_environment`), `omuzaki-char-api/src/runtime.rs` (`StepCtx::physics`), `omuzaki-anim-mock` (ignore it), and
   every `StepCtx` literal the field breaks. Acceptance: `SpringEnv` builds only through `new` and `Default`; the
   default impl changes nothing (an existing solver's output is bit-identical); the Render, stage and V0 lead co-signs.
2. **Contracts, stage area** (alone, after the Director signs §3). Fence: `omuzaki-stage-api`'s scene module
   (`WindDef`, `ResolvedWind::at`, the `wind` key, `physics`). Acceptance: every field is checked at entry with a
   finding per bad key; `at` is a pure function (same scene, seed and time give the same bits; a different seed
   differs); `speed = 0` is exactly zero wind; JSON and TOML round-trips.
3. **Rig: dial and wind in the springs** (after 1). Fence: `omuzaki-rig/src/spring/**` and its tests and benches.
   Acceptance:
   - physics 1 and zero wind is bit-identical to today;
   - physics 0 writes the rest rotations;
   - the oracle with the same wind term agrees to 1e-4 at every frame rate in the #1849 table;
   - a steady wind settles a chain to a fixed downwind angle;
   - no allocation;
   - the §6 budgets.
4. **Stage: the scene wind and the dial in the frame** (after 2 and 3; files of the Render, stage and V0 lead). Fence:
   `omuzaki-stage/src/frame/*`, `omuzaki-stage/src/scene/*` (resolve), the two ops. Acceptance:
   - a scene with wind moves a kit character's hair, and the same scene and seed renders the same frames twice;
   - the dial at 0 renders the hair at rest;
   - the `FrameInput::wind` override wins over the scene's wind;
   - no per-frame allocation.
5. **Hair and cloth read the dial** (after 1, #182 and C2). Fence: the hair runtime and `omuzaki-char/src/cloth/runtime.rs`.
   Acceptance: physics 1 is bit-identical, physics 0 gives the skinned rest mesh within 1e-5 m, and the blend is
   continuous in between.
6. **Spring presets** (after 1, independent of 3 to 5). Fence: `crates/omuzaki-char/data/spring-presets/`, the op in
   `omuzaki-char`, and the library kind row. Acceptance:
   - every preset validates;
   - applying one mid-swing doesn't pop (largest turn per frame below the free swing's);
   - a VRM export of a preset chain carries its params;
   - renders of the kit hair under each preset as evidence;
   - the library conformance run passes.

#36 (RG-03) closes when tasks 3 to 6 have merged.
