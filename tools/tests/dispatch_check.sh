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

# The stub: answers `gh issue view N`, `gh pr list` and `gh api` from $DISPATCH_FIXTURES, and refuses anything else.
# A fixture that is missing is a failed call (exit 1), except dependency links, which default to none.
mkdir -p "$work/bin"
cat >"$work/bin/gh" <<'S'
#!/usr/bin/env bash
case "$1 $2" in
  "issue view") cat "$DISPATCH_FIXTURES/issue-$3.json" ;;
  "pr list") cat "$DISPATCH_FIXTURES/prs.json" ;;
  "api "*)
    case "$*" in
      *"/dependencies/blocked_by"*|*"/dependencies/blocking"*)
        kind=$(sed -n 's#.*/dependencies/\([a-z_]*\).*#\1#p' <<<"$*")
        number=$(sed -n 's#.*/issues/\([0-9]*\)/dependencies/.*#\1#p' <<<"$*")
        cat "$DISPATCH_FIXTURES/deps-$kind-$number.json" 2>/dev/null || echo '[]'
        ;;
      *"/pulls/"*"/files"*)
        number=$(sed -n 's#.*/pulls/\([0-9]*\)/files.*#\1#p' <<<"$*")
        cat "$DISPATCH_FIXTURES/files-$number.json" || exit 1
        ;;
      *"/contents/crates"*) cat "$DISPATCH_FIXTURES/crates.json" || exit 1 ;;
      *) echo "stub gh: unexpected api call: $*" >&2; exit 3 ;;
    esac
    ;;
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

# native links, blocked by: this task is blocked by 9004 (a native link, no body line), and PR 606 closes 9004.
scenario native-blocked-by
issue 9010 <<'B'
**Fence:** `crates/omuzaki-ops/src/a.rs`
B
echo '[{"number":9004}]' >"$FX/deps-blocked_by-9010.json"
cat >"$FX/prs.json" <<'J'
[{"number":606,"headRefName":"cloud/606-ops","isDraft":false,"createdAt":"2026-10-04T11:00:00Z",
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"body":"Closes #9004"}]
J
run 9010
expect_line "go #9010" "native blocked-by: an overlap with the blocker's PR is go"
expect_code 0 "native blocked-by: exit 0"

# native links, blocking: this task blocks 9012, and PR 607 closes 9012, so the overlap is chained.
scenario native-blocking
issue 9011 <<'B'
**Fence:** `crates/omuzaki-ops/src/a.rs`
B
echo '[{"number":9012}]' >"$FX/deps-blocking-9011.json"
cat >"$FX/prs.json" <<'J'
[{"number":607,"headRefName":"cloud/607-ops","isDraft":false,"createdAt":"2026-10-04T12:00:00Z",
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"body":"Closes #9012"}]
J
run 9011
expect_line "go #9011" "native blocking: an overlap with the PR for the issue this task blocks is go"
expect_code 0 "native blocking: exit 0"

# fence parsing: entries on one line, bullets quoted and bare mixed, and brace groups kept whole.
scenario one-line
issue 9020 <<'B'
**Fence:** tools/x.py, tools/y.py
B
cat >"$FX/prs.json" <<'J'
[{"number":641,"headRefName":"cloud/641","isDraft":false,"createdAt":"2026-10-10T10:00:00Z","files":[{"path":"tools/y.py"}],"body":""}]
J
run 9020
expect_line "wait #9020: overlap with #641 (tools/y.py)" "fence: comma-separated entries on one line are each read"

scenario mixed
issue 9021 <<'B'
**Fence:**
- `tools/a.py`
- tools/b.py: the other one
B
cat >"$FX/prs.json" <<'J'
[{"number":642,"headRefName":"cloud/642","isDraft":false,"createdAt":"2026-10-10T11:00:00Z","files":[{"path":"tools/b.py"}],"body":""}]
J
run 9021
expect_line "wait #9021: overlap with #642 (tools/b.py)" "fence: a bare bullet beside a quoted one is still read"

scenario braces
issue 9022 <<'B'
**Fence:**
- `crates/{omuzaki-io-ops,omuzaki-ops}/src/a.rs`
- `crates/omuzaki-git/{src/a.rs,tests/b.rs}`
B
cat >"$FX/prs.json" <<'J'
[{"number":643,"headRefName":"cloud/643","isDraft":false,"createdAt":"2026-10-10T12:00:00Z","files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"body":""},
 {"number":644,"headRefName":"cloud/644","isDraft":false,"createdAt":"2026-10-10T13:00:00Z","files":[{"path":"crates/omuzaki-git/tests/b.rs"}],"body":""}]
J
run 9022
expect_line "wait #9022: overlap with #643 (crates/omuzaki-ops/src/a.rs); overlap with #644 (crates/omuzaki-git/tests/b.rs)" "fence: brace groups expand and commas inside them do not split"

# wildcard crate: crates/*-api/... names every matching folder, so contract-first applies to them.
scenario wildcard
issue 9023 <<'B'
**Fence:** `crates/*-api/src/types.rs`
B
cat >"$FX/crates.json" <<'J'
[{"name":"omuzaki-ops-api","type":"dir"},{"name":"omuzaki-core","type":"dir"},{"name":"README.md","type":"file"}]
J
cat >"$FX/prs.json" <<'J'
[{"number":645,"headRefName":"cloud/645","isDraft":false,"createdAt":"2026-10-10T14:00:00Z","files":[{"path":"crates/omuzaki-ops-api/src/lib.rs"}],"body":""}]
J
run 9023
expect_line "wait #9023: contract first: omuzaki-ops-api has open PR(s) #645; a contract lands alone" "wildcard: names the matching crate for contract first"

# wildcard crate, crates/ unreadable: the task waits rather than assume no crate matches.
scenario wildcard-unreadable
issue 9024 <<'B'
**Fence:** `crates/*-api/src/types.rs`
B
echo '[]' >"$FX/prs.json"
run 9024
expect_line "wait #9024: cannot resolve crates/*-api/src/types.rs: the crates/ folder could not be read" "wildcard: waits when crates/ cannot be read"

# truncated file list: the PR has 101 files, gh lists 100, and the REST list (two pages) has the 101st in the fence.
scenario truncated
issue 9025 <<'B'
**Fence:** `tools/t.py`
B
python3 -I -c '
import json
listed = [{"path": f"docs/f-{i}.md"} for i in range(100)]
print(json.dumps([{"number": 646, "headRefName": "cloud/646", "isDraft": False, "createdAt": "2026-10-10T15:00:00Z",
                   "files": listed, "changedFiles": 101, "body": ""}]))' >"$FX/prs.json"
python3 -I -c '
import json
page1 = [{"filename": f"docs/f-{i}.md"} for i in range(100)]
page2 = [{"filename": "tools/t.py"}]
print(json.dumps(page1) + json.dumps(page2))' >"$FX/files-646.json"
run 9025
expect_line "wait #9025: overlap with #646 (tools/t.py)" "truncated: the full file list is read from the API, pages included"

# truncated file list, unreadable: every task waits rather than assume no overlap.
scenario truncated-unreadable
issue 9026 <<'B'
**Fence:** `tools/t.py`
B
python3 -I -c '
import json
print(json.dumps([{"number": 647, "headRefName": "cloud/647", "isDraft": False, "createdAt": "2026-10-10T16:00:00Z",
                   "files": [{"path": f"docs/f-{i}.md"} for i in range(100)], "changedFiles": 150, "body": ""}]))' >"$FX/prs.json"
run 9026
expect_line "wait #9026: cannot read the changed files of #647; waiting until they can be read" "truncated: waits when the full list cannot be read"

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
