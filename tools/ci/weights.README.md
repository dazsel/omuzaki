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

Two measured corrections after the first full-workspace runs on this branch:

- `omuzaki-char` is 1100, not the 582 the formula gives, and is pinned to 4 partitions (`parts`). Hash partitioning is a pure function of the test name: 2 partitions put both slow tests in one job (14 minutes), 4 separate them (a partition's tests took 60 to 295 s). Its slowest single tests take 287 s and 264 s
  (`cloth_contact::a_million_random_steps_produce_no_nan`, `body_shape::every_corner_at_the_slider_extremes...`), so a
  partition can never finish in under about 5 minutes plus compile; it must not share a job with much else.
- `omuzaki-ui` is 400, not 158: a job holding it spent 7 minutes compiling (the egui/wgpu test dependencies are not in
  the shared cache), against 2 to 3 minutes for jobs without it.

- `part_weights` gives the four `omuzaki-char` partitions their measured wall seconds (65 s compile plus the partition's
  test time in a full run: 390, 144, 201, 107 s). The first holds `cloth_contact::a_million_random_steps_produce_no_nan`
  (287 to 419 s, one thread), so it should get a job almost to itself. They sum to the crate's 1100.

The weights are estimates. Refresh them from a full-workspace run when the per-job durations drift apart.
