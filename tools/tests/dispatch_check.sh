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
# A fixture that is missing is a failed call (exit 1), except dependency links, which default to none. A file named
# graphql-refused in the scenario makes the two GraphQL-backed calls fail, as the cloud proxy does with a 403.
mkdir -p "$work/bin"
cat >"$work/bin/gh" <<'S'
#!/usr/bin/env bash
refused() { [[ -e "$DISPATCH_FIXTURES/graphql-refused" ]] && { echo "HTTP 403: GraphQL refused" >&2; return 0; }; return 1; }
case "$1 $2" in
  "issue view") refused && exit 1; cat "$DISPATCH_FIXTURES/issue-$3.json" ;;
  "pr list") refused && exit 1; cat "$DISPATCH_FIXTURES/prs.json" ;;
  "api "*)
    case "$*" in
      *"/dependencies/blocked_by"*|*"/dependencies/blocking"*)
        kind=$(sed -n 's#.*/dependencies/\([a-z_]*\).*#\1#p' <<<"$*")
        number=$(sed -n 's#.*/issues/\([0-9]*\)/dependencies/.*#\1#p' <<<"$*")
        cat "$DISPATCH_FIXTURES/deps-$kind-$number.json" 2>/dev/null || echo '[]'
        ;;
      *"/pulls/"*"/files"*)
        number=$(sed -n 's#.*/pulls/\([0-9]*\)/files.*#\1#p' <<<"$*")
        echo "$number" >>"$DISPATCH_FIXTURES/files-calls"
        cat "$DISPATCH_FIXTURES/files-$number.json" 2>/dev/null || echo '[]'
        ;;
      *"/pulls?state=open"*) cat "$DISPATCH_FIXTURES/pulls.json" || exit 1 ;;
      *"/issues/"*) cat "$DISPATCH_FIXTURES/issue-$(sed -n 's#.*/issues/\([0-9]*\)$#\1#p' <<<"$*").json" || exit 1 ;;
      *"repos/dazsel/omuzaki") cat "$DISPATCH_FIXTURES/repo.json" || exit 1 ;;
      *"/contents/crates"*) cat "$DISPATCH_FIXTURES/crates.json" || exit 1 ;;
      *"/contents") cat "$DISPATCH_FIXTURES/root.json" || exit 1 ;;
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
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"closingIssuesReferences":[{"number":9100,"repository":{"nameWithOwner":"dazsel/omuzaki"}}]},
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
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"closingIssuesReferences":[{"number":9101,"repository":{"nameWithOwner":"dazsel/omuzaki"}}]}]
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
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"closingIssuesReferences":[{"number":9004,"repository":{"nameWithOwner":"dazsel/omuzaki"}}]}]
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
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"closingIssuesReferences":[{"number":9004,"repository":{"nameWithOwner":"dazsel/omuzaki"}}]}]
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
  "files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"closingIssuesReferences":[{"number":9012,"repository":{"nameWithOwner":"dazsel/omuzaki"}}]}]
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
  "files":[{"path":"tools/y.py"}],"closingIssuesReferences":[{"number":9005,"repository":{"nameWithOwner":"dazsel/omuzaki"}}]}]
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

# fence_matches: one table for every rule. Each row is entry, path, expected (True, False, or "error" for unreadable).
python3 -I - "$CHECK" <<'P'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("dc", sys.argv[1])
dc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dc)
T, F, E = True, False, "error"
table = [
    # literal file: exact path only
    ("tools/x.py", "tools/x.py", T), ("tools/x.py", "tools/y.py", F), ("tools/x.py", "tools/x.py/z", T),
    ("tools/x.py", "xtools/x.py", F), ("./tools/x.py", "tools/x.py", T), ("CLAUDE.md", "CLAUDE.md", T),
    # literal directory, with and without trailing slash: every descendant, not siblings
    ("tools", "tools/x.py", T), ("tools/", "tools/x.py", T), ("tools/", "tools/a/b/c.py", T),
    ("tools/", "tools2/x.py", F), ("tools", "tool/x.py", F), ("tools/", "tools", T),
    # * stays within one segment
    ("tools/*.py", "tools/x.py", T), ("tools/*.py", "tools/a/x.py", F), ("tools/*", "tools/a/x.py", T),
    ("crates/*-api/src", "crates/omuzaki-ops-api/src/lib.rs", T), ("crates/*-api/src", "crates/a/b-api/src/x", F),
    ("a/?.rs", "a/b.rs", T), ("a/?.rs", "a/bc.rs", F), ("a/[ab].rs", "a/b.rs", T), ("a/[!ab].rs", "a/b.rs", F),
    # ** crosses segments, and a/**/b may match a/b
    ("crates/**/mod.rs", "crates/a/b/mod.rs", T), ("crates/**/mod.rs", "crates/mod.rs", T),
    ("crates/**/mod.rs", "crates/a/b/lib.rs", F), ("docs/**", "docs/a/b.md", T), ("**/x.py", "tools/a/x.py", T),
    # ancestor directories: globs and brace groups match descendants too
    ("crates/omuzaki-*/src/", "crates/omuzaki-ops/src/a.rs", T), ("crates/omuzaki-*/src", "crates/omuzaki-ops/tests/a.rs", F),
    ("crates/omuzaki-*", "crates/omuzaki-ops/src/a.rs", T), ("tools/*.d", "tools/x.d/deep/y", T),
    ("crates/{a,b}/src/", "crates/b/src/x.rs", T), ("crates/{a,b}/src/", "crates/c/src/x.rs", F),
    ("tools/{x,y}.py", "tools/y.py", T), ("crates/{a,{b,c}}/src", "crates/c/src/z.rs", T),
    ("crates/omuzaki-{ops,io}-*/src/", "crates/omuzaki-io-ops/src/a.rs", T),
    # unreadable entries fail closed
    ("tools/{a,b", "tools/a", E), ("tools/a}", "tools/a", E), ("a/[bc", "a/b", E), ("/etc/passwd", "etc/passwd", E),
    ("../x", "x", E), ("a//b", "a/b", E), ("", "a", E), ("/", "a", E),
    ("tools/[z-a].py", "tools/x.py", E),
]
bad = 0
for entry, path, want in table:
    try:
        got = dc.fence_matches(entry, path)
    except ValueError:
        got = E
    if got != want:
        bad += 1
        print(f"FAIL: fence_matches({entry!r}, {path!r}) = {got}, wanted {want}", file=sys.stderr)
