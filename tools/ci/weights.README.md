# weights.json

Seconds of test time per crate, used by `cargo xtask check --format json` (`xtask/src/report.rs`) to balance the CI
test jobs. Crates not listed weigh `default`. Only crates above the default are listed.

Source: the test-job logs of merge-queue run 38596279345 (all six test jobs). For each crate the seconds between its
first `Running ...` line and the end of its last test binary (doctests included) are summed from the log timestamps.
`omuzaki-char` is 1361 s there (about 17 minutes in run 38589415493: the same tests, `cargo test` running its binaries
one after another). Compile time is not included, so a crate that is cheap to test but slow to build still weighs the
default; the default stands in for that per-crate cost.

Weights are serial times (the old `cargo test` behaviour); nextest runs tests in parallel, so real jobs finish sooner.
Refresh them when the measured shard spread drifts: compare the per-job durations on a full-workspace run.
