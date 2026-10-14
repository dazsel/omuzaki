#!/usr/bin/env bash
# Acceptance check: the spike carries no espeak-ng (GPL-3.0-or-later) and no sherpa-onnx.
#
#   tools/check-no-espeak.sh    # from spikes/speech: builds with symbols kept and inspects the binary
#
# Four independent checks, each of which must find nothing: the resolved dependency tree, the symbol table of the
# binary, its strings, and its dynamic dependencies. A positive control proves the inspection can see the C++ side:
# whisper.cpp's own symbols must be found in the same binary, otherwise "no espeak" would mean nothing.
set -uo pipefail
cd "$(dirname "$0")/.."

PATTERN='espeak|sherpa|piper_phonemize|phonemizer|ucd_'
status=0
fail() { echo "FAIL: $*"; status=1; }

echo "== build (release profile, symbols kept)"
CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=0 cargo build --release --bin speech-bench || { echo "build failed"; exit 2; }
# The target directory may be redirected, so ask cargo where the binary went instead of guessing.
BIN=$(cargo metadata --format-version 1 --no-deps | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/release/speech-bench

echo "== 1. dependency tree (all targets, all features)"
if cargo tree --target all --all-features -e normal,build,dev | grep -iE "$PATTERN"; then fail "a speech-GPL crate is in the tree"; else echo "ok: none"; fi

echo "== 2. symbol table"
syms=$(nm -C "$BIN" 2>/dev/null)
echo "symbols: $(wc -l <<<"$syms")"
if grep -ciE 'whisper_full_with_state' <<<"$syms" >/dev/null; then echo "control ok: whisper.cpp symbols are visible"; else fail "control: whisper_full_with_state not found, nm cannot see the C++ code"; fi
if grep -iE "$PATTERN" <<<"$syms" | head -5 | grep .; then fail "espeak/sherpa symbols present"; else echo "ok: none"; fi

echo "== 3. strings"
if strings -a "$BIN" | grep -iE "$PATTERN" | head -5 | grep .; then fail "espeak/sherpa strings present"; else echo "ok: none"; fi

echo "== 4. dynamic dependencies"
ldd "$BIN"
if ldd "$BIN" | grep -iE "$PATTERN"; then fail "linked against espeak/sherpa"; else echo "ok: none"; fi

echo "== licence of what the binary links (cargo deny, the repo policy)"
cargo deny --config ../../deny.toml check licenses bans sources || fail "cargo deny"

[[ $status -eq 0 ]] && echo "ALL CHECKS PASSED" || echo "CHECKS FAILED"
exit $status
