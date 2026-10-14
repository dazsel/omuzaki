#!/usr/bin/env bash
# Tests for tools/ci/evidence.py: identical renders compare as identical, a one-level change shows in the statistics and
# the difference picture, and a case the base cannot render is reported rather than fatal.
# Run: bash tools/tests/evidence.sh. Needs Pillow (the visual-evidence job installs it); skips without.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
EVIDENCE="$TESTS_DIR/../ci/evidence.py"
if ! python3 -c 'import PIL' 2>/dev/null; then echo "skip: Pillow is not installed"; exit 0; fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

mkdir -p "$work/base" "$work/head/crates/x"
cat >"$work/head/crates/x/evidence.toml" <<'T'
[[case]]
name = "flat"
env = { OUT = "{out}" }
steps = [{ run = ["python3", "-c", "import os; from PIL import Image; v = int(open('level').read()); Image.new('RGB', (8, 8), (9, v, 9)).save(os.environ['OUT'] + '/p.png')"] }]
images = ["p.png"]
T
export RUNNER_TEMP="$work/tmp"

echo 40 >"$work/base/level"; echo 40 >"$work/head/level"
python3 -I "$EVIDENCE" run --crates x --before "$work/base" --after "$work/head" --out "$work/same" >/dev/null
if grep -q 'identical | 0 |' "$work/same/comment.md"; then pass "identical renders are reported identical"; else fail "identical: $(cat "$work/same/comment.md")"; fi

echo 41 >"$work/head/level"
python3 -I "$EVIDENCE" run --crates x --before "$work/base" --after "$work/head" --out "$work/diff" >/dev/null
if grep -q '100.000 % | 1 |' "$work/diff/comment.md"; then pass "a one-level change shows in the table"; else fail "change: $(cat "$work/diff/comment.md")"; fi
if python3 -c "from PIL import Image; import sys; sys.exit(0 if Image.open('$work/diff/x/flat/p.diff.png').getpixel((0, 0))[0] > 0 else 1)"; then
  pass "the difference picture is lit"; else fail "difference picture is black"; fi

rm "$work/base/level"
python3 -I "$EVIDENCE" run --crates x --before "$work/base" --after "$work/head" --out "$work/new" >/dev/null 2>&1
if grep -q 'could not render it' "$work/new/comment.md"; then pass "a case the base cannot render is reported"; else fail "new case: $(cat "$work/new/comment.md")"; fi

((failures == 0))