sys.exit(1 if bad else 0)
P
if [[ $? == 0 ]]; then pass "fence_matches: every rule"; else fail "fence_matches table"; fi

# regression: `Fence: tools/, CLAUDE.md` keeps the top-level folder, so a PR in tools/ blocks.
scenario topdir
issue 9020 <<'B'
**Fence:** tools/, CLAUDE.md
B
cat >"$FX/prs.json" <<'J'
[{"number":641,"headRefName":"cloud/641","isDraft":false,"createdAt":"2026-10-09T10:00:00Z","files":[{"path":"tools/x.py"}],"body":""}]
J
run 9020
expect_line "wait #9020: overlap with #641 (tools/x.py)" "top-level folder entry is kept"

# regression: a glob folder covers its descendants.
scenario globdir
issue 9021 <<'B'
**Fence:** `crates/omuzaki-*/src/`
B
cat >"$FX/prs.json" <<'J'
[{"number":642,"headRefName":"cloud/642","isDraft":false,"createdAt":"2026-10-09T10:00:00Z","files":[{"path":"crates/omuzaki-ops/src/a.rs"}],"body":""}]
J
echo '[{"name":"omuzaki-ops","type":"dir"}]' >"$FX/crates.json"
run 9021
if grep -q "overlap with #642 (crates/omuzaki-ops/src/a.rs)" <<<"$out"; then pass "glob folder covers descendants"; else fail "glob folder: $out"; fi

# fail closed: an entry that cannot be understood waits, even beside a good one.
scenario unreadable
issue 9022 <<'B'
**Fence:** `tools/x.py`, `crates/{a,b/src`
B
echo '[]' >"$FX/prs.json"
run 9022
if grep -q "^wait #9022: cannot understand the fence entry crates/{a,b/src" <<<"$out"; then pass "unreadable entry waits"; else fail "unreadable entry: $out"; fi

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

# top-level names: a slashless fence word is a path only when it names a root entry (`tools`, `LICENSE-MIT`).
scenario topwords
issue 9030 <<'B'
**Fence:** tools, CLAUDE.md
B
echo '[{"name":"tools","type":"dir"},{"name":"CLAUDE.md","type":"file"},{"name":"LICENSE-MIT","type":"file"}]' >"$FX/root.json"
cat >"$FX/prs.json" <<'J'
[{"number":651,"headRefName":"cloud/651","isDraft":false,"createdAt":"2026-10-11T10:00:00Z","files":[{"path":"tools/x.py"}],"body":""}]
J
run 9030
expect_line "wait #9030: overlap with #651 (tools/x.py)" "top-level: a slashless root folder (tools) collides with its files"
expect_code 1 "top-level: exit 1"

scenario topfile
issue 9031 <<'B'
**Fence:** `LICENSE-MIT`
B
echo '[{"name":"tools","type":"dir"},{"name":"LICENSE-MIT","type":"file"}]' >"$FX/root.json"
cat >"$FX/prs.json" <<'J'
[{"number":652,"headRefName":"cloud/652","isDraft":false,"createdAt":"2026-10-11T11:00:00Z","files":[{"path":"LICENSE-MIT"}],"body":""}]
J
run 9031
expect_line "wait #9031: overlap with #652 (LICENSE-MIT)" "top-level: a slashless root file collides with the same file"

scenario topunreadable
issue 9032 <<'B'
**Fence:** tools
B
echo '[]' >"$FX/prs.json"
run 9032
if grep -q "^wait #9032: cannot understand the fence entry tools" <<<"$out"; then pass "top-level: a slashless word waits when the root cannot be read"; else fail "top-level unreadable: $out"; fi

# fence_entries: the token rule, table-driven. Each row is (body, top-level names or None, entries, bad).
python3 -I - "$CHECK" <<'P'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("dc", sys.argv[1])
dc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dc)
root = {"tools", "LICENSE-MIT", "CLAUDE.md", "crates", "README.md"}
table = [
    ("Fence: tools, LICENSE-MIT, CLAUDE.md, only", root, ["tools", "LICENSE-MIT", "CLAUDE.md"], []),
    ("Fence: `tools`, `only`", root, ["tools", "only"], []),  # quoted slashless tokens stay even when absent from main
    ("Fence: `tools`, only", root, ["tools"], []),  # an unquoted word that names no root entry is prose
    ("Fence: none", root, [], []),
    ("Fence: tools, LICENSE-MIT", None, [], ["tools", "LICENSE-MIT"]),
    ("Fence: tools/x.py, only", None, ["tools/x.py"], ["only"]),
]
bad = 0
for body, top, want_entries, want_bad in table:
    got = dc.fence_entries(body, top)
    if got != (want_entries, want_bad):
        bad += 1
        print(f"FAIL: fence_entries({body!r}) = {got}, wanted {(want_entries, want_bad)}", file=sys.stderr)
