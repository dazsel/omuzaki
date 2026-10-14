#!/usr/bin/env bash
# Tests for tools/ci/plan-outputs.py: what a pull request and the merge queue run for the same change.
# Run: bash tools/tests/plan_outputs.sh. Exit 0 when every case passes, 1 otherwise.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PLAN="$TESTS_DIR/../ci/plan-outputs.py"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }
# value KEY: the value of KEY in the last run's output.
value() { sed -n "s/^$1=//p" <<<"$out"; }
expect() { if [[ $(value "$1") == "$2" ]]; then pass "$3"; else fail "$3: $1 was '$(value "$1")', wanted '$2'"; fi; }

cat >"$work/leaf.json" <<'J'
{"mode":"crates","trigger":null,"empty":false,"changed":["a-api"],"direct":["a-api","a-impl"],
 "full":["a-api","a-impl","a-app"],"link_only":["a-cli"],
 "shards_changed":[["a-api"]],"shards_full":[["a-api","a-app"],["a-impl"]]}
J
cat >"$work/docs.json" <<'J'
{"mode":"crates","trigger":null,"empty":true,"changed":[],"direct":[],"full":[],"link_only":[],
 "shards_changed":[],"shards_full":[]}
J

out=$(python3 -I "$PLAN" "$work/leaf.json" pull_request)
expect test_matrix '{"include":[{"name":"1","crates":"a-api"}]}' "a PR tests only the changed crate"
expect lint_crates 'a-api a-impl' "a PR lints the changed crate and its direct dependents"
expect link_crates 'a-cli' "a PR still runs the link gates"
expect cross_crates 'a-api a-app a-cli a-impl' "the cross-build set is everything affected"

out=$(python3 -I "$PLAN" "$work/leaf.json" merge_group)
expect test_matrix '{"include":[{"name":"1","crates":"a-api a-app"},{"name":"2","crates":"a-impl"}]}' "the queue tests everything affected, sharded"
expect lint_crates 'a-api a-app a-cli a-impl' "the queue lints everything affected, link-only crates included"

out=$(python3 -I "$PLAN" "$work/docs.json" merge_group)
expect has_tests false "a docs-only change has no tests"
expect has_cross false "a docs-only change builds nothing"
expect empty true "a docs-only change is empty"

python3 -I "$PLAN" >/dev/null 2>&1 && fail "missing arguments should be an error" || pass "missing arguments are an error"
exit $((failures > 0))
