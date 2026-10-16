#!/usr/bin/env bash
# Tests for tools/gh-read-token.sh against a local fake: `curl` and (for one case) `openssl` are stubs on PATH, so no
# network, no real App and no real key. The key is a throwaway generated here and deleted on exit. Nothing prints a
# token or a key; the fake token is the literal string FAKETOKEN and is only compared, never shown.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
SCRIPT=$HERE/../gh-read-token.sh
W=$(mktemp -d) && trap 'rm -rf "$W"' EXIT
mkdir "$W/bin" "$W/tmp"
REAL_OPENSSL=$(command -v openssl)
fails=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fails=$((fails + 1)); }
check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }

openssl genrsa -out "$W/key.pem" 2048 2>/dev/null || { echo "need openssl to make a test key"; exit 1; }
export OMUZAKI_READ_APP_ID=123 OMUZAKI_READ_APP_KEY_B64=$(base64 < "$W/key.pem" | tr -d '\n')
unset XDG_RUNTIME_DIR
export TMPDIR=$W/tmp FAKE_LOG=$W/log FAKE_BODY=$W/body FAKE_ARGS=$W/args FAKE_MODE=good

# Fake curl: logs "METHOD URL", every argument (to FAKE_ARGS) and the POST body; the Authorization header is only
# accepted on stdin (`-H @-`), where it is classified as the fake token or a JWT and never logged. FAKE_MODE picks a failure.
cat > "$W/bin/curl" <<'FAKE'
#!/usr/bin/env bash
method=GET url= body=
printf '%s\n' "$@" >> "$FAKE_ARGS"
while [ $# -gt 0 ]; do
  case $1 in
    -X) method=$2; shift ;;
    -d) body=$2; shift ;;
    -H) if [ "$2" = @- ]; then
          IFS= read -r hdr
          case $hdr in
            "Authorization: Bearer FAKETOKEN") echo "auth-stdin-token" >> "$FAKE_LOG" ;;
            "Authorization: Bearer "?*) echo "auth-stdin-jwt" >> "$FAKE_LOG" ;;
          esac
        fi
        shift ;;
    --max-time) shift ;;
    http*) url=$1 ;;
  esac
  shift
done
echo "$method $url" >> "$FAKE_LOG"
[ -n "$body" ] && printf '%s' "$body" > "$FAKE_BODY"
case $url in
  */repos/*/installation) printf '{"id":42}' ;;
  */access_tokens)
    [ "$FAKE_MODE" = refuse ] && exit 22
    exp=$(date -u -d '+1 hour' +%Y-%m-%dT%H:%M:%SZ)
    case $FAKE_MODE in
      write) perm='"contents":"write"'; repos='[{"full_name":"dazsel/omuzaki"}]' ;;
      tworepos) perm='"contents":"read"'; repos='[{"full_name":"dazsel/omuzaki"},{"full_name":"dazsel/other"}]' ;;
      *) perm='"contents":"read"'; repos='[{"full_name":"dazsel/omuzaki"}]' ;;
    esac
    printf '{"token":"FAKETOKEN","expires_at":"%s","permissions":{%s},"repositories":%s}' "$exp" "$perm" "$repos" ;;
  */installation/repositories) printf '{}' ;;
  */rate_limit)
    case $FAKE_MODE in ratefail) exit 22 ;; ratebad) printf 'not json' ;; *) printf '{"resources":{"core":{"remaining":4999,"limit":5000}}}' ;; esac ;;
esac
FAKE
chmod +x "$W/bin/curl"
export PATH=$W/bin:$PATH
run() { "$SCRIPT" "$@" 2>"$W/err" >"$W/out"; }
fresh() { rm -rf "${W:?}/tmp/"* "$W/log" "$W/body" "$W/args"; }
DIR=$W/tmp/omuzaki-read-token-$(id -u)
CACHE=$DIR/cache

bash -n "$SCRIPT" && ok "bash -n" || bad "bash -n"

# Mint: scoped body, token on stdout, private cache.
fresh; run
check "mint prints the token" '[ "$(cat "$W/out")" = FAKETOKEN ]'
check "mint requests read-only permissions and the one repository" 'grep -q "\"repositories\":\[\"omuzaki\"\]" "$W/body" && ! grep -q write "$W/body"'
check "cache is mode 600 in a 700 directory" '[ "$(stat -c %a "$CACHE")" = 600 ] && [ "$(stat -c %a "$DIR")" = 700 ]'
check "no temp files left" '[ "$(ls "$DIR" | wc -l)" = 1 ] && [ "$(ls "$W/tmp" | wc -l)" = 1 ]'
check "the JWT went to curl on stdin, never in its arguments" 'grep -q auth-stdin-jwt "$W/log" && ! grep -qi -e bearer -e "eyJ" "$W/args"'
check "stderr is empty and holds no token" '[ ! -s "$W/err" ]'

# Cache hit: no further mint.
: > "$W/log"; run
check "cache hit returns the token without calling GitHub" '[ "$(cat "$W/out")" = FAKETOKEN ] && [ ! -s "$W/log" ]'