sys.exit(1 if bad else 0)
P
if [[ $? == 0 ]]; then pass "fence_entries: slashless words by root name"; else fail "fence_entries: slashless table"; fi

# closing references come from GitHub's list, not the body: a body keyword GitHub didn't list doesn't count.
scenario not-closes
issue 42 <<'B'
**Fence:** `tools/n.py`
B
cat >"$FX/prs.json" <<'J'
[{"number":660,"headRefName":"cloud/660","isDraft":false,"createdAt":"2026-10-12T10:00:00Z",
  "files":[{"path":"tools/n.py"}],"body":"Does not close #42","closingIssuesReferences":[]}]
J
run 42
expect_line "wait #42: overlap with #660 (tools/n.py)" "closing refs: 'Does not close #42' is not a closing reference"

# brace expansion is bounded: 30 groups make 2^30 alternatives, so the entry waits; 8 groups (256) are still read.
scenario brace-limit
{ echo '**Fence:**'; printf -- '- `tools/%s`\n' "$(printf '{a,b}%.0s' {1..30})"; } >"$work/body"
issue 9050 <"$work/body"
echo '[]' >"$FX/prs.json"
run 9050
if grep -q "^wait #9050: cannot understand the fence entry tools/" <<<"$out"; then pass "brace limit: 30 groups wait"; else fail "brace limit: $out"; fi
scenario brace-edge
{ echo '**Fence:**'; printf -- '- `tools/%s`\n' "$(printf '{a,b}%.0s' {1..8})"; } >"$work/body"
issue 9051 <"$work/body"
echo '[]' >"$FX/prs.json"
run 9051
expect_line "go #9051" "brace limit: 8 groups (256 alternatives) are read"

# a backtick span keeps its spaces: one entry, so a PR touching the file collides.
scenario spaced
issue 9052 <<'B'
**Fence:** `docs/My File.md`
B
cat >"$FX/prs.json" <<'J'
[{"number":661,"headRefName":"cloud/661","isDraft":false,"createdAt":"2026-10-12T11:00:00Z",
  "files":[{"path":"docs/My File.md"}],"body":""}]
J
run 9052
expect_line "wait #9052: overlap with #661 (docs/My File.md)" "quoted span: a path with spaces is one entry"

# an invalid character range is an unreadable entry: a wait, not a traceback.
scenario bad-range
issue 9053 <<'B'
**Fence:** `tools/[z-a].py`
B
echo '[]' >"$FX/prs.json"
run 9053
if [[ $code == 1 ]] && grep -q "^wait #9053: cannot understand the fence entry tools/\[z-a\].py" <<<"$out"; then
  pass "invalid regex range waits"
else
  fail "invalid regex range: exit $code: $out $(cat "$work/err")"
fi

# closing references name a repository: `Fixes other/repo#42` is not this repository's issue 42.
scenario other-repo
issue 42 <<'B'
**Fence:** `tools/o.py`
B
cat >"$FX/prs.json" <<'J'
[{"number":662,"headRefName":"cloud/662","isDraft":false,"createdAt":"2026-10-12T12:00:00Z",
  "files":[{"path":"tools/o.py"}],"body":"Fixes other/repo#42",
  "closingIssuesReferences":[{"number":42,"repository":{"nameWithOwner":"other/repo"}}]}]
J
run 42
expect_line "wait #42: overlap with #662 (tools/o.py)" "closing refs: another repository's #42 does not remove the PR"

# renamed files: a rename out of a fenced folder collides through its old path.
scenario rename
issue 9060 <<'B'
**Fence:** `crates/a`
B
cat >"$FX/prs.json" <<'J'
[{"number":663,"headRefName":"cloud/663","isDraft":false,"createdAt":"2026-10-12T13:00:00Z",
  "files":[{"path":"crates/b/x.rs"}],"body":""}]
J
cat >"$FX/files-663.json" <<'J'
[{"filename":"crates/b/x.rs","status":"renamed","previous_filename":"crates/a/x.rs"}]
J
run 9060
expect_line "wait #9060: overlap with #663 (crates/a/x.rs)" "rename: the old path of a renamed file collides"
[[ $(grep -cx 663 "$FX/files-calls") == 1 ]] && pass "one fetch: the rename list is read once" \
  || fail "one fetch: #663 read $(grep -cx 663 "$FX/files-calls") times"

# renamed files: an unreadable rename list makes the PR's files unknown, so the task waits.
scenario rename-unreadable
issue 9061 <<'B'
**Fence:** `crates/a`
B
cat >"$FX/prs.json" <<'J'
[{"number":664,"headRefName":"cloud/664","isDraft":false,"createdAt":"2026-10-12T14:00:00Z",
  "files":[{"path":"docs/other.md"}],"body":""}]
J
echo 'not json' >"$FX/files-664.json"
run 9061
expect_line "wait #9061: cannot read the changed files of #664; waiting until they can be read" "rename: waits when the rename list cannot be read"

# dependencies: only an anchored `Blocked by:` or `Depends on:` line counts; prose about blocking does not.
scenario prose-dependency
issue 9062 <<'B'
This task is not blocked by #99.

