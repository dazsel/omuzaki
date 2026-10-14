#!/usr/bin/env bash
# Tests for tools/dispatch-check.py, with a stub `gh` that prints fixture JSON, so GitHub is never reached.
# Run: bash tools/tests/dispatch_check.sh. Exit 0 when every case passes, 1 otherwise.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CHECK="$TESTS_DIR/../dispatch-check.py"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# The stub: answers `gh issue view N` and `gh pr list` from $DISPATCH_FIXTURES, and refuses anything else.
mkdir -p "$work/bin"
cat >"$work/bin/gh" <<'S'
#!/usr/bin/env bash
case "$1 $2" in
  "issue view") cat "$DISPATCH_FIXTURES/issue-$3.json" ;;
  "pr list") cat "$DISPATCH_FIXTURES/prs.json" ;;
  *) echo "stub gh: unexpected call: $*" >&2; exit 3 ;;
esac
S
chmod +x "$work/bin/gh"

# scenario NAME: start a fresh fixture directory for one case.
scenario() { FX="$work/$1"; mkdir -p "$FX"; }
# issue N: the issue body from stdin, encoded as gh prints it.
issue() {
  python3 -I -c 'import json,sys; print(json.dumps({"number": int(sys.argv[1]), "title": "t", "body": sys.stdin.read()}))' "$1" >"$FX/issue-$1.json"
}
# run ARGS...: the check against the current scenario, with the stub first on PATH. Sets $out and $code.
run() {
  out=$(PATH="$work/bin:$PATH" DISPATCH_FIXTURES="$FX" timeout 120 python3 -I "$CHECK" "$@" 2>"$work/err")
  code=$?
}
# expect_line TEXT DESCRIPTION: the output has a line exactly TEXT.
expect_line() {
  if grep -qxF -- "$1" <<<"$out"; then pass "$2"; else fail "$2: no line '$1' in: $out $(cat "$work/err")"; fi
}
expect_code() {
  if [[ $code == "$1" ]]; then pass "$2"; else fail "$2: exit $code, wanted $1 ($(cat "$work/err"))"; fi
}

# go: a fence nothing open touches; a draft that touches it doesn't count.
scenario go
issue 9001 <<'B'
**Parent:** #9000

**Fence:**
- `crates/omuzaki-io-ops/src/{a,b}.rs`
- `tools/x.py`: the script

**Acceptance:** green
B
cat >"$FX/prs.json" <<'J'
[{"number":601,"headRefName":"cloud/601-audio","isDraft":false,"createdAt":"2026-10-01T10:00:00Z",
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"body":"Closes #9100"},
 {"number":602,"headRefName":"local/602-x","isDraft":true,"createdAt":"2026-10-02T10:00:00Z",
  "files":[{"path":"tools/x.py"}],"body":""}]
J
run 9001
expect_line "go #9001" "go: a free fence is go"
expect_code 0 "go: exit 0"

# overlap: an open PR changes a file in the fence, and nothing chains it.
scenario overlap
issue 9002 <<'B'
**Fence:**
- `crates/omuzaki-ops/src/a.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":603,"headRefName":"local/603-ops","isDraft":false,"createdAt":"2026-10-03T10:00:00Z",
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"body":"Closes #9101"}]
J
run 9002
expect_line "wait #9002: overlap with #603 (crates/omuzaki-ops/src/a.rs)" "overlap: waits and names the PR"
expect_code 1 "overlap: exit 1"

# chained overlap: the task is blocked by the issue that PR closes, so the overlap is allowed.
scenario chained
issue 9003 <<'B'
**Blocked by:** #9004

**Fence:**
- `crates/omuzaki-ops/src/a.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":604,"headRefName":"cloud/604-ops","isDraft":false,"createdAt":"2026-10-04T10:00:00Z",
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"body":"Fixes #9004"}]
J
run 9003
expect_line "go #9003" "chained: an overlap with the blocking work is go"
expect_code 0 "chained: exit 0"

# own PR: the PR that closes this task is not a conflict with it.
scenario own
issue 9005 <<'B'
**Fence:** `tools/y.py`
B
cat >"$FX/prs.json" <<'J'
[{"number":605,"headRefName":"port/605-y","isDraft":false,"createdAt":"2026-10-05T10:00:00Z",
  "files":[{"path":"tools/y.py"}],"body":"Closes #9005"}]
J
run 9005
expect_line "go #9005" "own PR: the task's own PR is go"

