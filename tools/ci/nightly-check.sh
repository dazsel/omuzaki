#!/usr/bin/env bash
# The full nightly matrix, run on a GitHub-hosted runner (.github/workflows/nightly.yml). Also runnable by hand:
#
#   tools/ci/nightly-check.sh
#
# Unlike the PR gate this never narrows to affected crates: it covers every crate, every cargo target (libs, bins, tests,
# benches, examples, doctests), the advisory database, the Windows cross-build and the WebAssembly check. It catches what
# a green PR can hide: a break in a crate nobody touched, a newly published advisory, a yanked dependency.
set -uo pipefail

CI_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh source-path=SCRIPTDIR
source "$CI_DIR/lib.sh"

ci_begin "Nightly"
ci_note "Commit" "$(git rev-parse --short HEAD)"
ci_note "Runner" "${RUNNER_NAME:-local}"

stage "Cargo.lock is current" lockfile_current
stage "fmt" cargo fmt --all --check
stage "clippy (all targets)" cargo clippy --locked --workspace --all-targets -- -D warnings
stage "test (all targets)" cargo test --locked --workspace --all-targets
stage "doctests" cargo test --locked --workspace --doc
# The link gate in release too (ADR 0015): every App's tests/registrations.rs. The PR gate runs it in dev with the tests.
stage "link gate (release)" cargo test --locked --release --workspace --test registrations
# The full check includes `advisories`, which fetches the RustSec database; PRs skip it so they stay offline-fast.
stage "deny (licences, bans, sources, advisories)" cargo deny check
stage "Windows cross-build (all targets)" cargo build --locked --workspace --all-targets --target x86_64-pc-windows-gnu
# The Windows zip, then its smoke test under Wine (the script skips with a printed reason when Wine is absent).
stage "Windows zip + Wine smoke" bash -c 'cargo xtask dist --target x86_64-pc-windows-gnu --no-models && tools/ci/wine-smoke.sh'
# omuzaki-ai's tests as a Windows build under Wine: the model cache's Windows paths (flushing through a writable handle,
# renaming onto a blob that is already there, #795) and the sandbox's Windows limits.
if command -v wine >/dev/null 2>&1; then
  stage "omuzaki-ai tests under Wine" env CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine WINEDEBUG=-all \
    cargo test --locked -p omuzaki-ai --target x86_64-pc-windows-gnu
else
  skip "omuzaki-ai tests under Wine" "wine is not installed"
fi

mapfile -t wasm_crates < <(python3 -I "$CI_DIR/crates.py" wasm)
if [[ -z ${wasm_crates[0]:-} ]]; then
  skip "WebAssembly check" "no crate targets wasm"
else
  mapfile -t wasm_flags < <(printf -- '-p\n%s\n' "${wasm_crates[@]}")
  stage "WebAssembly check" cargo check --locked --target wasm32-unknown-unknown "${wasm_flags[@]}"
fi

# lint-deps arrives with xtask; until then there is nothing to run, and a missing command must not fail CI.
if xtask_affected HEAD >/dev/null; then
  stage "lint-deps (crate dependency rules)" cargo xtask lint-deps
else
  skip "lint-deps (crate dependency rules)" "xtask lint-deps is not available yet"
fi

ci_finish
