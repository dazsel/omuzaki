#!/usr/bin/env bash
# Tests for tools/ci/gate-aggregate.py and the workflow_dispatch case of tools/ci/plan-outputs.py: what `gate` accepts
# for a pull request, the merge queue and a hosted-tests dispatch. Run: bash tools/tests/gate_aggregate.sh.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
GATE="$TESTS_DIR/../ci/gate-aggregate.py"
PLAN="$TESTS_DIR/../ci/plan-outputs.py"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# needs PLAN LINT TEST SELFGATE HAS_TESTS [SKIP]: a toJSON(needs) document with those job results.
needs() {
    python3 -I -c 'import json,sys; p,l,t,s,h,k=(sys.argv[1:]+[""])[:6]
print(json.dumps({"plan":{"result":p,"outputs":{"has_tests":h,"skip":k}},"lint":{"result":l,"outputs":{}},
 "test":{"result":t,"outputs":{}},"link-gate":{"result":"skipped","outputs":{}},"win-cross":{"result":"skipped","outputs":{}},
 "self-gate":{"result":s,"outputs":{}}}))' "$@"
}
# gate EVENT HOSTED NEEDS: the gate's exit code.
gate() { NEEDS=$3 EVENT=$1 HOSTED=$2 python3 -I "$GATE" >/dev/null 2>&1; }
expect_pass() { if gate "$2" "$3" "$4"; then pass "$1"; else fail "$1: wanted pass"; fi; }
expect_red() { if gate "$2" "$3" "$4"; then fail "$1: wanted fail"; else pass "$1"; fi; }

D=workflow_dispatch
expect_pass "hosted: tests ran and passed" $D true "$(needs success success success skipped true)"
expect_pass "hosted: an empty plan (no tests) passes" $D true "$(needs success success skipped skipped false)"
expect_red "hosted: input off fails, though everything is skipped or green" $D false "$(needs success success success skipped true)"
expect_red "hosted: a skipped plan fails" $D true "$(needs skipped skipped skipped skipped false)"
expect_red "hosted: a failed plan fails" $D true "$(needs failure skipped skipped skipped false)"
expect_red "hosted: skipped tests with a nonempty plan fail" $D true "$(needs success success skipped skipped true)"
expect_red "hosted: a failed test fails" $D true "$(needs success success failure skipped true)"
expect_red "hosted: a cancelled test fails" $D true "$(needs success success cancelled skipped true)"
expect_red "hosted: a failed lint fails" $D true "$(needs success failure success skipped true)"
expect_red "hosted: lint skipped after a good plan fails" $D true "$(needs success skipped success skipped true)"

# The other events keep their rules.
expect_pass "PR: self-gate success passes without tests" pull_request false "$(needs success success skipped success false)"
expect_red "PR: a failed self-gate fails" pull_request false "$(needs success success skipped failure false)"
expect_red "PR: a skipped self-gate fails" pull_request false "$(needs success success skipped skipped false)"
expect_pass "PR: a draft (everything skipped) passes" pull_request false "$(needs skipped skipped skipped skipped false)"
expect_pass "PR: a push of only a main merge skips lint" pull_request false "$(needs success skipped skipped success false true)"
expect_pass "queue: tests passed" merge_group false "$(needs success success success skipped true)"
expect_red "queue: a failed test fails" merge_group false "$(needs success success failure skipped true)"

# Planning for a dispatch: like the queue (tests and lint on everything affected), with the base falling back to main.
cat >"$work/leaf.json" <<'J'
{"mode":"crates","trigger":null,"empty":false,"changed":["a-api"],"direct":["a-api","a-impl"],
 "full":["a-api","a-impl","a-app"],"link_only":["a-cli"],
 "shards_changed":[["a-api"]],"shards_full":[["a-api","a-app"],["a-impl"]]}
J
cat >"$work/docs.json" <<'J'
{"mode":"crates","trigger":null,"empty":true,"changed":[],"direct":[],"full":[],"link_only":[],
 "shards_changed":[],"shards_full":[]}
J
out=$(python3 -I "$PLAN" "$work/leaf.json" workflow_dispatch)
[[ $out == *'has_tests=true'* && $out == *'"crates":"a-api a-app"'* ]] && pass "dispatch plans the affected test shards" || fail "dispatch test plan"
[[ $out == *'lint_crates=a-api a-app a-cli a-impl'* ]] && pass "dispatch lints everything affected" || fail "dispatch lint plan"
out=$(python3 -I "$PLAN" "$work/docs.json" workflow_dispatch)
[[ $out == *'has_tests=false'* ]] && pass "a docs-only dispatch has no tests" || fail "dispatch empty plan"

exit $((failures > 0))
