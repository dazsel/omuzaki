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
        cat "$DISPATCH_FIXTURES/files-$number.json" 2>/dev/null || echo '[]'
        ;;
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
    ("Fence: `tools`, `only`", root, ["tools"], []),
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

# closing references come from GitHub, not the body: "Does not close #42" is not a reference, so the PR is other.
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

# a missing fixture is a gh failure: exit 2 with a message, not a crash.
scenario missing
echo '[]' >"$FX/prs.json"
run 9999
expect_code 2 "missing issue: exit 2"

((failures == 0)) || exit 1
