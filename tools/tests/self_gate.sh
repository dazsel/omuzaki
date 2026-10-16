#!/usr/bin/env bash
# Tests for tools/ci/self-gate.py: who can attest, which head the attestation must name, and which pushes skip CI.
# Run: bash tools/tests/self_gate.sh. Exit 0 when every case passes, 1 otherwise.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
GATE="$TESTS_DIR/../ci/self-gate.py"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }
# check HEAD [ALLOWED]: the exit code of the check against $work/comments.json.
check() { python3 -I "$GATE" check --heads "$1" --allowed "${2:-}" --comments "$work/comments.json" >/dev/null 2>&1; }
expect_ok() { if check "${@:2}"; then pass "$1"; else fail "$1: wanted pass"; fi; }
expect_red() { if check "${@:2}"; then fail "$1: wanted fail"; else pass "$1"; fi; }
# hosted RUNS_JSON FILES_JSON HEAD: the exit code of a check with no comments and a hosted-run fixture.
hosted() { printf '{"runs":%s,"files":%s}' "$1" "$2" >"$work/hosted.json"; printf '[]' >"$work/comments.json"
    python3 -I "$GATE" check --heads "$3" --comments "$work/comments.json" --hosted "$work/hosted.json" >/dev/null 2>&1; }
# value KEY: the value of KEY in the last `heads` output.
value() { sed -n "s/^$1=//p" <<<"$out"; }
expect() { if [[ $(value "$1") == "$2" ]]; then pass "$3"; else fail "$3: $1 was '$(value "$1")', wanted '$2'"; fi; }

HEAD=aaaaaaa1111111111111111111111111111111111
OTHER=bbbbbbb2222222222222222222222222222222222
line() { printf 'Ready.\n\nSelf-gate: pass at %s (cargo xtask check --affected, 12 tests)' "$1"; }
# comment ID LOGIN ASSOCIATION BODY
comment() { python3 -I -c 'import json,sys; print(json.dumps({"id":int(sys.argv[1]),"user":{"login":sys.argv[2]},"author_association":sys.argv[3],"body":sys.argv[4]}))' "$@"; }
comments() { printf '[%s]' "$(IFS=,; echo "$*")" >"$work/comments.json"; }

comments "$(comment 1 maint MEMBER "$(line aaaaaaa)")"
expect_ok "a member's line naming the head passes" "$HEAD"
expect_red "a line naming another commit is stale" "$OTHER"

comments "$(comment 1 maint MEMBER "$(line aaaaaaa)")" "$(comment 2 maint MEMBER "$(line bbbbbbb)")"
expect_red "the latest line wins over an earlier one for the head" "$HEAD"
expect_ok "the latest line wins: it names the other commit" "$OTHER"

comments "$(comment 2 maint MEMBER "$(line aaaaaaa)")" "$(comment 1 maint MEMBER "$(line bbbbbbb)")"
expect_ok "latest means highest comment id, not list order" "$HEAD"

comments "$(comment 1 stranger NONE "$(line aaaaaaa)")"
expect_red "an outsider cannot attest" "$HEAD"
comments "$(comment 1 stranger CONTRIBUTOR "$(line aaaaaaa)")"
expect_red "a past contributor cannot attest" "$HEAD"
comments "$(comment 1 maint MEMBER "$(line bbbbbbb)")" "$(comment 2 stranger NONE "$(line aaaaaaa)")"
expect_red "an outsider's newer line neither attests nor hides the maintainer's stale one" "$HEAD"
expect_ok "a login in PR_ALLOWED_AUTHORS attests" "$HEAD" "x,stranger"
comments "$(comment 1 stranger NONE "$(line aaaaaaa)")"
expect_ok "an allowed login with a private membership attests" "$HEAD" "x,stranger"
expect_red "a login that only contains an allowed one does not" "$HEAD" "strange"

