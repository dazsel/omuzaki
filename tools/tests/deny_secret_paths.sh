#!/usr/bin/env bash
# Tests for tools/hooks/deny-secret-paths.sh: secret paths are blocked (exit 2), ordinary commands pass (exit 0).
set -uo pipefail
hook="$(dirname "$0")/../hooks/deny-secret-paths.sh"; fails=0
check() {  # check <want exit> <command>
  local json; json=$(python3 -c 'import json, sys; print(json.dumps({"tool_input": {"command": sys.argv[1]}}))' "$2")
  "$hook" <<<"$json" 2>/dev/null; local got=$?
  if [ "$got" = "$1" ]; then echo "PASS ($got) $2"; else echo "FAIL (got $got, want $1) $2"; fails=$((fails + 1)); fi
}
check 2 'cat .env'
check 2 'python3 -c "open(\".env\").read()"'
check 2 'cat ~/.ssh/id_ed25519'
check 2 'ls $HOME/.ssh'
check 2 'cat ~/.config/gh/hosts.yml'
check 2 'cat ~/.cache/huggingface/token'
check 2 'cat ~/.netrc'
check 2 'cat .env.local'
check 0 'cargo xtask check --affected'
check 0 'git push origin cloud/1234-thing'
check 0 'grep -rn environment crates/'
check 0 'cat docs/maintainers/cloud-lane.md'
check 0 'echo "$ENV_VAR"'
[ "$fails" = 0 ] && echo "deny-secret-paths: all passed" || { echo "deny-secret-paths: $fails failed"; exit 1; }