**Fence:** `crates/a/x.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":665,"headRefName":"cloud/665","isDraft":false,"createdAt":"2026-10-12T15:00:00Z",
  "files":[{"path":"crates/a/x.rs"}],"body":"",
  "closingIssuesReferences":[{"number":99,"repository":{"nameWithOwner":"dazsel/omuzaki"}}]}]
J
run 9062
expect_line "wait #9062: overlap with #665 (crates/a/x.rs)" "dependencies: prose 'not blocked by #99' records nothing"

scenario depends-on
issue 9063 <<'B'
- Depends on: #99

**Fence:** `crates/a/x.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":666,"headRefName":"cloud/666","isDraft":false,"createdAt":"2026-10-12T16:00:00Z",
  "files":[{"path":"crates/a/x.rs"}],"body":"",
  "closingIssuesReferences":[{"number":99,"repository":{"nameWithOwner":"dazsel/omuzaki"}}]}]
J
run 9063
expect_line "go #9063" "dependencies: a 'Depends on:' line chains the overlap"

# a wildcard fence also names crates that open PRs add (not on main yet): contract first fires for the new -api crate.
scenario newcrate
issue 9070 <<'B'
**Fence:** `crates/*-api/src/types.rs`
B
echo '[{"name":"omuzaki-core","type":"dir"}]' >"$FX/crates.json"
echo '[{"name":"crates","type":"dir"}]' >"$FX/root.json"
cat >"$FX/prs.json" <<'J'
[{"number":701,"headRefName":"cloud/701","isDraft":false,"createdAt":"2026-10-09T10:00:00Z","files":[{"path":"crates/new-api/src/lib.rs"}],"body":""}]
J
run 9070
expect_line "wait #9070: contract first: new-api has open PR(s) #701; a contract lands alone" "new crate from an open PR: waits"

# a backtick-quoted slashless fence entry is kept even when main lacks it, so an open PR adding it collides.
scenario newroot
issue 9071 <<'B'
**Fence:** `NEWROOT`
B
echo '[{"name":"tools","type":"dir"}]' >"$FX/root.json"
cat >"$FX/prs.json" <<'J'
[{"number":702,"headRefName":"cloud/702","isDraft":false,"createdAt":"2026-10-09T10:00:00Z","files":[{"path":"NEWROOT"}],"body":""}]
J
run 9071
expect_line "wait #9071: overlap with #702 (NEWROOT)" "quoted slashless path absent from main: overlaps"

# a missing fixture is a gh failure: exit 2 with a message, not a crash.
scenario missing
echo '[]' >"$FX/prs.json"
run 9999
expect_code 2 "missing issue: exit 2"

