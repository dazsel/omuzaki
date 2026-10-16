#!/usr/bin/env bash
# gh-read-token.sh: print a short-lived, read-only GitHub token for dazsel/omuzaki from the GitHub App `omuzaki-read`.
#
# Why: every lead and session shares one account's 5,000 REST calls an hour, and the fleet ran it out on 2026-10-15.
# The read-only App has its own budget, so bulk reads (open pull requests, issues, CI runs) move to it; writes stay on
# the session's normal GitHub access. The token is scoped when it is minted: read-only permissions and this one
# repository, whatever else the installation is later granted.
#
# SECRET: the App private key and the minted token are credentials. Never print, log, echo or commit either. The key
# can sign for the App and mint tokens until it is rotated, so a disclosed key means rotate it, even though the
# repository is public.
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
DIR= CACHE=
# What the minted token may do. Edit here (and in the App's settings) if bulk reads need another read permission.
BODY='{"repositories":["omuzaki"],"permissions":{"actions":"read","checks":"read","contents":"read","issues":"read","metadata":"read","pull_requests":"read","statuses":"read"}}'
KEYFILE= CTMP=
cleanup() { [ -n "$KEYFILE" ] && rm -f "$KEYFILE"; [ -n "$CTMP" ] && rm -f "$CTMP"; return 0; }
api() { curl -sf --max-time 30 -H "Accept: application/vnd.github+json" "$@"; }
# Calls with a credential: the Authorization header goes to curl on stdin (printf is a builtin), so neither the JWT
# nor the token ever appears in a process's arguments.
api_auth() { local cred=$1; shift; printf 'Authorization: Bearer %s\n' "$cred" | api -H @- "$@"; }
b64() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
# private PATH TYPE MODE: PATH is a real (not linked) file or directory, owned by us, with exactly this mode.
private() {
  local m o
  [ "$2" "$1" ] && [ ! -L "$1" ] || return 1
  read -r m o < <(stat -c '%a %u' "$1" 2>/dev/null || stat -f '%Lp %u' "$1" 2>/dev/null) || return 1
  [ "$m" = "$3" ] && [ "$o" = "$(id -u)" ]
}
# The cache and the key tempfile live in a private directory, never loose in a shared $TMPDIR: $XDG_RUNTIME_DIR when
# set, else a per-uid directory made 0700. Refuse unless it is a real directory of ours with mode 0700.
setup_dir() {
  if [ -n "${XDG_RUNTIME_DIR:-}" ]; then DIR=$XDG_RUNTIME_DIR/omuzaki-read-token
  else DIR=${TMPDIR:-/tmp}/omuzaki-read-token-$(id -u); fi
  mkdir -m 700 "$DIR" 2>/dev/null
  private "$DIR" -d 700 || { echo "gh-read-token: $DIR is not a private directory of ours (a real directory, owned by you, mode 0700); refusing" >&2; return 1; }
  CACHE=$DIR/cache
}

token() {
  local now exp tok
  now=$(date +%s)
  # Key, cache and their temp files only ever touch the verified directory; clean up on every way out.
  trap cleanup EXIT; trap 'exit 130' INT; trap 'exit 143' TERM
  setup_dir || return 1
  if private "$CACHE" -f 600 && read -r exp tok < "$CACHE" && [ -n "${tok:-}" ] && [ "${exp:-0}" -gt $((now + 600)) ]; then
    printf '%s' "$tok"; return 0
  fi
  [ -n "${OMUZAKI_READ_APP_ID:-}" ] && [ -n "${OMUZAKI_READ_APP_KEY_B64:-}" ] \
    || { echo "gh-read-token: OMUZAKI_READ_APP_ID and OMUZAKI_READ_APP_KEY_B64 must be set" >&2; return 1; }
  local key b64 header payload sig jwt inst resp
  # The key exists on disk only for the one signing call; EXIT, INT and TERM remove it (and any half-written cache).
  key=$(umask 077; mktemp "$DIR/key.XXXXXX") || return 1
  KEYFILE=$key
  if ! printf '%s' "$OMUZAKI_READ_APP_KEY_B64" | base64 -d > "$key" 2>/dev/null; then
    rm -f "$key"; KEYFILE=; echo "gh-read-token: OMUZAKI_READ_APP_KEY_B64 is not valid base64" >&2; return 1
  fi
  header=$(printf '{"alg":"RS256","typ":"JWT"}' | b64)
  payload=$(printf '{"iat":%d,"exp":%d,"iss":"%s"}' $((now - 60)) $((now + 540)) "$OMUZAKI_READ_APP_ID" | b64)
  sig=$(printf '%s.%s' "$header" "$payload" | openssl dgst -sha256 -sign "$key" -binary | b64); rm -f "$key"; KEYFILE=
  [ -n "$sig" ] || { echo "gh-read-token: signing failed (is the key a PEM private key?)" >&2; return 1; }
  jwt="$header.$payload.$sig"
  inst=$(api_auth "$jwt" "https://api.github.com/repos/$REPO/installation" \
         | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["id"])') \
    || { echo "gh-read-token: GitHub refused the App (wrong ID or key, or not installed on $REPO)" >&2; return 1; }
  resp=$(api_auth "$jwt" -X POST -H "Content-Type: application/json" -d "$BODY" "https://api.github.com/app/installations/$inst/access_tokens") \
    || { echo "gh-read-token: GitHub refused to issue a token" >&2; return 1; }
  read -r tok exp < <(printf '%s' "$resp" | python3 -I -c '
import json, sys, datetime
d = json.load(sys.stdin)
# Refuse a token that is not read-only on exactly this repository, whatever was asked for.
bad = [k for k, v in d.get("permissions", {}).items() if v != "read"]
repos = [r.get("full_name") for r in d.get("repositories", [])]
if bad or repos != ["dazsel/omuzaki"]:
    sys.exit("gh-read-token: the token is not read-only on only dazsel/omuzaki; refusing it")
print(d["token"], int(datetime.datetime.fromisoformat(d["expires_at"].replace("Z", "+00:00")).timestamp()))') \
    || { echo "gh-read-token: unexpected token response" >&2; return 1; }
  # A fresh private file renamed over the cache: writing through `>` would follow a planted file or symlink and keep
  # its permissions. A failed cache write only costs a new token next time.
  if CTMP=$(umask 077; mktemp "$DIR/cache.XXXXXX"); then
    { printf '%s %s\n' "$exp" "$tok" > "$CTMP" && mv -f "$CTMP" "$CACHE"; } || rm -f "$CTMP"
    CTMP=
  fi
  printf '%s' "$tok"
}

if [ "${1:-}" = --check ]; then
  tok=$(token) || exit 1
  # Only an App token can list its installation's repositories; a token swapped for the user's on the way is refused.
  if api_auth "$tok" "https://api.github.com/installation/repositories" >/dev/null; then
    left=$(api_auth "$tok" https://api.github.com/rate_limit \
           | python3 -I -c 'import json,sys; c=json.load(sys.stdin)["resources"]["core"]; print("%s/%s" % (c["remaining"], c["limit"]))') \
      && [ -n "$left" ] || { echo "gh-read-token: the rate-limit lookup failed; can't report the App budget" >&2; exit 1; }
    echo "ok: the token reaches GitHub as the App; App budget left: $left"
  else
    echo "not ok: GitHub didn't accept the token as the App (a proxy may be replacing the Authorization header)" >&2
    exit 1
  fi
  exit 0
fi
token
