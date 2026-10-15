#!/usr/bin/env bash
# Tests for tools/ci/nextest-args.py: how a shard item becomes nextest arguments.
# Run: bash tools/tests/nextest_args.sh. Exit 0 when every case passes, 1 otherwise.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ARGS="$TESTS_DIR/../ci/nextest-args.py"

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
  '-p|omuzaki-char|-E|binary(=palette_independence)|--no-tests=pass' "an isolated binary runs alone and passes if renamed away"

refuses 'omuzaki-char' "a whole crate is not a shard item (the job runs those in one call)"
refuses 'c@5/4' "a partition beyond the count"
refuses 'c@0/4' "partitions count from 1"
refuses "c@1/2!a'; rm -rf /" "a label that is not a label"
refuses 'c@=' "an isolated item without a binary"

if ((failures > 0)); then echo "$failures failed" >&2; exit 1; fi
