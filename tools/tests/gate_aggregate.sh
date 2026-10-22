#!/usr/bin/env bash
# Tests for tools/ci/gate-aggregate.py: what the `gate` summary counts as green. Run: bash tools/tests/gate_aggregate.sh.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
GATE="$TESTS_DIR/../ci/gate-aggregate.py"

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# needs PLAN LINT TEST: a toJSON(needs) document with those job results.
needs() {
    python3 -I -c 'import json,sys; p,l,t=sys.argv[1:4]
print(json.dumps({"plan":{"result":p,"outputs":{}},"lint":{"result":l,"outputs":{}},"test":{"result":t,"outputs":{}},
 "link-gate":{"result":"skipped","outputs":{}},"win-cross":{"result":"skipped","outputs":{}}}))' "$@"
}
gate() { NEEDS=$1 python3 -I "$GATE" >/dev/null 2>&1; }
expect_pass() { if gate "$2"; then pass "$1"; else fail "$1: wanted pass"; fi; }
expect_red() { if gate "$2"; then fail "$1: wanted fail"; else pass "$1"; fi; }

expect_pass "tests ran and passed" "$(needs success success success)"
expect_pass "an empty plan (no tests) passes" "$(needs success success skipped)"
expect_pass "a draft (everything skipped) passes" "$(needs skipped skipped skipped)"
expect_red "a failed test fails" "$(needs success success failure)"
expect_red "a cancelled test fails" "$(needs success success cancelled)"
expect_red "a failed lint fails" "$(needs success failure success)"
expect_red "lint skipped after a good plan fails" "$(needs success skipped success)"
expect_red "a failed plan fails" "$(needs failure skipped skipped)"

exit $((failures > 0))