comments "$(comment 1 maint OWNER "$(line aaaaaaa)")"
expect_ok "the owner attests" "$HEAD"
comments "$(comment 1 maint COLLABORATOR "$(line aaaaaaa)")"
expect_ok "a collaborator attests" "$HEAD"
comments "$(comment 1 maint MEMBER '> Self-gate: pass at aaaaaaa (quoted)')"
expect_red "a quoted line does not attest" "$HEAD"
comments "$(comment 1 maint MEMBER 'Self-gate: fail at aaaaaaa (cargo xtask check --affected)')"
expect_red "a failing line does not attest" "$HEAD"
comments "$(comment 1 maint MEMBER 'Self-gate: pass at aaaaaa (too short)')"
expect_red "an abbreviation under seven characters does not attest" "$HEAD"
comments
expect_red "no comments, no attestation" "$HEAD"
comments "$(comment 1 maint MEMBER "$(line aaaaaaa)")"
expect_ok "any owned head counts" "$OTHER $HEAD"

# Owned heads, on a real repository: a PR branch that merges main twice, then a merge that resolves a change.
repo="$work/repo"
git init -q -b main "$repo"
g() { git -C "$repo" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false "$@"; }
echo base >"$repo/base.txt"; echo pr >"$repo/pr.txt"; g add -A; g commit -qm base
g checkout -qb pr
echo own >"$repo/pr.txt"; g commit -qam own; own=$(g rev-parse HEAD)
g checkout -q main; echo main1 >"$repo/base.txt"; g commit -qam main1
g checkout -q pr; g merge -q --no-ff -m "merge main" main; merge1=$(g rev-parse HEAD)
g checkout -q main; echo main2 >"$repo/other.txt"; g add -A; g commit -qm main2
g checkout -q pr; g merge -q --no-ff -m "merge main again" main; merge2=$(g rev-parse HEAD)
heads() { out=$(cd "$repo" && python3 -I "$GATE" heads "$@"); }

heads "$merge2" main "$merge1"
expect owned "$merge2 $merge1 $own" "merges of main keep every earlier head owned"
expect skip true "a push of only a main merge skips CI"
heads "$merge2" main "$own"
expect skip true "a push of several main merges skips CI"
heads "$merge2" main "$(printf 'f%.0s' {1..40})"
expect skip false "a push over an unrelated head does not skip"
heads "$merge2" main
expect skip false "a run with no previous head (opened, ready) does not skip"

# A merge that changes a file the PR also changed, and a PR commit on top, are both the PR's own work.
echo own2 >"$repo/pr.txt"; g commit -qam own2; own2=$(g rev-parse HEAD)
heads "$own2" main "$merge2"
expect owned "$own2" "a commit of the PR's own is the only owned head"
expect skip false "a push with new work does not skip"

g checkout -q main; echo main3 >"$repo/pr.txt"; g commit -qam main3
g checkout -q pr; g merge -q -m "merge main3" main >/dev/null 2>&1 || true
echo resolved >"$repo/pr.txt"; g add -A; g commit -qm "merge main3 (resolved)"; resolved=$(g rev-parse HEAD)
heads "$resolved" main "$own2"
expect skip false "a merge that resolves a conflict is not a main-only merge"
expect owned "$resolved" "a resolved merge starts a new owned head"

# A conflict resolved to either parent's version equals that parent, yet still discards work: not main-only either.
conflict() { # NAME RESOLUTION: PR and main both change conflict.txt; the merge keeps `ours` (PR) or `theirs` (main).
    g checkout -q -b "$1-pr" "$resolved"; echo "pr-$1" >"$repo/conflict.txt"; g add -A; g commit -qm "pr $1"
    local before; before=$(g rev-parse HEAD)
    g checkout -q main; echo "main-$1" >"$repo/conflict.txt"; g add -A; g commit -qm "main $1"
    g checkout -q "$1-pr"; g merge -q -m "merge main ($1)" -X "$2" main >/dev/null 2>&1
    heads "$(g rev-parse HEAD)" main "$before"
}
conflict keep-main theirs
expect skip false "a conflict resolved to main's version is not a main-only merge"
conflict keep-pr ours
expect skip false "a conflict resolved to the PR's version is not a main-only merge"