# closes_in_body: GitHub's keyword rule on a PR body. A negated keyword and another repository's issue do not count.
# Run through importlib because the script's name has a hyphen.
parser_cases() {
  out=$(python3 -I -c '
import importlib.util, sys
spec = importlib.util.spec_from_file_location("dispatch_check", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
for body in ["Fixes #42", "Closes: #7", "Does not close #42", "Fixes other/repo#42", "prefix #9", "bugfix #3",
             "resolved #5 and Closed #6", "Fixes\n#8", "Use `Fixes #10` to link", "a ``Fixes #11`` span",
             "<!-- Fixes #12 --> Closes #13", "```\nFixes #14\n```\nFixes #15", "~~~md\nCloses #16\n",
             "<!-- Fixes #17", "Example:\n\n    Fixes #18\n\nCloses #19", "\tFixes #20", "text\n    Fixes #21", ">     Fixes #22", "> quoted Closes #23"]:
    print(body.replace("\n", " "), sorted(mod.closes_in_body(body)))
' "$CHECK" 2>"$work/err")
  expect_line "Fixes #42 [42]" "keyword: Fixes #42 closes 42"
  expect_line "Closes: #7 [7]" "keyword: an optional colon is allowed"
  expect_line "Does not close #42 [42]" "keyword: GitHub's rule is lexical, so a negated keyword still counts"
  expect_line "Fixes other/repo#42 []" "keyword: another repository's issue does not count"
  expect_line "prefix #9 []" "keyword: a keyword inside a word does not count"
  expect_line "bugfix #3 []" "keyword: a suffix of a word does not count"
  expect_line "resolved #5 and Closed #6 [5, 6]" "keyword: every keyword in a body counts, case-insensitively"
  expect_line "Fixes #8 []" "keyword: a line break between keyword and number does not count"
  expect_line "Use \`Fixes #10\` to link []" "keyword: an inline code span is an example, not a reference"
  expect_line "a \`\`Fixes #11\`\` span []" "keyword: a double-backtick span is an example too"
  expect_line "<!-- Fixes #12 --> Closes #13 [13]" "keyword: an HTML comment does not count; prose after it does"
  expect_line "\`\`\` Fixes #14 \`\`\` Fixes #15 [15]" "keyword: a fenced block does not count; prose after it does"
  expect_line "~~~md Closes #16  []" "keyword: an unclosed fence runs to the end of the body"
  expect_line "<!-- Fixes #17 []" "keyword: an unclosed HTML comment runs to the end of the body"
  expect_line "Example:      Fixes #18  Closes #19 [19]" "keyword: an indented code block after a blank line is an example"
  expect_line "	Fixes #20 []" "keyword: a tab-indented block at the start is an example"
  expect_line "text     Fixes #21 [21]" "keyword: an indented line that continues a paragraph is text"
  expect_line ">     Fixes #22 []" "keyword: indented code inside a block quote is an example"
  expect_line "> quoted Closes #23 [23]" "keyword: quoted prose still counts, as on GitHub"
}
parser_cases

# REST path, with GraphQL refused: the same verdicts as the GraphQL path. #9100 is closed by its own PR 610 (go);
# #9101 overlaps PR 611, whose body only says "Related to #9101", so it does not close it and the overlap counts.
rest_fixtures() {
  touch "$FX/graphql-refused"
  issue 9100 <<'B'
**Fence:** `crates/omuzaki-ops/src/a.rs`
B
  issue 9101 <<'B'
**Fence:** `crates/omuzaki-io-ops/src/b.rs`
B
  echo '[{"name":"omuzaki-ops","type":"dir"},{"name":"omuzaki-io-ops","type":"dir"}]' >"$FX/crates.json"
  echo '[{"name":"crates","type":"dir"}]' >"$FX/root.json"
  echo '{"default_branch":"main"}' >"$FX/repo.json"
  cat >"$FX/pulls.json" <<'J'
[{"number":610,"head":{"ref":"cloud/610-own"},"draft":false,"created_at":"2026-10-11T10:00:00Z","base":{"ref":"main"},"body":"Fixes #9100"},
 {"number":611,"head":{"ref":"local/611-x"},"draft":false,"created_at":"2026-10-11T11:00:00Z","base":{"ref":"main"},"body":"Related to #9101"},
 {"number":612,"head":{"ref":"local/612-draft"},"draft":true,"created_at":"2026-10-11T12:00:00Z","base":{"ref":"main"},"body":null}]
J
  echo '[{"filename":"crates/omuzaki-ops/src/a.rs","status":"modified"}]' >"$FX/files-610.json"
  echo '[{"filename":"crates/omuzaki-io-ops/src/b.rs","status":"modified"}]' >"$FX/files-611.json"
  echo '[{"filename":"crates/omuzaki-io-ops/src/b.rs","status":"modified"}]' >"$FX/files-612.json"
}

# refused: the GraphQL reads fail, so the script falls back to REST, says so on stderr, and gives the same verdicts.
scenario rest-refused
rest_fixtures
run 9100
expect_line "go #9100" "rest fallback: own PR closes the task by body keyword, so go"
expect_code 0 "rest fallback: exit 0 for go"
grep -q "GraphQL refused, reading REST instead" "$work/err" && pass "rest fallback: says so on stderr" \
  || fail "rest fallback: no stderr notice: $(cat "$work/err")"
run 9101
expect_line "wait #9101: overlap with #611 (crates/omuzaki-io-ops/src/b.rs)" "rest fallback: a mention without a keyword leaves the overlap"
expect_code 1 "rest fallback: exit 1 for wait"

# DISPATCH_CHECK_REST=1: GraphQL is not tried at all (no notice), and the verdicts match. The prs.json fixture is an
# empty list, so a GraphQL read that wrongly ran would say go for #9101 and fail the test.
scenario rest-only
rest_fixtures
rm "$FX/graphql-refused"
echo '[]' >"$FX/prs.json"
DISPATCH_CHECK_REST=1 run 9101
expect_line "wait #9101: overlap with #611 (crates/omuzaki-io-ops/src/b.rs)" "rest only: verdict from REST"
[[ ! -s "$work/err" ]] && pass "rest only: GraphQL not tried, no notice" || fail "rest only: stderr: $(cat "$work/err")"

# one fetch per PR: a REST run reads each open PR's file list once, drafts included.
scenario rest-once
rest_fixtures
run 9100
for pr in 610 611 612; do
  [[ $(grep -cx "$pr" "$FX/files-calls") == 1 ]] && pass "one fetch: REST reads #$pr once" \
    || fail "one fetch: #$pr read $(grep -cx "$pr" "$FX/files-calls") times"
done

# base: closing keywords count only on a pull request into the default branch. The same "Fixes #9102" PR into release
# does not close #9102, so its overlap counts; into main it closes #9102, so the task is its own and goes.
one_pull() {
  printf '[{"number":%s,"head":{"ref":"local/%s-x"},"draft":false,"created_at":"2026-10-11T13:00:00Z","base":{"ref":"%s"},"body":"Fixes #9102"}]\n' \
    "$1" "$1" "$2" >"$FX/pulls.json"
  echo '[{"filename":"crates/omuzaki-ops/src/c.rs","status":"modified"}]' >"$FX/files-$1.json"
}
scenario rest-base
rest_fixtures
issue 9102 <<'B'
**Fence:** `crates/omuzaki-ops/src/c.rs`
B
one_pull 613 release
run 9102
expect_line "wait #9102: overlap with #613 (crates/omuzaki-ops/src/c.rs)" "rest base: a PR into a non-default branch does not close the task"
one_pull 614 main
run 9102
expect_line "go #9102" "rest base: a PR into the default branch closes the task"

# cap: the REST files endpoint returns at most 3,000 files. A PR with 3,000 is unreadable, so the task waits; 2,999 is read.
# big_pr NUMBER COUNT: the only open PR, into main with no closing keyword, whose REST list has COUNT files.
big_pr() {
  printf '[{"number":%s,"head":{"ref":"local/%s-big"},"draft":false,"created_at":"2026-10-11T14:00:00Z","base":{"ref":"main"},"body":""}]\n' \
    "$1" "$1" >"$FX/pulls.json"
  python3 -I -c 'import json,sys; print(json.dumps([{"filename": f"crates/omuzaki-io-ops/src/f{i}.rs", "status": "modified"} for i in range(int(sys.argv[1]))]))' \
    "$2" >"$FX/files-$1.json"
}
scenario rest-cap
rest_fixtures
issue 9103 <<'B'
**Fence:** `tools/x.py`
B
big_pr 615 3000
run 9103
expect_line "wait #9103: cannot read the changed files of #615; waiting until they can be read" "rest cap: 3,000 files is unreadable"
big_pr 615 2999
run 9103
expect_line "go #9103" "rest cap: 2,999 files is read in full"


# per-module buckets (ADR 0062): every TEST_MODULES entry maps to its module, as a file and (for a folder) inside it;
# the ADR's own examples; and a fence's literal prefix names its module, or every bucket when it could reach several.
python3 -I - "$CHECK" <<'P'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("dc", sys.argv[1])
dc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dc)
bad = 0
def want(got, expected, label):
    global bad
    if got != expected:
        bad += 1
        print(f"FAIL: {label}: got {got!r}, wanted {expected!r}", file=sys.stderr)
    else:
        print(f"ok: {label}")
# the intended mapping, written out a second time here rather than read from TEST_MODULES: an entry that is changed to
# the wrong module or deleted in the table fails, and so does one missing here. Each entry is then checked through
# bucket_of: a file key is its own path, a folder key holds files and is itself the folder.
EXPECTED = {
    "omuzaki-char": {
        "tests/accessory_rigid.rs": "accessory",
        "tests/body.rs": "body",
        "tests/body_shape.rs": "body",
        "tests/builder.rs": "builder",
        "tests/cloth_contact.rs": "cloth",
        "tests/cloth_contact_replay.rs": "cloth",
        "tests/cloth_live.rs": "cloth",
        "tests/cloth_solver": "cloth",
        "tests/hair_bangs.rs": "hair",
        "tests/hair_braid.rs": "hair",
        "tests/hair_cap_sweep.rs": "hair",
        "tests/hair_curtain.rs": "hair",
        "tests/hair_pipeline.rs": "hair",
        "tests/head_ears.rs": "head",
        "tests/head_part.rs": "head",
        "tests/head_quality": "head",
        "tests/head_shape.rs": "head",
        "tests/head_trace.rs": "head",
        "tests/kit_builder.rs": "kit",
        "tests/kit_parity.rs": "kit",
        "tests/kit_sweep.rs": "kit",
        "tests/look_ops.rs": "ops",
        "tests/op_layout.rs": "ops",
        "tests/op_part_sources.rs": "ops",
        "tests/op_variant.rs": "ops",
        "tests/op_weights.rs": "ops",
        "tests/ops_body.rs": "ops",
        "tests/ops_character.rs": "ops",
        "tests/ops_part_param.rs": "ops",
        "tests/palette_contrast.rs": "palette",
        "tests/palette_independence.rs": "palette",
        "tests/palette_ops.rs": "palette",
        "tests/skin_tone.rs": "palette",
        "tests/template_soft_sd.rs": "template",
        "tests/template_soft_sd.proptest-regressions": "template",
        "tests/templates.rs": "template",
        "benches/builder.rs": "builder",
        "benches/cloth_contact.rs": "cloth",
        "benches/cloth_runtime.rs": "cloth",
        "benches/cloth_solver.rs": "cloth",
        "benches/hair_cap_sweep.rs": "hair",
        "benches/hair_runtime.rs": "hair",
        "benches/hair_verlet.rs": "hair",
        "benches/kit.rs": "kit",
    },
    "omuzaki-face": {
        "tests/brows.rs": "painter",
        "tests/cheeks_nose_marks.rs": "painter",
        "tests/eyes_aperture.rs": "painter",
        "tests/eyes_iris.rs": "painter",
        "tests/mouth_interior.rs": "painter",
        "tests/mouth_shape.rs": "painter",
        "tests/op_expression.rs": "ops",
        "tests/op_mapping.rs": "ops",
        "tests/op_style.rs": "ops",
        "tests/raster_subpixel.rs": "raster",
        "tests/raster_unit.rs": "raster",
        "tests/system.rs": "system",
        "tests/underlay.rs": "underlay",
        "benches/cheeks_nose_marks.rs": "painter",
        "benches/face_system.rs": "system",
        "benches/mouth_interior.rs": "painter",
        "benches/mouth_shape.rs": "painter",
        "benches/painter_eyes.rs": "painter",
    },
}
got = {(c, k): m for c, keys in dc.TEST_MODULES.items() for k, m in keys.items()}
intended = {(c, k): m for c, keys in EXPECTED.items() for k, m in keys.items()}
differ = sorted((c, k, got.get((c, k)), intended.get((c, k))) for c, k in set(got) | set(intended)
                if got.get((c, k)) != intended.get((c, k)))
want(differ, [], "TEST_MODULES holds exactly the intended mapping")
for crate, keys in EXPECTED.items():
    for key, module in keys.items():
        if "." in key.rsplit("/", 1)[-1]:
            want(dc.bucket_of(crate, key, True), module, f"{crate}/{key} -> {module}")
        else:
            want(dc.bucket_of(crate, key + "/main.rs", True), module, f"{crate}/{key}/main.rs -> {module}")
            want(dc.bucket_of(crate, key, False), module, f"{crate}/{key} (folder) -> {module}")
S, B = dc.SHARED, dc.BROAD
# a changed file: the ADR's examples and the shared files
paths = [
    ("omuzaki-face", "src/layer.rs", "layer"), ("omuzaki-face", "tests/brows.rs", "painter"),
    ("omuzaki-face", "src/painter/brows.rs", "painter"), ("omuzaki-face", "src/ops_shared/check.rs", "ops_shared"),
    ("omuzaki-face", "src/ops/expression/set.rs", "ops"), ("omuzaki-face", "tests/hot_loop.rs", S),
    ("omuzaki-face", "benches/gpu_face.rs", S), ("omuzaki-char", "src/hair/tube.rs", "hair"),
    ("omuzaki-char", "src/hair.rs", "hair"), ("omuzaki-char", "src/builder.rs", "builder"),
    ("omuzaki-char", "src/lib.rs", S), ("omuzaki-char", "src/main.rs", S), ("omuzaki-char", "src/NOTICES.md", S),
    ("omuzaki-char", "src/kit/NOTICES.md", "kit"), ("omuzaki-char", "Cargo.toml", S), ("omuzaki-char", "build.rs", S),
    ("omuzaki-char", "tests/common/mod.rs", S), ("omuzaki-char", "tests/fixtures/kit/pchip.json", S),
    ("omuzaki-char", "tests/skin.rs", S), ("omuzaki-char", "tests/cloth_solver/suite.rs", "cloth"),
    ("omuzaki-char", "tests/head_quality/anchors.rs", "head"), ("omuzaki-char", "benches/kit.rs", "kit"),
    ("omuzaki-char", "examples/stature_lineup.rs", S),
]
for crate, rel, expected in paths:
    want(dc.bucket_of(crate, rel, True), expected, f"file {crate}/{rel} -> {expected}")
# a fence entry: its literal prefix names its bucket; a glob or a folder holding several names every bucket
fences = [
    ("omuzaki-char", "src/hair/**", {"hair"}), ("omuzaki-char", "src/hair/", {"hair"}),
    ("omuzaki-char", "src/hair.rs", {"hair"}), ("omuzaki-char", "src/kit/NOTICES.md", {"kit"}),
    ("omuzaki-char", "src/", {B}), ("omuzaki-char", "src", {B}), ("omuzaki-char", "src/*.rs", {B}),
    ("omuzaki-char", "src/hair*/x.rs", {B}), ("omuzaki-char", "", {B}), ("omuzaki-char", "*.toml", {B}),
    ("omuzaki-char", "tests/", {B}), ("omuzaki-char", "tests/*.rs", {B}), ("omuzaki-char", "tests/brows*.rs", {B}),
    ("omuzaki-char", "tests/cloth_solver/**", {"cloth"}), ("omuzaki-char", "tests/cloth_solver", {"cloth"}),
    ("omuzaki-char", "Cargo.toml", {S}), ("omuzaki-char", "src/lib.rs", {S}), ("omuzaki-char", "tests/common/**", {S}),
]
for crate, rel, expected in fences:
    want(dc.fence_buckets(crate, rel), expected, f"fence {crate}/{rel} -> {sorted(expected)}")
sys.exit(1 if bad else 0)
P
if [[ $? == 0 ]]; then pass "module buckets: the table, the ADR examples and the fence rules"; else fail "module buckets table"; fi

# module cap: a full module waits and is named; a free module in the same crate goes; a fence naming both waits on the full one.
scenario module
issue 9201 <<'B'
**Fence:** `crates/omuzaki-char/src/hair/tube.rs`
B
issue 9202 <<'B'
**Fence:** `crates/omuzaki-char/src/head/mesh.rs`
B
issue 9203 <<'B'
**Fence:** `crates/omuzaki-char/src/{hair,head}/x.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":701,"headRefName":"cloud/701-hair","isDraft":false,"createdAt":"2026-10-01T10:00:00Z","files":[{"path":"crates/omuzaki-char/src/hair/a.rs"}],"body":""},
 {"number":702,"headRefName":"cloud/702-hair","isDraft":false,"createdAt":"2026-10-01T11:00:00Z","files":[{"path":"crates/omuzaki-char/src/hair/b.rs"}],"body":""},
 {"number":703,"headRefName":"local/703-hair","isDraft":false,"createdAt":"2026-10-01T12:00:00Z","files":[{"path":"crates/omuzaki-char/src/hair/c.rs"}],"body":""}]
J
run 9201 9202 9203
expect_line "wait #9201: module cap: omuzaki-char/hair has 3 open PRs (#701, #702, #703) (cap 3)" "module cap: a full module waits and is named"
expect_line "go #9202" "module cap: a free module in the same crate is go"
expect_line "wait #9203: module cap: omuzaki-char/hair has 3 open PRs (#701, #702, #703) (cap 3)" "module cap: a fence naming a full and a free module names the full one"
expect_code 1 "module cap: exit 1"
run 9201 --module-cap 4
expect_line "go #9201" "module cap: a higher --module-cap lets it go"

# ceiling: 12 open PRs in the crate (the default, ADR 0062) wait, though no module is full and the crate cap (3) would
# bite any other crate. Twelve modules, one PR each, so no module is full; the fence is a thirteenth, empty module.
scenario ceiling
issue 9204 <<'B'
**Fence:** `crates/omuzaki-char/src/template/layout.rs`
B
python3 -I -c '
import json
mods = ["accessory", "body", "builder", "cloth", "fit", "head", "kit", "look", "hair", "ops", "palette", "rig"]
print(json.dumps([{"number": 800 + i, "headRefName": f"cloud/{800 + i}", "isDraft": False,
                   "createdAt": "2026-10-08T10:00:00Z", "files": [{"path": f"crates/omuzaki-char/src/{m}/x.rs"}], "body": ""}
                  for i, m in enumerate(mods)]))' >"$FX/prs.json"
run 9204
expect_line "wait #9204: crate ceiling: omuzaki-char has 12 open PRs (#800, #801, #802, #803, #804, #805, #806, #807, #808, #809, #810, #811) (ceiling 12)" "ceiling: the default waits at 12 open PRs, and the crate cap does not count char"
run 9204 --module-ceiling 13
expect_line "go #9204" "ceiling: a higher --module-ceiling lets it go"

# shared slot: a root file, lib.rs, tests/common and an unlisted test take the one slot; a task that does not need it goes.
scenario shared
issue 9205 <<'B'
**Fence:** `crates/omuzaki-char/src/head/mesh.rs`
B
issue 9206 <<'B'
**Fence:** `crates/omuzaki-char/Cargo.toml`
B
issue 9207 <<'B'
**Fence:** `crates/omuzaki-char/tests/common/mod.rs`
B
issue 9208 <<'B'
**Fence:** `crates/omuzaki-char/tests/skin.rs`
B
issue 9209 <<'B'
**Fence:** `crates/omuzaki-char/tests/hair_pipeline.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":811,"headRefName":"cloud/811-lib","isDraft":false,"createdAt":"2026-10-11T10:00:00Z","files":[{"path":"crates/omuzaki-char/src/lib.rs"}],"body":""}]
J
run 9205 9206 9207 9208 9209
expect_line "go #9205" "shared: a task that does not need the shared slot is go"
expect_line "wait #9206: shared slot: omuzaki-char is taken by #811" "shared: a root file waits while the slot is taken"
expect_line "wait #9207: shared slot: omuzaki-char is taken by #811" "shared: tests/common waits while the slot is taken"
expect_line "wait #9208: shared slot: omuzaki-char is taken by #811" "shared: an unlisted test takes the slot"
expect_line "go #9209" "shared: a listed test counts against its module, not the slot"
echo '[]' >"$FX/prs.json"
run 9206
expect_line "go #9206" "shared: a free slot lets the root file go"

# face: tests/brows.rs counts against painter, so a third painter PR fills it; layer is its own module and stays free.
scenario face
issue 9210 <<'B'
**Fence:** `crates/omuzaki-face/src/painter/marks.rs`
B
issue 9211 <<'B'
**Fence:** `crates/omuzaki-face/src/layer.rs`
B
cat >"$FX/prs.json" <<'J'
[{"number":821,"headRefName":"cloud/821-brows","isDraft":false,"createdAt":"2026-10-12T10:00:00Z","files":[{"path":"crates/omuzaki-face/tests/brows.rs"}],"body":""},
 {"number":822,"headRefName":"cloud/822-iris","isDraft":false,"createdAt":"2026-10-12T11:00:00Z","files":[{"path":"crates/omuzaki-face/src/painter/eyes/iris.rs"}],"body":""},
 {"number":823,"headRefName":"local/823-system","isDraft":false,"createdAt":"2026-10-12T12:00:00Z","files":[{"path":"crates/omuzaki-face/src/system.rs"}],"body":""},
 {"number":824,"headRefName":"local/824-eyes","isDraft":false,"createdAt":"2026-10-12T13:00:00Z","files":[{"path":"crates/omuzaki-face/tests/eyes_iris.rs"}],"body":""}]
J
run 9210 9211
expect_line "wait #9210: module cap: omuzaki-face/painter has 3 open PRs (#821, #822, #824) (cap 3)" "face: tests/brows.rs and tests/eyes_iris.rs count against painter"
expect_line "go #9211" "face: layer stays free while system has a PR"

# broad fence: a glob that could reach any module waits on a full one, though it names no folder.
scenario broad
issue 9212 <<'B'
**Fence:** `crates/omuzaki-char/*.toml`
B
issue 9213 <<'B'
**Fence:** `crates/omuzaki-char/src/hair*/x.rs`
B
issue 9214 <<'B'
**Fence:** `crates/omuzaki-char/src/head/`
B
cat >"$FX/prs.json" <<'J'
[{"number":701,"headRefName":"cloud/701-hair","isDraft":false,"createdAt":"2026-10-01T10:00:00Z","files":[{"path":"crates/omuzaki-char/src/hair/a.rs"}],"body":""},
 {"number":702,"headRefName":"cloud/702-hair","isDraft":false,"createdAt":"2026-10-01T11:00:00Z","files":[{"path":"crates/omuzaki-char/src/hair/b.rs"}],"body":""},
 {"number":703,"headRefName":"local/703-hair","isDraft":false,"createdAt":"2026-10-01T12:00:00Z","files":[{"path":"crates/omuzaki-char/src/hair/c.rs"}],"body":""}]
J
run 9212 9213 9214
expect_line "wait #9212: module cap: omuzaki-char/hair has 3 open PRs (#701, #702, #703) (cap 3)" "broad: a glob over the crate root may reach the full module"
expect_line "wait #9213: module cap: omuzaki-char/hair has 3 open PRs (#701, #702, #703) (cap 3)" "broad: a glob folder may reach the full module"
expect_line "go #9214" "broad: a literal folder with a free module is go"

((failures == 0)) || exit 1
