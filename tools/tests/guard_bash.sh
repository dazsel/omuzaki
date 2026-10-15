#!/usr/bin/env bash
# Tests for tools/hooks/guard_bash.py: secret paths and Director-only commands are blocked (exit 2) in any argument
# order, ordinary commands pass (exit 0), and input the hook can't read blocks (fail closed).
set -uo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"; hook="$root/tools/hooks/guard_bash.py"; fails=0
run() {  # run <want exit> <stdin> <label>
  python3 -I "$hook" <<<"$2" 2>/dev/null; local got=$?
  if [ "$got" = "$1" ]; then echo "PASS ($got) $3"; else echo "FAIL (got $got, want $1) $3"; fails=$((fails + 1)); fi
}
check() {  # check <want exit> <command>
  run "$1" "$(python3 -I -c 'import json, sys; print(json.dumps({"tool_input": {"command": sys.argv[1]}}))' "$2")" "$2"
}
# Secret paths, including the bare directories.
check 2 'cat .env'
check 2 'cat .env.local'
check 2 'python3 -c "open(\".env\").read()"'
check 2 'cat ~/.ssh/id_ed25519'
check 2 'ls $HOME/.ssh'
check 2 'cat ~/.config/gh/hosts.yml'
check 2 'find ~/.config -type f -exec cat {} +'
check 2 'tar -cf /tmp/c.tar ~/.config'
check 2 'tar -cf /tmp/c.tar ~/.cache'
check 2 'cat ~/.cache/huggingface/token'
check 2 'cat ~/.netrc'
check 2 'cat ~/.git-credentials'
# Director-only gh commands, in every argument position.
check 2 'gh pr merge 12'
check 2 'gh pr -R dazsel/omuzaki merge 12'
check 2 'gh -R dazsel/omuzaki pr merge 12'
check 2 'gh pr --repo=dazsel/omuzaki merge 12 --squash'
check 2 'GH_TOKEN=x gh pr merge 12 --auto'
check 2 '/usr/bin/gh pr merge 12'
check 2 'cd crates && gh pr merge 12'
check 2 'echo ok; gh release create v1'
check 2 'gh repo edit --enable-auto-merge'
check 2 'gh secret set X'
check 2 'gh variable list'
check 2 'gh workflow disable ci.yml'
check 2 'gh api -X DELETE repos/o/r/git/refs/heads/x'
check 2 'gh api repos/o/r/git/refs/heads/x --method=delete'
check 2 'gh api -XDELETE repos/o/r/issues/comments/1'
check 2 'gh api -X PUT repos/o/r/pulls/12/merge'
check 2 'gh api repos/o/r/merges -f base=main -f head=x'
check 2 "gh api graphql -f query='mutation { enqueuePullRequest(input: {pullRequestId: \"x\"}) { clientMutationId } }'"
check 2 'sh -c "gh pr merge 12"'
check 2 'bash -c "cd x && gh pr -R o/r merge 12"'
# Forcing or deleting pushes.
check 2 'git push --force origin x'
check 2 'git push origin x --force'
check 2 'git push -f origin x'
check 2 'git push -fu origin x'
check 2 'git push origin +x'
check 2 'git push origin +HEAD:refs/heads/x'
check 2 'git push origin :x'
check 2 'git push --delete origin x'
check 2 'git push origin -d x'
check 2 'git -C ~/work/omuzaki push --force origin x'
check 2 'git push --mirror origin'
# Ordinary commands pass.
check 0 'cargo xtask check --affected'
check 0 'git push origin cloud/1234-thing'
check 0 'git push -u origin HEAD:refs/heads/cloud/1234-thing'
check 0 'git push --force-with-lease=cloud/x:abc123 origin cloud/x'
check 0 'git -c core.hooksPath=/dev/null commit -m "merge main"'
check 0 'git merge origin/main'
check 0 'gh pr view 12 --json mergeable'
check 0 'gh pr list -R dazsel/omuzaki --state merged'
check 0 'gh pr checks 12 -R dazsel/omuzaki'
check 0 'gh api repos/o/r/pulls/12/merged'
check 0 'gh repo view'
check 0 'gh run view 123 --log-failed'
check 0 'grep -rn environment crates/'
check 0 'cat docs/maintainers/cloud-lane.md'
check 0 'echo "$ENV_VAR"'
check 0 'cat web/vite.config.ts'
check 0 'ls ~/.cache/pip'
check 0 'cat .envrc'
check 0 "cat > notes.md <<'X'
it's fine
X"
# Input the hook can't read blocks.
run 2 'not json' 'invalid JSON blocks'
run 2 '{"tool_input": {"command": 5}}' 'a non-string command blocks'
# The installed handler blocks when the hook can't start.
if python3 -I -c 'import json, sys
h = json.load(open(sys.argv[1]))["hooks"]["PreToolUse"][0]["hooks"][0]
sys.exit(0 if h.get("onFailure") == "block" and "guard_bash.py" in h["command"] else 1)' "$root/.claude/settings.json"; then
  echo "PASS settings: the Bash hook is guard_bash.py with onFailure block"
else echo "FAIL settings: the Bash hook must be guard_bash.py with onFailure block"; fails=$((fails + 1)); fi
[ "$fails" = 0 ] && echo "guard-bash: all passed" || { echo "guard-bash: $fails failed"; exit 1; }
