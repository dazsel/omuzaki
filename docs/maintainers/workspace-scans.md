---
title: "Workspace scans in CI"
summary: "The tests that read every crate, why they run on every change, and how to add one."
area: maintainers
---

# Workspace scans in CI

Most tests check their own crate, so CI runs them only when a change reaches that crate. A few tests read the whole
workspace instead: every crate's `clippy.toml`, every manifest, every task file. A change in one crate can break one of
those tests without touching the test's crate, and the affected-crate scope would then never run it. The break would
show up only in the nightly run (#2139).

These tests are listed in `xtask/src/scans.rs` (`SCANS`). The CI plan reads the list from `cargo xtask check --format
json` (`scans`), and the `workspace scans` job runs each target on every pull request and every push to `main`, an
empty scope included. They are listed as test targets, never as whole crates, so together they stay fast.

| Package | Test target | What it checks |
|---|---|---|
| `omuzaki-ops-api` | `fs_fence` | every crate that registers ops forbids reaching files by path in its own `clippy.toml` |
| `omuzaki-cli` | `registrations` | the CLI's link gate: its registrations match those of every crate it links (ADR 0015) |
| `omuzaki-stage` | `registrations` | the stage's link gate |
| `omuzaki-test` | `eval_lint` | the eval lint over the real workspace's task files |
| `omuzaki-input` | `fences` | no other crate turns on the input test hooks, and nothing injects or hooks input (ADR 0060) |
| `omuzaki-test` | `gpu_instances` | no test, bench or example starts wgpu's GL backend |

## Adding one

A new test that reads the workspace goes into `SCANS`, or into `NOT_SCANS` with the reason it doesn't scan.
`cargo xtask lint-scans` fails on a test that reads the workspace without being in either list, and on a listed target
whose file is gone. The lint job runs it on every change.
