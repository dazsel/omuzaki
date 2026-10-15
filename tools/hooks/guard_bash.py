#!/usr/bin/env python3
"""PreToolUse hook for Bash (.claude/settings.json): refuse commands that read secrets or do what only the Director does.

The settings allow `Bash` wholesale, and their `Bash(...)` denies match a command only as written, so `gh pr -R o/r
merge 1` would pass `Bash(gh pr merge *)`. This hook reads each command the way the tools parse it instead, in any
argument order, and blocks:
- secret paths: `.env`, `~/.ssh`, `~/.config`, `~/.cache` and the Hugging Face cache, `.netrc`, git credentials and
  private keys, including the directories themselves (`tar ~/.config` reads every file in it);
- `gh pr merge`, `gh release`, repository settings, secrets and variables, turning workflows on or off, `gh api`
  DELETE calls, and merging through `gh api` (the REST merge endpoint and the GraphQL merge and auto-merge mutations);
- a `git push` that forces (`--force`, `-f`, a `+` refspec, `--mirror`) or deletes (`--delete`, `-d`, a `:` refspec,
  `--prune`). `--force-with-lease` on your own branch stays allowed.

It guards against a session's mistakes, not a determined bypass: a variable holding `merge`, a script file or `eval`
gets past any reading of the command line. The real boundaries are that a session VM holds no project secrets and
GitHub's own permissions and rulesets.

Input: the hook JSON on stdin ({"tool_input": {"command": ...}}). Exit 2 blocks the call and shows stderr to Claude.
Anything this hook cannot read also exits 2 (fail closed), and settings.json sets `onFailure: "block"` so a hook that
cannot start blocks too.
"""

import json
import os
import re
import shlex
import sys

# A secret path named as a path word (after a space, `/`, a quote, `=` and the like), including the bare directories:
# `tar ~/.config` reads every file in it. `vite.config.ts`, `.envrc` and other caches (`~/.cache/pip`) stay allowed.
_START = r"(?:^|(?<=[\s/'\"\\=:(<>]))"
_END = r"(?=[\s/'\"\\)<>;|&]|$)"
SECRET_PATH = re.compile(
    _START + r"(?:\.env(?:\.[\w.-]+)?|\.ssh|\.config|\.netrc|\.git-credentials)" + _END
    + r"|" + _START + r"\.cache(?=[\s'\"\\)<>;|&]|$)"
    + r"|\.cache/huggingface"
    + r"|id_(?:rsa|ed25519|ecdsa)"
)

# Separators between simple commands. Splitting inside a quoted string only makes extra fragments, which can at worst
# block a command that quotes a forbidden one; it never hides one.
SEPARATORS = re.compile(r"\n|;|&&|\|\||\||&|\$\(|`|\(|\)")

# gh: the subcommands of each command group that only the Director runs (None: the whole group).
GH_RESERVED = {
    "pr": {"merge"},
    "release": None,
    "repo": {"delete", "edit", "rename", "archive", "unarchive"},
    "secret": None,
    "variable": None,
    "workflow": {"enable", "disable"},
}
# gh api calls that merge: the REST pull request and branch merge endpoints, and the GraphQL merge, queue and
# auto-merge mutations.
GH_API_MERGE = re.compile(
    r"\bpulls/[^/\s]+/merge\b|\brepos/[^/\s]+/[^/\s]+/merges\b"
    r"|mergePullRequest|enqueuePullRequest|enablePullRequestAutoMerge|mergeBranch",
    re.IGNORECASE,
)
# git options that take a value before the subcommand (`git -C dir push`).
GIT_OPTIONS_WITH_VALUE = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path"}
# Nesting depth for commands quoted inside commands (`sh -c "gh pr merge 1"`).
MAX_DEPTH = 3


def tokens(segment):
    """Shell words of one simple command; a segment shlex cannot parse (a heredoc line's apostrophe) splits on spaces."""
    try:
        return shlex.split(segment, comments=False)
    except ValueError:
        return segment.split()


