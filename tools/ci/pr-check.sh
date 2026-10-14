#!/usr/bin/env bash
# The per-PR gate, run on a GitHub-hosted runner (.github/workflows/pr.yml). Also runnable by hand:
#
#   tools/ci/pr-check.sh origin/main
#
# Two modes. When `cargo xtask check --affected --list` works (xtask_affected in lib.sh), xtask runs fmt, clippy, test,
# deny and lint-deps on the affected crates only. Until that lands, or if xtask itself is broken, the same
# gates run on the whole workspace: always correct, only slower. The summary says which mode ran. Then the affected
# crates are cross-built for Windows and checked for WebAssembly (spec v3 section 34).
set -uo pipefail

CI_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh source-path=SCRIPTDIR
source "$CI_DIR/lib.sh"

base=${1:?usage: pr-check.sh <base-ref>   e.g. origin/main}

ci_begin "PR checks"
ci_note "Commit" "$(git rev-parse --short HEAD)"
ci_note "Base" "$base"
ci_note "Runner" "${RUNNER_NAME:-local}"

stage "Cargo.lock is current" lockfile_current

if affected_list=$(xtask_affected "$base"); then
  ci_note "Mode" "cargo xtask check --affected (affected crates only)"
  # xtask prints one crate per line, and nothing at all when a change touches no crate (a docs-only PR).
  mapfile -t affected < <(sed '/^[[:space:]]*$/d' <<<"$affected_list")
  stage "xtask check --affected" cargo xtask check --affected --base "$base"
else
  ci_note "Mode" "whole workspace (xtask check is not available yet)"
  mapfile -t affected < <(python3 -I "$CI_DIR/crates.py" members)
  stage "fmt" cargo fmt --all --check
  stage "clippy" cargo clippy --locked --workspace --all-targets -- -D warnings
  stage "test" cargo test --locked --workspace
  stage "deny (licences, bans, sources)" cargo deny check licenses bans sources
fi

# The shell tests in tools/tests/.
for shell_test in "$CI_DIR"/../tests/*.sh; do
  stage "shell test $(basename "$shell_test")" bash "$shell_test"
done

# Every golden image the manifests name is in the store and undamaged. The golden store is an external asset store that
# hosted runners don't have, so this stage skips there, with the reason shown in the summary.
if golden_reason=$(golden_skip_reason); then
  skip "golden verify" "$golden_reason"
else
  stage "golden verify" golden_verify
fi

ci_note "Affected crates" "${affected[*]:-none}"

if ((${#affected[@]} == 0)); then
  skip "Windows cross-build" "no crate affected"
  skip "WebAssembly check" "no crate affected"
else
  mapfile -t windows_flags < <(printf -- '-p\n%s\n' "${affected[@]}")
  stage "Windows cross-build" cargo build --locked --target x86_64-pc-windows-gnu "${windows_flags[@]}"

  mapfile -t wasm_crates < <(python3 -I "$CI_DIR/crates.py" wasm "${affected[@]}")
  if [[ -z ${wasm_crates[0]:-} ]]; then
    skip "WebAssembly check" "no affected crate targets wasm"
  else
    mapfile -t wasm_flags < <(printf -- '-p\n%s\n' "${wasm_crates[@]}")
    stage "WebAssembly check" cargo check --locked --target wasm32-unknown-unknown "${wasm_flags[@]}"
  fi
fi

ci_finish
