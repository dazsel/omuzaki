---
title: "0062: In large crates, the open-PR cap counts per top-level module (2026-10-15)"
summary: "In large crates, the open-PR cap counts per top-level module"
area: decisions
---

# 0062: In large crates, the open-PR cap counts per top-level module (2026-10-15)

**Status:** accepted (2026-10-15). **Supersedes in part:** ADR 0053 §1.1, the per-crate cap, for the crates named
below. **Context:** ADR 0053 (limits on work in progress), ADR 0054 (generated module lists); ruling on #2,
2026-10-15.

## Context

ADR 0053 caps open pull requests at 3 per crate, because a crate's shared files (its module list above all) made one
crate one unit of conflict. ADR 0054's generated module lists removed that file. `omuzaki-char` and `omuzaki-face` are
large crates whose top-level modules (`hair/`, `head/`, `body/`, `cloth/`, `painter/` and so on) are independent.
Most of their ready work sits in disjoint modules. At 3 per crate, `omuzaki-char` merges about 2 to 3 pull requests an
hour, which puts a 4 to 6 hour tail on independent work.

## Decision

1. **In `omuzaki-char` and `omuzaki-face`, the cap of 3 counts per top-level module**, with a ceiling of 12 open pull
   requests for the crate. A top-level module is a folder `src/<module>/` or a file `src/<module>.rs` (such as
   `omuzaki-char/src/builder.rs` or `omuzaki-face/src/layer.rs`); each is one bucket, apart from `lib.rs`, which is
   shared (§3).
2. **Nested modules, tests, benches and examples count against their top-level module.** `src/painter/brows.rs`
   counts against `painter/`, and so does its test `tests/brows.rs`; `tests/hair_pipeline.rs` counts against `hair/`.
   A test, bench or example counts against the top-level module whose code it exercises, whatever it is named. That
   includes a module's own test folder and its aggregator, such as `tests/cloth_solver/` (with its `main.rs`) for
   `cloth/` and `tests/head_quality/` for `head/`.
3. **Everything else takes the crate's one shared slot.** Any file that belongs to no single module is shared: every
   file at the crate root (`Cargo.toml`, `build.rs`, `clippy.toml`, `evidence.toml` and any added later), `lib.rs`,
   shared test infrastructure (`tests/common/`, `tests/fixtures/`, an aggregator across modules), and data or tests
   that several modules use. A pull request that changes any of them takes the shared slot.
4. **The file-overlap check at dispatch still applies** (`tools/dispatch-check.py`). Two pull requests that touch one
   file never run at once, whatever the counts say.
5. Every other crate keeps the per-crate cap (5 since 2026-10-15, `operating.md`), and this decision leaves the
   total cap (150) unchanged. Adding a crate to this list is a Director ruling on #2.

## Consequences

- `docs/maintainers/operating.md` and `docs/maintainers/cloud-lane.md` state the new rule.
- `tools/dispatch-check.py` learns per-module counting (#1743). Until it lands, a task whose fence lies only in these
  two crates is checked with the crate ceiling, `tools/dispatch-check.py --crate-cap 12 <issue>`, which still checks
  file overlap and the total, and the lead counts modules by hand. `--crate-cap` applies to every crate in the fence,
  so for a task whose fence also names another crate, the lead reads each crate's count from the right run: the
  default run's count for the other crate (cap 5), and the `--crate-cap 12` run's verdict for these two. The default
  run's verdict on `omuzaki-char` or `omuzaki-face` is ignored. The overlap and total checks are the same in both
  runs.
- Rejected:
  - raising the per-crate cap for every crate, since small crates still share their files;
  - counting per file only, since review load per area still needs a bound.