# A merge whose second parent is not on main is the PR's own work too.
g checkout -q -b side "$own"; echo side >"$repo/side.txt"; g add -A; g commit -qm side
g checkout -q pr; g merge -q --no-ff -m "merge side" side; sidemerge=$(g rev-parse HEAD)
heads "$sidemerge" main "$resolved"
expect skip false "merging a branch that is not main is not skipped"

# Hosted runs: one good run, then each rule broken in turn.
run() { # EVENT PATH SHA CONCLUSION TESTJOB_CONCLUSION
    printf '[{"id":7,"event":"%s","path":"%s","head_sha":"%s","conclusion":"%s","jobs":[{"name":"plan","conclusion":"success"},{"name":"test (1)","conclusion":"success"},{"name":"test (2)","conclusion":"%s"}]}]' "$@"
}
DOCS='["docs/a.md"]'
GOOD=$(run workflow_dispatch .github/workflows/ci.yml "$HEAD" success success)
hosted "$GOOD" "$DOCS" "$HEAD" && pass "a successful hosted run on the head passes" || fail "hosted pass case"
hosted "$(run pull_request .github/workflows/ci.yml "$HEAD" success success)" "$DOCS" "$HEAD" \
    && fail "(a) wrong event" || pass "(a) a run of another event does not count"
hosted "$(run workflow_dispatch .github/workflows/nightly.yml "$HEAD" success success)" "$DOCS" "$HEAD" \
    && fail "(b) wrong workflow" || pass "(b) another workflow does not count"
hosted "$GOOD" "$DOCS" "$OTHER" && fail "(c) wrong head" || pass "(c) a run on another head does not count"
hosted "$(run workflow_dispatch .github/workflows/ci.yml "$HEAD" failure success)" "$DOCS" "$HEAD" \
    && fail "(d) failed run" || pass "(d) a failed run does not count"
hosted "$(run workflow_dispatch .github/workflows/ci.yml "$HEAD" success skipped)" "$DOCS" "$HEAD" \
    && fail "(d) skipped test" || pass "(d) a skipped test job does not count"
hosted "$(run workflow_dispatch .github/workflows/ci.yml "$HEAD" success cancelled)" "$DOCS" "$HEAD" \
    && fail "(d) cancelled test" || pass "(d) a cancelled test job does not count"
hosted "$(run workflow_dispatch .github/workflows/ci.yml@refs/heads/x "$HEAD" success success)" "$DOCS" "$HEAD" \
    && pass "a ref-qualified workflow path counts" || fail "ref-qualified path"
EMPTY='[{"id":8,"event":"workflow_dispatch","path":".github/workflows/ci.yml","head_sha":"'$HEAD'","conclusion":"success","jobs":[{"name":"plan","conclusion":"success"},{"name":"test (${{ matrix.name }})","conclusion":"skipped"}]}]'
hosted "$EMPTY" "$DOCS" "$HEAD" && pass "an empty plan (matrix never expanded) passes" || fail "empty plan"
hosted "$EMPTY" '[".github/workflows/ci.yml"]' "$HEAD" && fail "empty plan, workflow edit" || pass "an empty plan does not excuse a workflow edit"
hosted "$GOOD" '["docs/a.md",".github/workflows/ci.yml"]' "$HEAD" \
    && fail "(e) workflow edit" || pass "(e) a PR that edits a workflow needs a human line"
hosted "$GOOD" '[".github/workflows/new.yml"]' "$HEAD" && fail "(e) other workflow" || pass "(e) any file under workflows counts"
for path in tools/ci/gate-aggregate.py tools/ci/plan-outputs.py xtask/src/affected.rs .config/nextest.toml .github/actions/setup-linux/action.yml; do
    hosted "$GOOD" "[\"$path\"]" "$HEAD" && fail "(e) $path" || pass "(e) a PR that edits $path needs a human line"
done
hosted "$GOOD" '["tools/tests/self_gate.sh","crates/a/src/lib.rs"]' "$HEAD" && pass "(e) tests and crate code do not exclude" || fail "(e) over-broad"

python3 -I "$GATE" >/dev/null 2>&1 && fail "missing arguments should be an error" || pass "missing arguments are an error"
exit $((failures > 0))
