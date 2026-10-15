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
if grep -q '100.000 % (64 px) | 1 |' "$work/diff/comment.md"; then pass "a one-level change shows in the table"; else fail "change: $(cat "$work/diff/comment.md")"; fi
if python3 -c "from PIL import Image; import sys; sys.exit(0 if Image.open('$work/diff/x/flat/p.diff.png').getpixel((0, 0))[0] > 0 else 1)"; then
  pass "the difference picture is lit"; else fail "difference picture is black"; fi

# One changed pixel in a big picture must not round to "identical".
mkdir -p "$work/sparse/crates/x"
cat >"$work/sparse/crates/x/evidence.toml" <<'T'
[[case]]
name = "sparse"
env = { OUT = "{out}" }
steps = [{ run = ["python3", "-c", "import os; from PIL import Image; i = Image.new('RGB', (2048, 2048), (9, 9, 9)); i.putpixel((5, 5), (9, int(open('level').read()), 9)); i.save(os.environ['OUT'] + '/p.png')"] }]
images = ["p.png"]
T
mkdir -p "$work/sparse-base"; echo 9 >"$work/sparse-base/level"; echo 10 >"$work/sparse/level"
python3 -I "$EVIDENCE" run --crates x --before "$work/sparse-base" --after "$work/sparse" --out "$work/sparse-out" >/dev/null
if grep -q '(1 px) | 1 |' "$work/sparse-out/comment.md"; then pass "one changed pixel is not rounded to identical"; else fail "sparse: $(cat "$work/sparse-out/comment.md")"; fi

# A change confined to a tool reaches no crate (--crates is empty), yet its case must still run.
mkdir -p "$work/tool/crates/x" "$work/tool/tools"
cat >"$work/tool/crates/x/evidence.toml" <<'T'
[[case]]
name = "drawn"
env = { OUT = "{out}" }
steps = [{ run = ["python3", "tools/draw.py", "{out}/p.png"] }]
images = ["p.png"]
T
cat >"$work/tool/tools/draw.py" <<'T'
import sys
from PIL import Image
Image.new("RGB", (4, 4), (1, 2, 3)).save(sys.argv[1])
T
echo "tools/draw.py" >"$work/changed.txt"
python3 -I "$EVIDENCE" run --crates "" --changed "$work/changed.txt" --before "$work/tool" --after "$work/tool" --out "$work/tool-out" >/dev/null
if grep -q 'x/drawn' "$work/tool-out/comment.md"; then pass "a tools-only change selects the case that runs the tool"; else fail "tools-only: $(cat "$work/tool-out/comment.md")"; fi
echo "README.md" >"$work/changed.txt"
python3 -I "$EVIDENCE" run --crates "" --changed "$work/changed.txt" --before "$work/tool" --after "$work/tool" --out "$work/none-out" >/dev/null
if grep -q 'No declared cases' "$work/none-out/comment.md"; then pass "an unrelated change selects nothing"; else fail "unrelated: $(cat "$work/none-out/comment.md")"; fi

rm "$work/base/level"
python3 -I "$EVIDENCE" run --crates x --before "$work/base" --after "$work/head" --out "$work/new" >/dev/null 2>&1
if grep -q 'could not render it' "$work/new/comment.md"; then pass "a case the base cannot render is reported"; else fail "new case: $(cat "$work/new/comment.md")"; fi

((failures == 0))
