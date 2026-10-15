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
expect test_matrix '{"include":[]}' "a PR runs no tests: its session attests them"
expect has_tests false "a PR has no test job"
expect lint_crates 'a-api a-impl' "a PR lints the changed crate and its direct dependents"
expect link_crates 'a-cli' "a PR still runs the link gates"
expect cross_crates 'a-api a-app a-cli a-impl' "the cross-build set is everything affected"

out=$(python3 -I "$PLAN" "$work/leaf.json" merge_group)
expect test_matrix '{"include":[{"name":"1","crates":"a-api a-app","parts":""},{"name":"2","crates":"a-impl","parts":""}]}' "the queue tests everything affected, sharded"
expect lint_crates 'a-api a-app a-cli a-impl' "the queue lints everything affected, link-only crates included"

cat >"$work/split.json" <<'J'
{"mode":"workspace","trigger":"Cargo.lock","empty":false,"changed":[],"direct":[],"full":["a-api","a-big"],"link_only":[],
 "shards_changed":[],"shards_full":[["a-api","a-big@1/2"],["a-big@2/2"]]}
J
out=$(python3 -I "$PLAN" "$work/split.json" merge_group)
expect test_matrix '{"include":[{"name":"1","crates":"a-api","parts":"a-big@1/2"},{"name":"2","crates":"","parts":"a-big@2/2"}]}' "a partitioned crate keeps its partitions apart from whole crates"

expect has_tests true "the queue runs the test matrix"
out=$(python3 -I "$PLAN" "$work/split.json" pull_request)
expect has_tests false "a workspace-wide PR still runs no tests"

out=$(python3 -I "$PLAN" "$work/docs.json" merge_group)
expect has_tests false "a docs-only change has no tests"
expect has_cross false "a docs-only change builds nothing"
expect empty true "a docs-only change is empty"

# A change to the op contract reaches the CLI in full scope, with no link-only crate: the link-gate job still runs
# the CLI's product configuration, on a PR and in the queue (#1632).
cat >"$work/ops-api.json" <<'J'
{"mode":"crates","trigger":null,"empty":false,"changed":["omuzaki-ops-api"],
 "direct":["omuzaki-ops-api","omuzaki-cli"],"full":["omuzaki-cli","omuzaki-ops-api"],"link_only":[],
 "shards_changed":[["omuzaki-ops-api"]],"shards_full":[["omuzaki-cli","omuzaki-ops-api"]]}
J
for event in pull_request merge_group; do
  out=$(python3 -I "$PLAN" "$work/ops-api.json" "$event")
  expect cli_in_scope true "$event: the CLI in full scope runs its product configuration"
  expect has_link_gates false "$event: no link-only crate, so no other link gate"
done
out=$(python3 -I "$PLAN" "$work/leaf.json" pull_request)
expect cli_in_scope false "a change that doesn't reach the CLI skips its product configuration"
cat >"$work/cli-link-only.json" <<'J'
{"mode":"crates","trigger":null,"empty":false,"changed":["omuzaki-io-ops"],"direct":["omuzaki-io-ops"],
 "full":["omuzaki-io-ops"],"link_only":["omuzaki-cli"],"shards_changed":[["omuzaki-io-ops"]],
 "shards_full":[["omuzaki-io-ops"]]}
J
out=$(python3 -I "$PLAN" "$work/cli-link-only.json" pull_request)
expect cli_in_scope true "the CLI reached only through a link-only dependency runs it too"

python3 -I "$PLAN" >/dev/null 2>&1 && fail "missing arguments should be an error" || pass "missing arguments are an error"
exit $((failures > 0))