# crate cap: three open PRs in the fence's crate wait; a higher cap lets it go.
scenario crate
issue 9006 <<'B'
**Fence:** `crates/omuzaki-store/src/a.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":611,"headRefName":"cloud/611","isDraft":false,"createdAt":"2026-10-06T10:00:00Z","files":[{"path":"crates/omuzaki-store/src/b.rs"}],"body":""},
 {"number":612,"headRefName":"cloud/612","isDraft":false,"createdAt":"2026-10-06T11:00:00Z","files":[{"path":"crates/omuzaki-store/src/c.rs"}],"body":""},
 {"number":613,"headRefName":"local/613","isDraft":false,"createdAt":"2026-10-06T12:00:00Z","files":[{"path":"crates/omuzaki-store/src/d.rs"}],"body":""}]
J
run 9006
expect_line "wait #9006: crate cap: omuzaki-store has 3 open PRs (#611, #612, #613) (cap 3)" "crate cap: waits at 3"
run 9006 --crate-cap 4
expect_line "go #9006" "crate cap: a higher --crate-cap lets it go"

# total cap: 40 open PRs wait; --total-cap 41 lets it go.
scenario total
issue 9007 <<'B'
**Fence:** `tools/z.py`
B
python3 -I -c '
import json
print(json.dumps([{"number": 700 + i, "headRefName": f"cloud/{700 + i}", "isDraft": False,
                   "createdAt": "2026-10-07T10:00:00Z", "files": [{"path": f"docs/d-{i}.md"}], "body": ""}
                  for i in range(40)]))' >"$FX/prs.json"
run 9007
expect_line "wait #9007: total cap: 40 open PRs (cap 40)" "total cap: waits at 40"
run 9007 --total-cap 41
expect_line "go #9007" "total cap: a higher --total-cap lets it go"

# no fence: prose without a Fence section waits.
scenario nofence
issue 9008 <<'B'
Just prose, with no section for the fence.
B
echo '[]' >"$FX/prs.json"
run 9008
expect_line "wait #9008: no fence" "no fence: waits"

# contract first: the fence names an *-api crate that another open PR also touches.
scenario contract
issue 9009 <<'B'
**Fence:** `crates/omuzaki-ops-api/src/types.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":621,"headRefName":"cloud/621","isDraft":false,"createdAt":"2026-10-09T10:00:00Z","files":[{"path":"crates/omuzaki-ops-api/src/lib.rs"}],"body":""}]
J
run 9009
expect_line "wait #9009: contract first: omuzaki-ops-api has open PR(s) #621; a contract lands alone" "contract first: waits"

# report: lanes, crates and drafts, oldest first.
scenario report
cat >"$FX/prs.json" <<'J'
[{"number":634,"headRefName":"main-x","isDraft":true,"createdAt":"2026-10-04T10:00:00Z","files":[{"path":"docs/a.md"}],"body":""},
 {"number":633,"headRefName":"port/633","isDraft":false,"createdAt":"2026-10-03T10:00:00Z","files":[],"body":""},
 {"number":632,"headRefName":"local/632","isDraft":false,"createdAt":"2026-10-02T10:00:00Z","files":[{"path":"crates/omuzaki-git/src/b.rs"}],"body":""},
 {"number":631,"headRefName":"cloud/631","isDraft":false,"createdAt":"2026-10-01T10:00:00Z","files":[{"path":"crates/omuzaki-git/src/a.rs"}],"body":""}]
J
run --report
expect_code 0 "report: exit 0"
expect_line "open pull requests: 4 (3 ready, 1 draft)" "report: counts"
expect_line "by lane: cloud 1, local 1, port 1, other 1" "report: lanes"
expect_line "  omuzaki-git: 2 (#631, #632)" "report: crates"
expect_line "  (no crate): 2 (#633, #634)" "report: PRs without a crate"
if [[ $(sed -n '/^oldest first:/{n;p}' <<<"$out") == "  #631 cloud/631 2026-10-01  crates: omuzaki-git" ]]; then
  pass "report: oldest first"
else
  fail "report: oldest first is wrong: $out"
fi

# JSON: the same verdict for tools.
scenario json
issue 9001 <<'B'
**Fence:** `tools/x.py`
B
echo '[]' >"$FX/prs.json"
run 9001 --json
if python3 -I -c 'import json,sys; r=json.load(sys.stdin); sys.exit(0 if r == [{"issue": 9001, "go": True, "reasons": []}] else 1)' <<<"$out"; then
  pass "json: the verdict as data"
else
  fail "json: $out"
fi

# a missing fixture is a gh failure: exit 2 with a message, not a crash.
scenario missing
echo '[]' >"$FX/prs.json"
run 9999
expect_code 2 "missing issue: exit 2"

((failures == 0)) || exit 1
