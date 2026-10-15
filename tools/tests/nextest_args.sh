#!/usr/bin/env bash
# Tests for tools/ci/nextest-args.py: how a shard item becomes nextest arguments.
# Run: bash tools/tests/nextest_args.sh. Exit 0 when every case passes, 1 otherwise.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ARGS="$TESTS_DIR/../ci/nextest-args.py"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }
# args ITEM WANTED DESCRIPTION: the arguments for ITEM, joined by `|`, equal WANTED.
args() {
  local got
  got=$(python3 -I "$ARGS" "$1" 2>/dev/null | paste -sd'|' -)
  if [[ $got == "$2" ]]; then pass "$3"; else fail "$3: got '$got', wanted '$2'"; fi
}
# refuses ITEM DESCRIPTION: the script exits 2 and prints nothing on stdout.
refuses() {
  local out status
  out=$(python3 -I "$ARGS" "$1" 2>/dev/null); status=$?
  if [[ $status -eq 2 && -z $out ]]; then pass "$2"; else fail "$2: status $status, output '$out'"; fi
}

args 'omuzaki-char@2/4' '-p|omuzaki-char|--partition|hash:2/4' "a plain partition"
args 'omuzaki-char@1/4!palette_independence' \
  '-p|omuzaki-char|--partition|hash:1/4|-E|not binary(=palette_independence)' "a partition skips an isolated binary"
args 'c@3/5!a,b' '-p|c|--partition|hash:3/5|-E|not (binary(=a) | binary(=b))' "a partition skips several binaries"
args 'c@1/1!a' '-p|c|--partition|hash:1/1|-E|not binary(=a)' "an unsplit crate with an isolated binary is partition 1 of 1"
args 'omuzaki-char@=palette_independence' \
  '-p|omuzaki-char|-E|binary(=palette_independence)' "an isolated binary runs alone"

refuses 'omuzaki-char' "a whole crate is not a shard item (the job runs those in one call)"
refuses 'c@5/4' "a partition beyond the count"
refuses 'c@0/4' "partitions count from 1"
refuses "c@1/2!a'; rm -rf /" "a label that is not a label"
refuses 'c@=' "an isolated item without a binary"

# With the binaries nextest lists (--list), names that no longer exist drop out of the filters.
cat >"$work/binaries.json" <<'J'
{"rust-build-meta":{},"rust-binaries":{"c":{"binary-name":"c"},"c::fast":{"binary-name":"fast"},"c::other":{"binary-name":"other"}}}
J
cat >"$work/renamed.json" <<'J'
{"rust-build-meta":{},"rust-binaries":{"c":{"binary-name":"c"},"c::fast":{"binary-name":"fast"},"c::renamed":{"binary-name":"renamed"}}}
J
listed() {
  local got
  got=$(python3 -I "$ARGS" "$1" --list "$2" 2>/dev/null | paste -sd'|' -)
  if [[ $got == "$3" ]]; then pass "$4"; else fail "$4: got '$got', wanted '$3'"; fi
}
listed 'c@1/2!other' "$work/binaries.json" '-p|c|--partition|hash:1/2|-E|not binary(=other)' "a listed isolated binary is skipped"
listed 'c@1/2!other' "$work/renamed.json" '-p|c|--partition|hash:1/2' "a renamed isolated binary is no longer skipped"
listed 'c@1/2!other,fast' "$work/renamed.json" '-p|c|--partition|hash:1/2|-E|not binary(=fast)' "only the missing one drops out"
listed 'c@=other' "$work/binaries.json" '-p|c|-E|binary(=other)' "a listed isolated binary runs alone"
python3 -I "$ARGS" 'c@=other' --list "$work/renamed.json" >/dev/null 2>&1
if [[ $? -eq 10 ]]; then pass "a renamed isolated binary has nothing to run (status 10)"; else fail "a renamed isolated binary has nothing to run: status $?"; fi

# Against real nextest, when it is installed: a crate with two test binaries, one of which the plan isolates. Whatever the
# binary is called, every test runs exactly once across the isolated item and the partitions.
if command -v cargo-nextest >/dev/null 2>&1; then
  crate="$work/fixture"
  mkdir -p "$crate/tests" "$crate/src"
  printf '[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[workspace]\n' >"$crate/Cargo.toml"
  : >"$crate/src/lib.rs"
  printf '#[test]\nfn a() {}\n#[test]\nfn b() {}\n' >"$crate/tests/fast.rs"
  printf '#[test]\nfn t1() {}\n#[test]\nfn t2() {}\n#[test]\nfn t3() {}\n' >"$crate/tests/slow.rs"
  # CI sets CARGO_TERM_COLOR=always; nextest's colour codes would split the names the grep below looks for.
  nextest() { (cd "$crate" && cargo nextest "$@" --color never 2>>"$work/nextest.err"); }
  run_items() { # run_items BINARY: the tests the plan's items for isolated BINARY run, one name per line
    local binary=$1 item out rc
    nextest list --offline -p fixture --list-type binaries-only --message-format json >"$work/real.json" || return 1
    for item in "fixture@=$binary" "fixture@1/2!$binary" "fixture@2/2!$binary"; do
      rc=0; out=$(python3 -I "$ARGS" "$item" --list "$work/real.json" 2>>"$work/nextest.err") || rc=$?
      if ((rc == 10)); then continue; elif ((rc != 0)); then return 1; fi
      mapfile -t selection <<<"$out"
      nextest list --offline "${selection[@]}" --message-format oneline || return 1
    done
  }
  for binary in slow gone; do
    got=$(run_items "$binary" | grep -o 'fixture::[a-z]* [a-z0-9_]*' | sort | paste -sd',' -)
    want='fixture::fast a,fixture::fast b,fixture::slow t1,fixture::slow t2,fixture::slow t3'
    if [[ $got == "$want" ]]; then pass "real nextest: isolating '$binary' lists every test exactly once"; else fail "real nextest, '$binary': got '$got'; stderr: $(tail -n 8 "$work/nextest.err" 2>/dev/null)"; fi
  done
else
  echo "skip: cargo-nextest is not installed, so the real-nextest cases did not run"
fi

if ((failures > 0)); then echo "$failures failed" >&2; exit 1; fi
