# xtask: the workspace-scan tests run on every change

- `cargo xtask check` now runs the tests that scan the whole workspace (the file fence, the link gates, the eval lint)
  whatever crates changed, and `lint-scans` fails when a new test reads the workspace without being listed (#1899).
- `check --format json` carries them in a `scans` field for the CI plan.
