#!/usr/bin/env bash
# Tests for tools/models/mediapipe-face.sh: it never deletes a --venv folder it did not create. Run:
# bash tools/tests/mediapipe_face_venv.sh. A fake `curl` that fails stands in for the network, so nothing is downloaded.
# Exit 0 when every case passes, 1 otherwise.
set -uo pipefail

SCRIPT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../models" && pwd)/mediapipe-face.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
printf '#!/bin/sh\nexit 22\n' >"$work/bin/curl"
chmod +x "$work/bin/curl"

failures=0
check() { # name, condition exit status
  if [ "$2" -eq 0 ]; then echo "ok: $1"; else echo "FAIL: $1" >&2; failures=$((failures + 1)); fi
}
run() { PATH="$work/bin:$PATH" bash "$SCRIPT" "$@" >/dev/null 2>&1; }

mkdir "$work/existing" && echo keep >"$work/existing/data"
run --out "$work/out1" --venv "$work/existing"; status=$?
check "an existing --venv is refused" "$([ "$status" -ne 0 ] && echo 0 || echo 1)"
check "an existing --venv keeps its files" "$([ -f "$work/existing/data" ] && echo 0 || echo 1)"

mkdir -p "$work/parent/out" && echo keep >"$work/parent/data"
run --out "$work/parent/out" --venv "$work/parent"; status=$?
check "a --venv holding --out is refused" "$([ "$status" -ne 0 ] && echo 0 || echo 1)"
check "a --venv holding --out keeps its files" "$([ -f "$work/parent/data" ] && echo 0 || echo 1)"

repo=$(cd "$(dirname "$SCRIPT")/../.." && pwd)
run --out "$repo/tools/models/should-not-exist" --venv "$work/v3"; status=$?
check "an --out inside the repo is refused" "$([ "$status" -ne 0 ] && echo 0 || echo 1)"
check "an --out inside the repo is not created" "$([ ! -e "$repo/tools/models/should-not-exist" ] && echo 0 || echo 1)"

run --out "$work/out2" --venv "$work/fresh"; status=$?
check "a failed download fails the script" "$([ "$status" -ne 0 ] && echo 0 || echo 1)"
check "a --venv the run never created is not left behind" "$([ ! -e "$work/fresh" ] && echo 0 || echo 1)"

[ "$failures" -eq 0 ]
