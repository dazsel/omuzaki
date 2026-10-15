# omuzaki-anim-api: `HumanoidFrames` checks `rest_local`'s length

- `HumanoidFrames::to_humanoid` and `apply_humanoid` return `Result<(), RestLengthMismatch>`: a `rest_local` that is not one transform per joint of the skeleton is an error, and `out` is left untouched. Before, a short slice could leave stale transforms and an oversized one panicked (#1118).
- `HumanoidRig` keeps its infallible signatures; its rest transforms come from the same skeleton, so the error cannot happen there.
- Both conversions are `#[inline]`, which the benchmarks in the pull request measured.
