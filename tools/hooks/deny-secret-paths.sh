#!/usr/bin/env bash
# PreToolUse hook for Bash (.claude/settings.json): refuse a shell command that names a secret path.
#
# The settings' `Read(...)` denies stop Claude's file tools only; a shell command such as `cat ~/.ssh/id_ed25519` or a
# script opening `.env` would still read the file. This closes that path for commands that name one. It is a guard
# against accidents, not a sandbox: a session VM holds no project secrets, and an obfuscated path gets past any pattern.
# Input: the hook JSON on stdin ({"tool_input": {"command": ...}}). Exit 2 blocks the call and shows stderr to Claude.
set -uo pipefail
command=$(python3 -c 'import json, sys; print(json.load(sys.stdin).get("tool_input", {}).get("command", ""))' 2>/dev/null) || exit 0
pattern='(^|[^[:alnum:]_])\.env([^[:alnum:]_]|$)|\.ssh(/|[^[:alnum:]_]|$)|\.config/|\.cache/huggingface|\.netrc|\.git-credentials|id_(rsa|ed25519|ecdsa)'
if grep -qE "$pattern" <<<"$command"; then
  echo "Blocked by tools/hooks/deny-secret-paths.sh: the command names a secret path (.env, ~/.ssh, ~/.config, ~/.cache/huggingface, .netrc, git credentials or a private key). Sessions never read secrets." >&2
  exit 2
fi
exit 0
