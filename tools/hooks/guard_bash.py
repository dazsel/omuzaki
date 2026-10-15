#!/usr/bin/env python3
"""PreToolUse hook for Bash (.claude/settings.json): refuse commands that read secrets or do what only the Director does.

The settings allow `Bash` wholesale, and their `Bash(...)` denies match a command only as written, so `gh pr -R o/r
merge 1` would pass `Bash(gh pr merge *)`. This hook reads each command the way the tools parse it instead, in any
argument order, and blocks:
- secrets: secret paths (`.env`, `~/.ssh`, `~/.config`, `~/.cache` and the Hugging Face cache, `.netrc`, git
  credentials, private keys, `/proc/*/environ`), including the bare directories (`tar ~/.config` reads every file in
  it); the commands that print a credential (`gh auth token`, `gh auth status --show-token`, `git credential`); and
  printing the environment (`env`, `printenv`, `export -p`, `set` on their own) or a secret variable (`$GH_TOKEN`,
  `printenv NAME` and `declare -p NAME` for a name with TOKEN, SECRET, PASSWORD, API_KEY, PRIVATE_KEY or CREDENTIAL in
  it);
- `gh pr merge`, `gh release`, repository settings, secrets and variables, and turning workflows on or off;
- every `gh api` call that writes: any method but GET, fields or `--input` without a method (gh then sends a POST), and
  GraphQL mutations, and a GraphQL body read from a file (`--input`), which the hook can't inspect. A session writes
  through `gh` subcommands (`gh pr comment`, `gh pr edit`), whose reserved ones are listed above, so an endpoint list
  can't fall behind;
- a `git push` that forces (`--force`, `-f`, a `+` refspec, `--mirror`) or deletes (`--delete`, `-d`, a `:` refspec,
  `--prune`), and a `--force-with-lease` push to any branch but the one checked out, or to many refs at once
  (`--all`, `--branches`, `--tags`, or no refspec where `push.default` is `matching` or a `remote.<name>.push` refspec
  adds refs).

Backslash-newline continuations are joined first, as the shell joins them. It guards against a session's mistakes,
not a determined bypass: a variable holding `merge`, a script file or `eval`
gets past any reading of the command line. The real boundaries are that a session VM holds no project secrets and
GitHub's own permissions and rulesets.

Input: the hook JSON on stdin ({"cwd": ..., "tool_input": {"command": ...}}). Exit 2 blocks the call and shows stderr
to Claude. Anything this hook cannot read also exits 2 (fail closed), and settings.json sets `onFailure: "block"` so a
hook that cannot start blocks too.
"""

import json
import os
import re
import shlex
import subprocess
import sys

# A secret path named as a path word (after a space, `/`, a quote, `=` and the like). `~/.config` and `~/.cache` count
# only under a home directory, so the repository's own `.config/nextest.toml` stays readable; gh's credentials
# (`.config/gh`) count anywhere. `vite.config.ts`, `.envrc` and other caches (`~/.cache/pip`) stay allowed.
_START = r"(?:^|(?<=[\s/'\"\\=:(<>]))"
_END = r"(?=[\s/'\"\\)<>;|&]|$)"
_HOME = r"(?:~|\$HOME|\$\{HOME\}|/home/[^/\s]+|/root|/Users/[^/\s]+)/"
SECRET_PATH = re.compile(
    _START + r"(?:\.env(?:\.[\w.-]+)?|\.ssh|\.netrc|\.git-credentials)" + _END
    + r"|" + _HOME + r"\.config" + _END
    + r"|" + _HOME + r"\.cache(?=[\s'\"\\)<>;|&]|$)"
    + r"|\.config/gh(?=[/\s'\"\\)<>;|&]|$)"
    + r"|\.cache/huggingface"
    + r"|id_(?:rsa|ed25519|ecdsa)"
    + r"|/proc/[^/\s]+/environ"
)
# A secret variable read by name (`$GH_TOKEN`, `${HF_TOKEN}`, `printenv GITHUB_TOKEN`), and the commands that print the
# whole environment when run on their own.
SECRET_NAME = r"[A-Za-z_]*(?:TOKEN|SECRET|PASSWORD|PASSWD|API_KEY|APIKEY|PRIVATE_KEY|CREDENTIAL)[A-Za-z0-9_]*"
SECRET_VARIABLE = re.compile(r"\$\{?" + SECRET_NAME + r"\b", re.IGNORECASE)
SECRET_NAME_WORD = re.compile(r"^" + SECRET_NAME + r"$", re.IGNORECASE)
ENV_DUMPERS = {"env", "printenv", "set", "export", "declare", "typeset"}

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
# gh api options that send a request body, which makes gh default to POST: the long forms, and the short field flags
# `-f`/`-F`, which also count attached (`-Fbody=hi`) or inside a short-flag group (`-pFbody=hi`).
GH_API_BODY_LONG = ("--field", "--raw-field", "--input")
GH_API_FIELD_FLAG = re.compile(r"-[A-Za-z]*?[fF](.*)", re.DOTALL)
GRAPHQL_MUTATION = re.compile(r"\bmutation\b", re.IGNORECASE)
# git options that take a value before the subcommand (`git -C dir push`), and git push options that take one.
GIT_OPTIONS_WITH_VALUE = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path"}
PUSH_OPTIONS_WITH_VALUE = {"-o", "--push-option", "--receive-pack", "--exec"}
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


