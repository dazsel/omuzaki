#!/usr/bin/env bash
# Tests for tools/ci/crates.py, with a stub `cargo` that prints fixed workspace metadata, so no real workspace is read.
# Run: bash tools/tests/crates_py.sh. Exit 0 when every case passes, 1 otherwise.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CRATES="$TESTS_DIR/../ci/crates.py"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# Three packages: one targets wasm by default, one opts out, one opts in explicitly.
mkdir -p "$work/bin"
cat >"$work/bin/cargo" <<'S'
#!/usr/bin/env bash
cat <<'J'
{"packages":[
 {"name":"c-ui","metadata":{"omuzaki":{"wasm":true}}},
 {"name":"a-core","metadata":null},
 {"name":"b-audio","metadata":{"omuzaki":{"wasm":false}}}
]}
J
S
chmod +x "$work/bin/cargo"
run() { PATH="$work/bin:$PATH" python3 -I "$CRATES" "$@"; }

# The CI step reads the output with mapfile; an empty result must give an empty array, not one empty name (#1262).
mapfile -t got < <(run wasm b-audio)
if ((${#got[@]} == 0)); then pass "only opted-out crates: nothing, so mapfile reads no names"; else fail "only opted-out crates gave ${#got[@]} entries: '${got[*]}'"; fi

mapfile -t got < <(run wasm b-audio a-core)
if [[ ${#got[@]} == 1 && ${got[0]} == a-core ]]; then pass "a mix: only the wasm crate"; else fail "a mix gave '${got[*]}'"; fi

got=$(run wasm)
if [[ $got == $'a-core\nc-ui' ]]; then pass "no names: every wasm crate, sorted"; else fail "no names gave '$got'"; fi

got=$(run members)
if [[ $got == $'a-core\nb-audio\nc-ui' ]]; then pass "members: every package, sorted"; else fail "members gave '$got'"; fi

((failures == 0)) || exit 1