# Cache rejected: wrong mode, symlink, expired.
chmod 644 "$CACHE"; : > "$W/log"; run
check "group/world-readable cache is not trusted; re-mint rewrites it 600" 'grep -q access_tokens "$W/log" && [ "$(stat -c %a "$CACHE")" = 600 ]'
rm -f "$CACHE"; printf '%s FAKETOKEN\n' "$(( $(date +%s) + 3000 ))" > "$W/real"; chmod 600 "$W/real"; ln -s "$W/real" "$CACHE"; : > "$W/log"; run
check "symlink cache is not trusted and is replaced by a file" 'grep -q access_tokens "$W/log" && [ ! -L "$CACHE" ]'
printf '%s FAKETOKEN\n' "$(( $(date +%s) + 100 ))" > "$CACHE"; chmod 600 "$CACHE"; : > "$W/log"; run
check "cache close to expiry is re-minted" 'grep -q access_tokens "$W/log"'

# Scope validation and refusal.
for m in write tworepos; do fresh; FAKE_MODE=$m run; rc=$?
  check "token with $m scope is refused, nothing cached" '[ $rc -ne 0 ] && [ ! -e "$CACHE" ] && grep -q refusing "$W/err" && ! grep -q FAKETOKEN "$W/out"'; done
fresh; FAKE_MODE=refuse run; rc=$?
check "GitHub refusing the token fails with a message" '[ $rc -ne 0 ] && grep -q "refused to issue" "$W/err"'
fresh; OMUZAKI_READ_APP_ID= run; rc=$?
check "missing variables fail with a message" '[ $rc -ne 0 ] && grep -q "must be set" "$W/err"'
fresh; OMUZAKI_READ_APP_KEY_B64='!!notbase64' run; rc=$?
check "bad key fails and leaves no key file" '[ $rc -ne 0 ] && [ -z "$(ls "$DIR")" ]'

# Interrupt during signing: the fake openssl sends TERM to the script (its grandparent), then exits; the key must not survive.
cat > "$W/bin/openssl" <<FAKE
#!/usr/bin/env bash
if [ "\$1" = dgst ]; then kill -TERM \$(ps -o ppid= -p \$PPID); cat >/dev/null; exit 0; fi
exec "$REAL_OPENSSL" "\$@"
FAKE
chmod +x "$W/bin/openssl"
# The runner may start us with TERM ignored or blocked (an ignored signal can't be trapped), so restore it first.
fresh; python3 -I -c 'import os, signal, sys; signal.signal(signal.SIGTERM, signal.SIG_DFL); signal.pthread_sigmask(signal.SIG_UNBLOCK, [signal.SIGTERM]); os.execv(sys.argv[1], sys.argv[1:])' "$SCRIPT" 2>"$W/err" >"$W/out"; rc=$?
check "TERM while the key is on disk: non-zero exit, no key file left" '[ $rc -ne 0 ] && [ -z "$(ls "$DIR")" ]'
rm "$W/bin/openssl"

# The cache directory is verified, and refused when it is not private.
fresh; mkdir -m 755 "$DIR"; run; rc=$?
check "a group/world-readable cache directory is refused before any call" '[ $rc -ne 0 ] && grep -q "not a private directory" "$W/err" && [ ! -e "$W/log" ] && [ ! -s "$W/out" ]'
fresh; mkdir "$W/elsewhere" && chmod 700 "$W/elsewhere"; ln -s "$W/elsewhere" "$DIR"; run; rc=$?
check "a symlinked cache directory is refused, nothing written through it" '[ $rc -ne 0 ] && grep -q "not a private directory" "$W/err" && [ -z "$(ls "$W/elsewhere")" ]'
rm -rf "$W/elsewhere"
fresh; mkdir "$W/xdg" && chmod 700 "$W/xdg"; XDG_RUNTIME_DIR=$W/xdg run; rc=$?
check "XDG_RUNTIME_DIR is used when set" '[ $rc -eq 0 ] && [ "$(stat -c %a "$W/xdg/omuzaki-read-token/cache")" = 600 ] && [ ! -e "$DIR" ]'

# --check exit paths.
fresh; run --check; rc=$?
check "--check ok shows the budget" '[ $rc -eq 0 ] && grep -q "budget left: 4999/5000" "$W/out"'
check "--check sent the token on stdin, never in arguments" '[ "$(grep -c auth-stdin-token "$W/log")" = 2 ] && ! grep -q -e FAKETOKEN -e Bearer "$W/args"'
for m in ratefail ratebad; do FAKE_MODE=$m run --check; rc=$?
  check "--check fails when the rate-limit lookup fails ($m)" '[ $rc -ne 0 ] && grep -q "rate-limit lookup failed" "$W/err" && ! grep -q "^ok" "$W/out"'; done
fresh; FAKE_MODE=refuse run --check; rc=$?
check "--check fails when no token can be minted" '[ $rc -ne 0 ]'

[ "$fails" -eq 0 ] && echo "all passed" || { echo "$fails failed"; exit 1; }
