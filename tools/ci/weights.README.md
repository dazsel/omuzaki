# weights.json

Estimated seconds of wall time one crate adds to a CI test job (compile of the workspace crates plus nextest run, with
the dependency cache warm), used by `cargo xtask check --format json` (`xtask/src/report.rs`) to balance the test jobs.
Crates not listed weigh `default`.

## Current numbers (2026-10-15, issue #2118)

Measured on the six test jobs of merge-queue run 38810711080 (24 affected crates). For every `cargo nextest run` call:
seconds from the first workspace crate's `Compiling` line to `Starting N tests` (the compile that stays when the
dependency cache is warm, links included) plus the `Summary` time (the run).

| Group (one call)                                         | compile | run | weight |
| -------------------------------------------------------- | ------: | --: | -----: |
| audio-api, identity-mock, ops-api, script                |      59 |   5 |     64 |
| face, history-api, io-ops, ops, settings-api, xtask      |     189 |  38 |    227 |
| mcp, ops-mock, test, test-face                           |     243 |   9 |    252 |
| anim, history-mock, link-probe, stage, take-api          |     231 | 137 |    368 |
| cli, library-api, test-char                              |     272 |   3 |    275 |
| `omuzaki-ui` (two calls before: 39 + 15 s and 43 + 5 s)  |      40 |  20 |     60 |
| `omuzaki-char` partitions 1/4, 2/4, 3/4, 4/4 (one call each) | 66, 76, 36, 80 | 395, 663, 173, 345 | |

A group's weight is split over its crates in proportion to the old estimate. Crates that run did not appear in this run
keep the old estimate scaled by the same factor as the groups together (1.65), and `default` too.

- `omuzaki-ui` is 60 and pinned to one part (`parts`). Its tests take 20 s; each extra call recompiled it for 4.8 to
  5.1 minutes (no dependency cache, see `.github/workflows/ci.yml`), so splitting it cost a shard 5 minutes to save 5
  seconds.
- `omuzaki-char::palette_independence` is `isolated`: three tests in one binary took 419, 450 and 466 s wherever they
  landed (partition 2/4 here), against 60 to 175 s for the next slowest. As its own item (80 s compile plus 466 s) it
  fills one shard, and the crate's four partitions skip it (`omuzaki-char@k/4!palette_independence`). nextest hashes a
  test name to its partition whatever else is filtered out, so every other test stays in the partition it had.
  Partition 2/4 without those three tests ended about 270 s after it started. `part_weights` are each partition's
  compile plus run on that basis: 465, 340, 245, 415 (sum 1465); `omuzaki-char` is 2011 = 1465 + 546.

Refresh the table from a full-workspace merge-queue run whenever the shard durations drift apart (the slowest shard more
than 20% over the mean).

The notes below are the earlier derivation, kept for the formula and the pinned-partition reasoning.

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