def body_flag(arg):
    """Whether a gh api word sends a body: a field flag (`-f`, `-F`, attached or grouped), or `--field`, `--raw-field`
    or `--input`, bare or with `=`."""
    if arg.startswith("--"):
        return arg.split("=", 1)[0] in GH_API_BODY_LONG
    return GH_API_FIELD_FLAG.match(arg) is not None


def field_text(arg):
    """A field word without its flag (`-Fname=v`, `-pFname=v`, `--field=name=v` give `name=v`), else the word itself.
    gh drops one `=` between the flag and the field, so `-F=name=v` is `name=v` too."""
    if arg.startswith("--"):
        text = arg.split("=", 1)[1] if arg.startswith(("--field=", "--raw-field=")) else arg
    else:
        match = GH_API_FIELD_FLAG.match(arg)
        text = match.group(1) if match else arg
    return text[1:] if text.startswith("=") else text


def reads_file(arg):
    """Whether a gh api word is a field read from a file (`name=@file`, or `@file` with no name), bare or with its flag
    (`-Fname=@file`, `-F=name=@file`)."""
    return re.match(r"^[\w.\[\]-]+=@", arg) or re.match(r"^(?:[\w.\[\]-]+=)?@", field_text(arg))


def gh_api_method(args):
    """The HTTP method of a `gh api` call, upper-cased: `-X M`, `-XM`, `--method M` or `--method=M`, else gh's default
    (POST when the call sends fields or `--input`, GET otherwise)."""
    for i, arg in enumerate(args):
        if arg in ("-X", "--method") and i + 1 < len(args):
            return args[i + 1].upper()
        if arg.startswith("--method="):
            return arg.split("=", 1)[1].upper()
        if arg.startswith("-X") and len(arg) > 2:
            return arg[2:].upper()
    return "POST" if any(body_flag(a) for a in args) else "GET"


def gh_api_refusal(args, endpoint):
    """Why a `gh api` call writes, or None. GraphQL reads go by POST, so GraphQL is judged by its operation instead."""
    if endpoint == "graphql":
        # A body or field read from a file or stdin (`--input`, `-F query=@file`) can't be inspected.
        if any(a == "--input" or a.startswith("--input=") or reads_file(a) for a in args):
            return "a GraphQL body or field from a file (`--input`, `-F name=@file`), which the hook can't inspect,"
        return "a GraphQL mutation through `gh api`" if any(GRAPHQL_MUTATION.search(a) for a in args) else None
    method = gh_api_method(args)
    return None if method in ("GET", "HEAD") else f"a `gh api` {method} call"


def gh_refusal(args):
    """Why a gh call is reserved, or None."""
    args = without_repo_flags(args)
    words = [a for a in args if not a.startswith("-")]
    if not words:
        return None
    group, sub = words[0], (words[1] if len(words) > 1 else "")
    shows_token = any(re.match(r"^(?:-t|--show-token)(?:=(?!false$).*)?$", a, re.IGNORECASE) for a in args)
    if group == "auth" and (sub == "token" or (sub == "status" and shows_token)):
        return f"`gh auth {sub}` prints a credential and"
    if group in GH_RESERVED and (GH_RESERVED[group] is None or sub in GH_RESERVED[group]):
        return "`" + " ".join(w for w in ("gh", group, sub) if w) + "`"
    if group == "api":
        return gh_api_refusal(args, sub)
    return None