def is_program(token, name):
    """Whether a word runs `name`, bare or by path (`/usr/bin/gh`)."""
    return os.path.basename(token) == name


def without_repo_flags(args):
    """gh's arguments without the inherited `-R`/`--repo` flag, which may sit anywhere."""
    kept, skip = [], False
    for arg in args:
        if skip:
            skip = False
        elif arg in ("-R", "--repo"):
            skip = True
        elif not (arg.startswith("--repo=") or (arg.startswith("-R") and len(arg) > 2)):
            kept.append(arg)
    return kept


def gh_api_method(args):
    """The HTTP method of a `gh api` call, upper-cased, from `-X M`, `-XM`, `--method M` or `--method=M`."""
    for i, arg in enumerate(args):
        if arg in ("-X", "--method") and i + 1 < len(args):
            return args[i + 1].upper()
        if arg.startswith("--method="):
            return arg.split("=", 1)[1].upper()
        if arg.startswith("-X") and len(arg) > 2:
            return arg[2:].upper()
    return ""


def gh_refusal(args):
    """Why a gh call is reserved, or None."""
    args = without_repo_flags(args)
    words = [a for a in args if not a.startswith("-")]
    if not words:
        return None
    group, sub = words[0], (words[1] if len(words) > 1 else "")
    if group in GH_RESERVED and (GH_RESERVED[group] is None or sub in GH_RESERVED[group]):
        return "`" + " ".join(w for w in ("gh", group, sub) if w) + "`"
    if group == "api":
        if gh_api_method(args) == "DELETE":
            return "a `gh api` DELETE call"
        if any(GH_API_MERGE.search(a) for a in args):
            return "merging through `gh api`"
    return None


def git_push_refusal(args):
    """Why a git call is a forcing or deleting push, or None."""
    i = 0
    while i < len(args) and args[i].startswith("-"):
        i += 2 if args[i] in GIT_OPTIONS_WITH_VALUE else 1
    if i >= len(args) or args[i] != "push":
        return None
    for arg in args[i + 1:]:
        if arg in ("--force", "--delete", "--mirror", "--prune") or arg.startswith(("+", ":")):
            return f"`git push {arg}`"
        # Short options combine (`-fu`); `f` forces and `d` deletes.
        if re.fullmatch(r"-[A-Za-z]+", arg) and set(arg[1:]) & {"f", "d"}:
            return f"`git push {arg}`"
        # A refspec `src:dst` with a `+` forces only that ref.
        if ":" in arg and not arg.startswith("-") and arg.split(":", 1)[0].startswith("+"):
            return f"`git push {arg}`"
    return None


def refusal(command, depth=0):
    """Why this command is blocked, or None."""
    if SECRET_PATH.search(command):
        return "it names a secret path (.env, ~/.ssh, ~/.config, ~/.cache, .netrc, git credentials or a private key)"
    for segment in SEPARATORS.split(command):
        words = tokens(segment)
        for i, word in enumerate(words):
            found = None
            if is_program(word, "gh"):
                found = gh_refusal(words[i + 1:])
            elif is_program(word, "git"):
                found = git_push_refusal(words[i + 1:])
            if found:
                return f"{found} is reserved for the Director"
        # A quoted word holding a command (`sh -c "..."`, `ssh host "..."`) is read the same way.
        if depth < MAX_DEPTH:
            for word in words:
                if " " in word:
                    found = refusal(word, depth + 1)
                    if found:
                        return found
    return None


def main():
    try:
        command = json.load(sys.stdin).get("tool_input", {}).get("command", "")
        if not isinstance(command, str):
            raise TypeError("tool_input.command is not a string")
    except (ValueError, TypeError, AttributeError) as exc:
        print(f"Blocked by tools/hooks/guard_bash.py: the hook could not read the call ({exc}).", file=sys.stderr)
        return 2
    why = refusal(command)
    if why:
        print(f"Blocked by tools/hooks/guard_bash.py: {why}. See docs/maintainers/cloud-lane.md.", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
