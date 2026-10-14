# weights.json

Estimated seconds of wall time one crate adds to a CI test job (compile plus nextest run), used by
`cargo xtask check --format json` (`xtask/src/report.rs`) to balance the test jobs. Crates not listed weigh `default`.

How the numbers were derived, per crate, from the test-job logs of merge-queue run 38596279345 (all six test jobs, plus
run 38589415493 for `omuzaki-char`'s per-binary times):

    weight = serial_test_seconds / 3 + 4 * test_binaries            (minimum 8)
    omuzaki-ui and omuzaki-cli: + 150, their measured extra compile time

- `serial_test_seconds`: seconds between the crate's first `Running ...` line and the end of its last test binary
  (doctests included), summed from log timestamps. `omuzaki-char` is 1361 s there. Divided by 3 because nextest runs
  tests in parallel on the 4-vCPU runner (the measured speedup of the first nextest run was 3 to 4).
- `test_binaries`: the number of `Running` lines for the crate. Each one is a separate link step, which is why
  compile time, not test time, dominated the first nextest run for crates with many small test binaries.

The weights are estimates. Refresh them from a full-workspace run when the per-job durations drift apart.