def git_output(directory, *args):
    """stdout of `git <args>` run in `directory`, or None when git fails or can't run."""
    try:
        out = subprocess.run(["git", "-C", directory, *args], capture_output=True, text=True, timeout=5, check=False)
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout if out.returncode == 0 else None


def current_branch(directory):
    """The branch checked out in `directory`, or None when it can't be read (no repository, a detached HEAD)."""
    out = git_output(directory, "symbolic-ref", "--quiet", "--short", "HEAD")
    return (out.strip() or None) if out else None


def config_key(key):
    """A config key in git's canonical spelling: section and variable names are case-insensitive, a subsection
    (`remote.<name>.push`) is not."""
    head, _, rest = key.partition(".")
    subsection, dot, variable = rest.rpartition(".")
    if not dot:
        return f"{head.lower()}.{rest.lower()}"
    return f"{head.lower()}.{subsection}.{variable.lower()}"


def git_config(directory, key, overrides=()):
    """The values of a git config key in git's precedence order, so the last one wins for a single-valued key: the
    repository's and the user's values, then the `git -c` overrides, which come after them."""
    wanted = config_key(key)
    values = (git_output(directory, "config", "--get-all", key) or "").splitlines()
    return values + [value for name, _, value in overrides if config_key(name) == wanted]


def push_words(push_args):
    """A push's positional words (its remote, then its refspecs) and the `--repo` value, or None without one."""
    positional, repo, skip = [], None, False
    for i, arg in enumerate(push_args):
        if skip:
            skip = False
        elif arg == "--repo":
            repo = push_args[i + 1] if i + 1 < len(push_args) else None
            skip = True
        elif arg.startswith("--repo="):
            repo = arg.split("=", 1)[1]
        elif arg in PUSH_OPTIONS_WITH_VALUE:
            skip = True
        elif not arg.startswith("-"):
            positional.append(arg)
    return positional, repo


def default_remote(directory, branch, overrides):
    """The remote a push goes to when it names none, in git's order: the branch's pushRemote, remote.pushDefault, the
    branch's remote, else the only remote when there is exactly one (git pushes there even when it isn't origin), else
    origin."""
    keys = ["remote.pushDefault"]
    if branch:
        keys = [f"branch.{branch}.pushRemote", *keys, f"branch.{branch}.remote"]
    for key in keys:
        values = git_config(directory, key, overrides)
        if values:
            return values[-1]
    remotes = (git_output(directory, "remote") or "").split()
    return remotes[0] if len(remotes) == 1 else "origin"


def implicit_push_refusal(remote, directory, branch, overrides):
    """Why a `--force-with-lease` push with no refspec reaches other refs, or None. Under the default `push.default`
    (`simple`) git pushes only the checked-out branch; `matching` pushes every branch of that name, and a
    `remote.<name>.push` refspec adds refs of its own."""
    defaults = git_config(directory, "push.default", overrides)
    if defaults and defaults[-1] == "matching":
        return ("a `--force-with-lease` push with no refspec under `push.default=matching`, which reaches other"
                " branches,")
    name = remote or default_remote(directory, branch, overrides)
    if git_config(directory, f"remote.{name}.push", overrides):
        return f"a `--force-with-lease` push with no refspec, which `remote.{name}.push` widens,"
    return None


def lease_refusal(push_args, directory, overrides):
    """Why a `--force-with-lease` push reaches another ref, or None. Each refspec's target must be the checked-out
    branch; with no refspec, see implicit_push_refusal."""
    positional, repo = push_words(push_args)
    # `--repo <remote>` names the remote, so every positional argument is then a refspec.
    remote = repo if repo is not None else next(iter(positional), None)
    refspecs = positional if repo is not None else positional[1:]
    branch = current_branch(directory)
    if not refspecs:
        return implicit_push_refusal(remote, directory, branch, overrides)
    for spec in refspecs:
        target = spec.split(":", 1)[1] if ":" in spec else spec
        target = target.removeprefix("refs/heads/")
        if target != "HEAD" and target != branch:
            return f"a `--force-with-lease` push to `{target}`, which isn't the checked-out branch,"
    return None


