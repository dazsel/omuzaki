---
title: "0068: Flat 2D puppets: one contract, one runtime, every source into the same PuppetDef (2026-10-29)"
summary: "Flat 2D puppets: puppet-api holds the data, omuzaki-puppet the runtime and the CPU reference renderer, Inochi2D is a lib; layered art, video and tracks author the same PuppetDef"
area: decisions
---

# 0068: Flat 2D puppets: one contract, one runtime, every source into the same PuppetDef (2026-10-29)

**Status:** Accepted (2026-10-29, the Render area's owner, under ADR 0067). **Design:** `docs/design/puppet2d.md`.

## Context

EX-08 asks for flat 2D puppets at full scope beside 3D characters. The quality references for short-form video are
flat 2D puppets, and the product could not make one: no 2D data model, no runtime, no renderer path, and no way to
author a puppet without a rigger's days in another tool.

## Decision

1. `omuzaki-puppet-api` (API) holds the data: `PuppetDef`, `ParamMapping` and `DrawList2D`, with `check()`. Model
   space is canvas pixels, y down, origin at the centre (Inochi2D's axes).
2. `omuzaki-puppet` (Implementation) holds the runtime: evaluation without per-frame allocation, pendulums at a fixed
   1/240 s step, the channel driver, the blend formulas as pure functions and a CPU renderer that is the reference for
   every other renderer and the offline path for shorts and tracks.
3. `omuzaki-inochi2d` (Lib) reads Inochi2D into `PuppetDef` with a report of what it mapped, made up, dropped or
   approximated, and limits checked before allocation.
4. Every way of making a puppet produces the same `PuppetDef`: an import, the auto-rig from layered art, a puppet from
   a video, or an agent writing JSON. A performance is a track of views and parameters, so any source of motion plays
   back the same way.
5. The GPU pass draws `DrawList2D` into the stage's target and is accepted against the CPU renderer's pixels.

## Consequences

- One runtime to make fast and correct; the importer, the auto-rig and the video solver are only producers.
- Shorts play on puppets now (offline, on the CPU); the live stage waits on the GPU pass.
- What `PuppetDef` can't yet hold of Inochi2D is reported by the importer and is leaf work on the EX-08 epic.
