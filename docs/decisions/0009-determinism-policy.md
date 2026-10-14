# 0009: Determinism policy (2026-10-12)

**Status:** accepted. **Context:** spec v3 §8 (determinism), §34 (golden renders); the render-api and anim-api contracts.

## Decision

1. **Same machine, same toolchain: bit-identical.** A recorded take, a score or an offline render, replayed with the same inputs, seed and build, gives identical bytes. Tests may assert byte equality here. Covered by:
   - seeded RNG per system (`omuzaki_core::rng`)
   - `FakeClock` in tests
   - fixed-step simulation
   - no wall-clock or thread-timing dependence in `Offline` mode
2. **Across platforms or toolchains: equal within a stated tolerance.** Each golden scene or numeric test that runs on more than one platform states its tolerance. The defaults:
   - images: 2 sRGB levels for CPU paths, per scene for GPU paths
   - channels: 1e-5
   - joint transforms: 1e-5 m and 1e-5 rad

   The usual causes are libm differences (`powf`, `sin`) and FMA contraction. We don't pay for cross-platform bit-exactness (a soft-float libm everywhere) unless a feature needs it.
3. **Serialisation never loses precision.** `serde_json` uses `float_roundtrip` workspace-wide, and TOML floats are written shortest-round-trip. A save and reload never changes a replay.
4. **What breaks determinism is a bug:** hash-map iteration order reaching output, unseeded RNG, reading the clock in a replayed path, data races. File it as a `bug` with the repro.

## Why

Goldens, replays, agent evaluations and "make the face more oval → same result every time" depend on (1). Requiring (2) to be bit-exact would cost real performance on the real-time path for no user-visible benefit.