def git_refusal(args, cwd):
    """Why a git call is reserved, or None: it prints a credential, or it is a forcing or deleting push."""
    i, directory, overrides = 0, cwd, []
    while i < len(args) and args[i].startswith("-"):
        if args[i] == "-C" and i + 1 < len(args):
            directory = os.path.join(directory, os.path.expanduser(args[i + 1]))
        # `git -c name=value` overrides the configuration that the lease check reads.
        if args[i] == "-c" and i + 1 < len(args):
            overrides.append(args[i + 1].partition("="))
        i += 2 if args[i] in GIT_OPTIONS_WITH_VALUE else 1
    sub = args[i] if i < len(args) else ""
    if sub == "credential" or sub.startswith("credential-"):
        return f"`git {sub}` prints a credential and"
    if sub != "push":
        return None
    push_args = args[i + 1:]
    for arg in push_args:
        if arg in ("--force", "--delete", "--mirror", "--prune") or arg.startswith(("+", ":")):
            return f"`git push {arg}`"
        # Short options combine (`-fu`); `f` forces and `d` deletes.
        if re.fullmatch(r"-[A-Za-z]+", arg) and set(arg[1:]) & {"f", "d"}:
            return f"`git push {arg}`"
        # A refspec `src:dst` with a `+` forces only that ref.
        if ":" in arg and not arg.startswith("-") and arg.split(":", 1)[0].startswith("+"):
            return f"`git push {arg}`"
    if any(a.startswith("--force-with-lease") for a in push_args):
        many = next((a for a in push_args if a in ("--all", "--branches", "--tags")), None)
        if many:
            return f"a `--force-with-lease` push with `{many}`, which reaches other branches,"
        return lease_refusal(push_args, directory, overrides)
    return None


def env_refusal(words):
    """Why a simple command prints the environment or a secret variable, or None."""
    if not words:
        return None
    program, args = os.path.basename(words[0]), words[1:]
    if program == "printenv" and any(SECRET_NAME_WORD.match(a) for a in args):
        return "printing a secret variable"
    # `declare -p NAME` prints the value, so a secret name is refused with or without the flag, as `printenv` does.
    if program in ("declare", "typeset") and any(SECRET_NAME_WORD.match(a.split("=", 1)[0]) for a in args):
        return f"printing a secret variable with `{program}`"
    # `set -euo pipefail` sets options; only a bare `set` lists the variables. The others print with options alone
    # (`env -0`, `export -p`, `declare -x`) and do something else once given a name or a command.
    if (program == "set" and not args) or (program in ENV_DUMPERS - {"set"} and all(a.startswith("-") for a in args)):
        return f"printing the whole environment (`{program}` on its own)"
    return None


def refusal(command, cwd, depth=0):
    """Why this command is blocked, or None."""
    command = command.replace("\\\n", "")  # the shell joins a backslash-newline continuation before parsing
    if SECRET_PATH.search(command):
        return "it names a secret path (.env, ~/.ssh, ~/.config, ~/.cache, .netrc, git credentials or a private key)"
    if SECRET_VARIABLE.search(command):
        return "it reads a secret variable (a name with TOKEN, SECRET, PASSWORD, API_KEY or CREDENTIAL in it)"
    for segment in SEPARATORS.split(command):
        words = tokens(segment)
        found = env_refusal(words)
        if found:
            return f"{found} could print a credential and"
        for i, word in enumerate(words):
            found = None
            if is_program(word, "gh"):
                found = gh_refusal(words[i + 1:])
            elif is_program(word, "git"):
                found = git_refusal(words[i + 1:], cwd)
            if found:
                return f"{found} is reserved for the Director"
        # A quoted word holding a command (`sh -c "..."`, `ssh host "..."`) is read the same way.
        if depth < MAX_DEPTH:
            for word in words:
                if " " in word:
                    found = refusal(word, cwd, depth + 1)
                    if found:
                        return found
    return None


def main():
    try:
        call = json.load(sys.stdin)
        command = call.get("tool_input", {}).get("command", "")
        cwd = call.get("cwd") or os.getcwd()
        if not isinstance(command, str) or not isinstance(cwd, str):
            raise TypeError("tool_input.command or cwd is not a string")
    except (ValueError, TypeError, AttributeError) as exc:
        print(f"Blocked by tools/hooks/guard_bash.py: the hook could not read the call ({exc}).", file=sys.stderr)
        return 2
    why = refusal(command, cwd)
    if why:
        print(
            f"Blocked by tools/hooks/guard_bash.py: {why}. Write through gh subcommands (gh pr comment, gh pr edit) and"
            " ask your lead for anything reserved. See docs/maintainers/cloud-lane.md.",
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
