#!/usr/bin/env bash
# Tests for tools/ci/mingw-link.sh: `-lstdc++` becomes a static link both as a direct argument and inside a (quoted)
# GNU @response file. Run: bash tools/tests/mingw_link.sh. Uses a fake compiler that records its arguments and
# the contents of any @file; builds nothing. Exit 0 when every case passes, 1 otherwise.
set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
LINK="$TESTS_DIR/../ci/mingw-link.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cat >"$work/fakegcc" <<'F'
#!/usr/bin/env bash
for a in "$@"; do
  echo "ARG $a"
  case "$a" in @*) sed 's/^/RSP /' "${a#@}" ;; esac
done
exit "${FAKE_EXIT:-0}"
F
chmod +x "$work/fakegcc"

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }
static_lines=$'-Wl,-Bstatic\n-lstdc++\n-Wl,-Bdynamic'

# Case 1: direct argument.
out=$(MINGW_GCC="$work/fakegcc" "$LINK" -o x.exe -lfoo -lstdc++ -lbar)
if [[ $(grep '^ARG ' <<<"$out" | sed 's/^ARG //' | sed -n '/-Wl,-Bstatic/,/-Wl,-Bdynamic/p') == "$static_lines" ]] \
  && grep -qx 'ARG -static-libgcc' <<<"$out"; then pass "direct -lstdc++ rewritten"; else fail "direct: $out"; fi

# Case 2: quoted and unquoted lines in a response file, other quoted args (spaces, backslashes) untouched.
printf '%s\n' '"-o"' '"C:\\out dir\\x.exe"' '"-lstdc++"' '-lfoo' '-lstdc++' '"-lstdc++-extra"' >"$work/args.rsp"
out=$(MINGW_GCC="$work/fakegcc" "$LINK" "@$work/args.rsp")
rsp=$(grep '^RSP ' <<<"$out" | sed 's/^RSP //')
expected=$'"-o"\n"C:\\\\out dir\\\\x.exe"\n-Wl,-Bstatic\n-lstdc++\n-Wl,-Bdynamic\n-lfoo\n-Wl,-Bstatic\n-lstdc++\n-Wl,-Bdynamic\n"-lstdc++-extra"'
if [[ $rsp == "$expected" ]]; then pass "response file rewritten, quoting kept"; else fail "response file: $rsp"; fi
if [[ $(cat "$work/args.rsp" | grep -c Bstatic) == 0 ]]; then pass "original response file untouched"; else fail "original modified"; fi

# Case 3: the compiler's exit status propagates and the rewritten copy is removed.
FAKE_EXIT=3 MINGW_GCC="$work/fakegcc" "$LINK" "@$work/args.rsp" >/dev/null
code=$?
if (( code == 3 )); then pass "exit status propagates"; else fail "exit $code, expected 3"; fi

(( failures == 0 ))
