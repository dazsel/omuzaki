# 0059: `HumanoidFrames` checks the length of `rest_local`, a breaking `omuzaki-anim-api` change (2026-10-15)

**Status:** proposed (PR #1625); accepted by the Director when merged. **Context:** #1118, #861 (`HumanoidFrames`);
`docs/design/anim-api.md`.

## Context

`HumanoidFrames::to_humanoid` and `apply_humanoid` index the `rest_local` slice they are given by joint, but nothing
checked its length. A short slice could leave transforms in `out` stale, and an oversized one panicked inside
`apply_humanoid`. Both functions are in the contract crate, so a caller outside `omuzaki-rig` could hit either case.

## Decision

1. `HumanoidFrames::new` records the skeleton's joint count.
2. `to_humanoid` and `apply_humanoid` return `Result<(), RestLengthMismatch>`. They compare `rest_local.len()` with the
   recorded count once, at the top. On a mismatch they return the error and write nothing to `out`.
3. `RestLengthMismatch { expected, actual }` is a documented `thiserror` type in `omuzaki_anim_api::humanoid_frames`.
4. Both functions are `#[inline]`, as the benchmarks in #1118 measured.

`omuzaki-rig`'s `HumanoidRig` keeps its infallible methods. Its rest transforms come from the same skeleton the table
was built from, so it handles the error with a `debug_assert!` and a comment saying why it cannot occur.

## Migration

- **A caller of `to_humanoid` or `apply_humanoid`** must use the result: propagate it with `?`, or handle it. Discarding
  it is a warning, because `Result` is `#[must_use]`.
- **A caller that builds the table from a skeleton and passes that skeleton's rest transforms** needs no change; the
  check always passes for it.

## Consequences

- Source-breaking for any caller outside this workspace that discards the result of either method. The only caller in
  the workspace is `omuzaki-rig`, which stays infallible.
- An exact-length `rest_local` gives the same output as before, bit for bit: the rig's conversion digest test is
  unchanged.
- The check is one comparison and adds no allocation; the A/B in #1625 is within the issue's bound.
