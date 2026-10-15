#!/usr/bin/env bash
# gh-read-token.sh: print a short-lived, read-only GitHub token for dazsel/omuzaki from the GitHub App `omuzaki-read`.
#
# Why: every lead and session shares one account's 5,000 REST calls an hour, and the fleet ran it out on 2026-10-15.
# The read-only App has its own budget, so bulk reads (open pull requests, issues, CI runs) move to it; writes stay on
# the session's normal GitHub access. The App can only read, and the repository is public, so a leaked token or key
# exposes nothing that isn't already public.
#
# Needs, from the cloud environment's settings (never from the repository):
#   OMUZAKI_READ_APP_ID       the App's ID
#   OMUZAKI_READ_APP_KEY_B64  its private key (the .pem file), base64-encoded on one line
# Use:   export GH_TOKEN=$(tools/gh-read-token.sh)   # then read-only `gh api ...` / curl calls use the App's budget
#        tools/gh-read-token.sh --check              # does the token reach GitHub as the App, and how much is left?
# The token lasts an hour; it is cached under $TMPDIR and reused while more than 10 minutes remain. Never echo it.
# Exit 1 with a message on stderr when the variables are missing or GitHub refuses.
set -uo pipefail
REPO=dazsel/omuzaki
CACHE=${TMPDIR:-/tmp}/omuzaki-read-token
api() { curl -sf --max-time 30 -H "Accept: application/vnd.github+json" "$@"; }

token() {
  local now exp tok
  now=$(date +%s)
  if [ -f "$CACHE" ] && read -r exp tok < "$CACHE" && [ -n "${tok:-}" ] && [ "${exp:-0}" -gt $((now + 600)) ]; then
    printf '%s' "$tok"; return 0
  fi
  [ -n "${OMUZAKI_READ_APP_ID:-}" ] && [ -n "${OMUZAKI_READ_APP_KEY_B64:-}" ] \
    || { echo "gh-read-token: OMUZAKI_READ_APP_ID and OMUZAKI_READ_APP_KEY_B64 must be set" >&2; return 1; }
  local key b64 header payload sig jwt inst resp
  key=$(mktemp) && chmod 600 "$key" && trap 'rm -f "$key"' RETURN
  printf '%s' "$OMUZAKI_READ_APP_KEY_B64" | base64 -d > "$key" 2>/dev/null \
    || { echo "gh-read-token: OMUZAKI_READ_APP_KEY_B64 is not valid base64" >&2; return 1; }
  b64() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
  header=$(printf '{"alg":"RS256","typ":"JWT"}' | b64)
  payload=$(printf '{"iat":%d,"exp":%d,"iss":"%s"}' $((now - 60)) $((now + 540)) "$OMUZAKI_READ_APP_ID" | b64)
  sig=$(printf '%s.%s' "$header" "$payload" | openssl dgst -sha256 -sign "$key" -binary | b64) \
    || { echo "gh-read-token: signing failed (is the key a PEM private key?)" >&2; return 1; }
  jwt="$header.$payload.$sig"
  inst=$(api -H "Authorization: Bearer $jwt" "https://api.github.com/repos/$REPO/installation" \
         | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["id"])') \
    || { echo "gh-read-token: GitHub refused the App (wrong ID or key, or not installed on $REPO)" >&2; return 1; }
  resp=$(api -X POST -H "Authorization: Bearer $jwt" "https://api.github.com/app/installations/$inst/access_tokens") \
    || { echo "gh-read-token: GitHub refused to issue a token" >&2; return 1; }
  read -r tok exp < <(printf '%s' "$resp" | python3 -I -c '
import json, sys, datetime
d = json.load(sys.stdin)
print(d["token"], int(datetime.datetime.fromisoformat(d["expires_at"].replace("Z", "+00:00")).timestamp()))') \
    || { echo "gh-read-token: unexpected token response" >&2; return 1; }
  (umask 077; printf '%s %s\n' "$exp" "$tok" > "$CACHE")
  printf '%s' "$tok"
}

if [ "${1:-}" = --check ]; then
  tok=$(token) || exit 1
  # Only an App token can list its installation's repositories; a token swapped for the user's on the way is refused.
  if api -H "Authorization: Bearer $tok" "https://api.github.com/installation/repositories" >/dev/null; then
    left=$(api -H "Authorization: Bearer $tok" https://api.github.com/rate_limit \
           | python3 -I -c 'import json,sys; c=json.load(sys.stdin)["resources"]["core"]; print("%s/%s" % (c["remaining"], c["limit"]))')
    echo "ok: the token reaches GitHub as the App; App budget left: $left"
  else
    echo "not ok: GitHub didn't accept the token as the App (a proxy may be replacing the Authorization header)" >&2
    exit 1
  fi
  exit 0
fi
token
