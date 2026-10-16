#!/usr/bin/env bash
# Tests for tools/latency: the stimulus clip and the glass-to-glass analysis, on synthetic recordings generated at run
# time with a known injected delay (nothing is committed or decoded from a real recording).
# Run: bash tools/tests/latency.sh. Exit 0 when every case passes, 1 otherwise. Standard-library Python only.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
python3 -I -m unittest discover -s "$TESTS_DIR/../latency" -p 'test_*.py' || { echo "FAIL: latency tests" >&2; exit 1; }
echo "ok: latency tests"
