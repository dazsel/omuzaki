#!/usr/bin/env python3
"""Decide whether a task may start now: its fence must not collide with open pull requests.

    dispatch-check.py ISSUE [ISSUE ...]    `go #N` or `wait #N: reasons` per task; exit 0 only if all are go
    dispatch-check.py --report             open pull requests by crate and by lane, oldest first
    options: --crate-cap N (default 3), --total-cap N (default 40), --json (for the checks)

Why a script: conflicts are decided when a task starts, not when it merges (ADR 0053 sections 1.2 and 2.1). The
Director or the cloud lead runs it once before starting work. Standard library plus `gh`; it only reads GitHub.

The fence is read from the issue body: the `Fence:` line (or `**Fence:**` section) up to the next blank line. Each
entry is a path or glob: backtick-quoted or bare, comma- or space-separated, one per bullet or several on a line.
`{a,b}` groups expand, and commas inside them do not split. A task with no fence waits. Checks, for each task:

- overlap: an open, non-draft PR changes a file inside the fence. Allowed when that PR closes this issue (it is the
  task's own PR), or when the work is chained: the PR closes an issue that blocks this one, or this issue blocks the
  issue the PR closes. The native dependency links (`.../dependencies/blocked_by` and `.../dependencies/blocking`)
  count; so does a `Blocked by:` or `Depends on:` line in the body, as a fallback.
- crate cap: the crates the fence names already have `--crate-cap` or more open PRs touching them. A wildcard crate
  (`crates/*-api/...`) names every matching folder under `crates/`.
- total cap: `--total-cap` or more open PRs in total.
- contract first: the fence names an `*-api` crate or `omuzaki-core` and another open PR touches that crate.

Fail closed: a fence entry that cannot be understood makes the task wait. `gh pr list` returns at most 100 files per PR, so a PR with more reads its full list from the REST API. That list
holds at most 3,000 files, so a PR with 3,000 or more counts as unreadable. If that list cannot be read, every task waits. If the `crates/` folder cannot be read, a task with a wildcard crate waits.

Open PRs are listed in one call with a limit of PR_LIMIT. Reaching the limit means the list may be cut, so the script
stops rather than guess.
"""

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
from collections import defaultdict

REPO = "dazsel/omuzaki"
PR_LIMIT = 1000
# The REST files endpoint returns at most this many files; a list of this size or more may be cut short.
REST_FILE_CAP = 3000
LANES = ("cloud", "local", "port")
# A brace group may expand to at most this many alternatives; a larger one makes its entry unreadable (fail closed).
BRACE_LIMIT = 256
# `Fence:` or `**Fence:**` (also `May touch:`) at the start of a line, with anything after the colon on the same line.
FENCE_HEADER = re.compile(r"^\s*\*{0,2}(?:fence|may touch)\*{0,2}\s*:\*{0,2}(.*)$", re.IGNORECASE)
# A fallback dependency line: `Blocked by: #N` or `Depends on: #N`, at the start of a line (bullets and bold allowed).
BLOCKED_BY = re.compile(r"^\s*(?:[-*]\s+)?\*{0,2}(?:blocked[- ]by|depends on)\*{0,2}\s*:", re.IGNORECASE)
ISSUE_REF = re.compile(r"#(\d+)")
# The crate a path lies in: `crates/<name>/...`. Crate names have no dots, so `crates/README.md` is not a crate.
CRATE = re.compile(r"crates/([A-Za-z0-9_-]+)(?:/|$)")
# The crate segment of a fence entry, which may be a wildcard such as `*-api`.
CRATE_SEGMENT = re.compile(r"crates/([^/]+)(?:/|$)")
WILDCARD_CRATE = re.compile(r"crates/[^/]*[*?\[]")
CRATE_NAME = re.compile(r"[A-Za-z0-9_-]+")
TRUNCATED_FILES = "cannot read the changed files of #{n}; waiting until they can be read"
UNREADABLE_ENTRY = "cannot understand the fence entry {entry}; waiting until it is fixed"
UNRESOLVED_WILDCARD = "cannot resolve {entry}: the crates/ folder could not be read"


class GhError(Exception):
    """A `gh` call failed or printed something that is not JSON."""


def run_gh(args):
    """Run `gh ARGS` and return its standard output."""
    try:
        proc = subprocess.run(["gh", *args], capture_output=True, text=True, timeout=120, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise GhError(f"gh {args[0]} {args[1]}: {exc}") from exc
    if proc.returncode != 0:
        raise GhError(f"gh {args[0]} {args[1]} failed: {proc.stderr.strip()}")
    return proc.stdout


def gh_json(args):
    """Run `gh ARGS` and return its parsed JSON output."""
    try:
        return json.loads(run_gh(args))
    except json.JSONDecodeError as exc:
        raise GhError(f"gh {args[0]} {args[1]} printed no JSON: {exc}") from exc


def gh_json_pages(args):
    """Run `gh ARGS` for a paginated list endpoint and return every item.

    `gh api --paginate` prints one JSON array per page, back to back rather than as one array, so each is decoded in turn.
    """
    items = []
    rest = run_gh(args).lstrip()
    decoder = json.JSONDecoder()
    try:
        while rest:
            value, end = decoder.raw_decode(rest)
            items.extend(value if isinstance(value, list) else [value])
            rest = rest[end:].lstrip()
    except json.JSONDecodeError as exc:
        raise GhError(f"gh {args[0]} {args[1]} printed no JSON: {exc}") from exc
    return items


def rest_only():
    """True when DISPATCH_CHECK_REST=1 skips GraphQL, so the REST path can be tested where GraphQL works."""
    return os.environ.get("DISPATCH_CHECK_REST") == "1"


def note_fallback(exc):
    """Say on stderr that a GraphQL read was refused and REST is used; the verdict on stdout does not change."""
    print(f"dispatch-check: GraphQL refused, reading REST instead: {exc}", file=sys.stderr)


def load_issue(number):
    """The issue's number, title and body, with a null body read as "".

    GraphQL first; cloud sessions sit behind a proxy that refuses GraphQL, so a refused call falls back to REST.
    """
    if not rest_only():
        try:
            return gh_json(["issue", "view", str(number), "-R", REPO, "--json", "number,title,body"])
        except GhError as exc:
            note_fallback(exc)
    data = gh_json(["api", f"repos/{REPO}/issues/{number}"])
    return {"number": data["number"], "title": data["title"], "body": data.get("body") or ""}


def load_dependencies(number):
    """The native blocked-by and blocking links of an issue, as two sets of issue numbers.

    `blocked_by` are the issues that block this one; `blocking` are the issues this one blocks.
    """
    links = {}
    for kind in ("blocked_by", "blocking"):
        items = gh_json_pages(["api", "--paginate", f"repos/{REPO}/issues/{number}/dependencies/{kind}"])
        links[kind] = {int(item["number"]) for item in items}
    return links["blocked_by"], links["blocking"]


def load_crates():
    """The names of the folders under crates/ on the default branch."""
    entries = gh_json(["api", f"repos/{REPO}/contents/crates"])
    return [entry["name"] for entry in entries if entry.get("type") == "dir"]


def pr_files(number, listed, changed):
    """The changed paths of a pull request, its renamed files' old paths included, or None when they cannot be read.

    `gh pr list` lists at most 100 files; `changed` is the true count. Up to that count the listed paths are used; more
    are read from the REST list. The REST list is fetched once and gives the renamed files' old paths too. None means
    the list is incomplete, and the caller must wait rather than assume no overlap.
    """
    files = rest_files(number)
    if files is None:
        return None
    listed_paths = [f["path"] for f in listed]
    total = len(listed_paths) if changed is None else changed
    if total <= len(listed_paths):
        paths = listed_paths
    elif len(files) >= total:
        paths = [f["filename"] for f in files]
    else:
        return None
    return with_renamed_sources(paths, files)


def rest_pr_files(number):
    """The changed paths of a pull request read in full from REST, renamed files included, or None when unreadable.

    REST has no 100-file cap, so this needs no `gh pr list` file list at all.
    """
    files = rest_files(number)
    return None if files is None else with_renamed_sources([f["filename"] for f in files], files)


def rest_files(number):
    """The REST file list of a pull request, one entry per changed file, or None when it cannot be read in full.

    The files endpoint returns at most REST_FILE_CAP files, so a list that long may be cut short and counts as unreadable.
    The paths and the renames both come from this one fetch, so each pull request is read once.
    """
    try:
        files = gh_json_pages(["api", "--paginate", f"repos/{REPO}/pulls/{number}/files"])
    except GhError:
        return None
    return None if len(files) >= REST_FILE_CAP else files


def with_renamed_sources(paths, files):
    """`paths` plus the old paths of the renamed files in `files`, a REST file list; each path is listed once."""
    sources = [f["previous_filename"] for f in files if f.get("status") == "renamed" and f.get("previous_filename")]
    return paths + [source for source in sources if source not in paths]


def load_default_branch():
    """The name of the repository's default branch. Raises GhError when it cannot be read."""
    branch = gh_json(["api", f"repos/{REPO}"]).get("default_branch")
    if not branch:
        raise GhError(f"gh api repos/{REPO} gave no default_branch")
    return branch


def load_top_level():
    """The names of the entries at the root of the default branch, files and folders alike."""
    return {entry["name"] for entry in gh_json(["api", f"repos/{REPO}/contents"])}


def closing_numbers(refs):
    """The numbers of the issues in REPO among GitHub's closing references (`number` and `repository`)."""
    return {
        int(ref["number"]) for ref in refs or []
        if str((ref.get("repository") or {}).get("nameWithOwner", "")).lower() == REPO.lower()
    }


# GitHub's closing keywords as REST has them in a PR body (GraphQL gives closingIssuesReferences instead). The keyword
# must start a word, must not follow "not " (a deliberately simple rule: "Does not close #42" is prose), and must be
# followed by `#N` directly, so `Fixes owner/repo#N` names another repository and is not counted.
CLOSING_REFERENCE = re.compile(
    r"(?<![\w-])(?<!not )(?:close[sd]?|fix(?:es|ed)?|resolve[sd]?)[ \t]*:?[ \t]*#(\d+)(?!\d)", re.IGNORECASE
)


def closes_in_body(body):
    """The issue numbers in this repository a PR body closes, by GitHub's keyword rule (see CLOSING_REFERENCE)."""
    return {int(number) for number in CLOSING_REFERENCE.findall(body or "")}


def check_list_limit(raw):
    """Stop when the open PR list reached PR_LIMIT: the list may be cut, and a cut list would hide overlaps."""
    if len(raw) >= PR_LIMIT:
        raise GhError(f"{len(raw)} open pull requests reached the list limit of {PR_LIMIT}; the list may be cut")


def load_prs():
    """Every open pull request, with its draft flag, branch, creation time, changed paths and the issues it closes.

    GraphQL first, in one `gh pr list` call. A refused call falls back to load_prs_rest, which reads the same list.
    """
    if rest_only():
        return load_prs_rest()
    try:
        raw = gh_json(
            ["pr", "list", "-R", REPO, "--state", "open", "--limit", str(PR_LIMIT),
             "--json", "number,headRefName,isDraft,createdAt,files,changedFiles,closingIssuesReferences"]
        )
    except GhError as exc:
        note_fallback(exc)
        return load_prs_rest()
    check_list_limit(raw)
    return [
        {
            "number": pr["number"],
            "branch": pr["headRefName"],
            "draft": bool(pr["isDraft"]),
            "created": pr.get("createdAt") or "",
            "files": pr_files(pr["number"], pr.get("files") or [], pr.get("changedFiles")),
            # The issues GitHub itself says this PR closes in this repository; prose such as "Does not close #42" is
            # not a reference, and `Fixes other/repo#42` names another repository's issue 42.
            "closes": closing_numbers(pr.get("closingIssuesReferences")),
        }
        for pr in raw
    ]


def load_prs_rest():
    """The same list as load_prs, read from REST: `pulls` for the PRs and `pulls/N/files` for each one's paths.

    REST has no closing references, so `closes` is read from the PR body by closes_in_body. GitHub honours closing
    keywords only on pull requests into the default branch, so a PR into any other base closes nothing.
    """
    default = load_default_branch()
    raw = gh_json_pages(["api", "--paginate", f"repos/{REPO}/pulls?state=open&per_page=100"])
    check_list_limit(raw)
    return [
        {
            "number": pr["number"],
            "branch": pr["head"]["ref"],
            "draft": bool(pr.get("draft")),
            "created": pr.get("created_at") or "",
            "files": rest_pr_files(pr["number"]),
            "closes": closes_in_body(pr.get("body")) if (pr.get("base") or {}).get("ref") == default else set(),
        }
        for pr in raw
    ]


def expand_braces(pattern):
    """Expand `{a,b}` groups in a glob, one group at a time: `src/{x,y}.rs` gives two patterns.

    Raises ValueError when the expansion would exceed BRACE_LIMIT alternatives.
    """
    match = re.search(r"\{([^{}]*)\}", pattern)
    if not match:
        return [pattern]
    expanded = []
    for alternative in match.group(1).split(","):
        expanded.extend(expand_braces(pattern[: match.start()] + alternative + pattern[match.end():]))
        # Each call checks its own output, so a long chain of groups stops at the first level over the limit.
        if len(expanded) > BRACE_LIMIT:
            raise ValueError(f"more than {BRACE_LIMIT} alternatives in {pattern!r}")
    return expanded


def split_entries(text, spaces=True):
    """Split on commas, and on whitespace too when `spaces`, outside braces: `crates/{a,b}/src` stays one entry."""
    parts, current, depth = [], "", 0
    for char in text:
        if char == "{":
            depth += 1
        elif char == "}":
            depth = max(0, depth - 1)
        if depth == 0 and (char == "," or (spaces and char.isspace())):
            if current:
                parts.append(current)
            current = ""
        else:
            current += char
    if current:
        parts.append(current)
    return parts


def fence_section(body):
    """The lines of the `Fence:` section of an issue body, the header's own text first. Empty when there is none."""
    lines = body.splitlines()
    for start, line in enumerate(lines):
        header = FENCE_HEADER.match(line)
        if header:
            break
    else:
        return []
    section = [header.group(1)]
    for line in lines[start + 1:]:
        # The section ends at a blank line or at the next bold header or markdown heading.
        if not line.strip() or re.match(r"^\s*(?:#|\*\*\w)", line):
            break
        section.append(line)
    return section


def fence_entries(body, top_level=None):
    """The fence's paths and globs, and the entries that cannot be understood, as `(entries, bad)`.

    Each line contributes its backtick-quoted entries and its bare ones. A bare entry is cut at the first ':', ';' or
    '(' on its line, so a trailing note is not read as a path. A token is a path when it has a '/', a '.' or a glob
    character, judged before any trailing slash is removed, so `tools/` stays. A slashless token (`tools`,
    `LICENSE-MIT`) is a path when it is backtick-quoted, or names an entry in `top_level`, the set of root names; other tokens are prose
    ("none") and are dropped. When `top_level` is None (the root could not be read) a slashless token cannot be told
    from prose, so it is reported in `bad`. `bad` lists entries that fence_matches rejects; the caller must wait on
    them (fail closed).
    """
    entries, bad = [], []
    for line in fence_section(body):
        line = re.sub(r"^\s*[-*]\s+", "", line)
        quoted = re.findall(r"`([^`]+)`", line)
        bare = re.split(r"[:;(]", re.sub(r"`[^`]*`", " ", line), maxsplit=1)[0]
        # A backtick span is one path even with spaces in it (`docs/My File.md`); commas still separate entries.
        explicit = {t.strip().strip("\"'`") for span in quoted for t in split_entries(span, spaces=False)}
        for token in [t for span in quoted for t in split_entries(span, spaces=False)] + split_entries(bare):
            token = token.strip().strip("\"'`")
            if not token:
                continue
            # A backtick-quoted slashless token is an explicit path even when `main` does not have it yet (an open PR
            # may add it); only unquoted plain words are judged against the root.
            if token in explicit:
                pass
            elif not re.search(r"[/.*?\[{}]", token) and token not in (top_level or ()):
                if top_level is None and token not in bad:
                    bad.append(token)
                continue
            try:
                patterns = expand_braces_checked(token)
                for pattern in patterns:
                    fence_matches(pattern, "x")  # validates the pattern
            except ValueError:
                if token not in bad:
                    bad.append(token)
                continue
            for pattern in patterns:
                pattern = pattern.removeprefix("./").rstrip("/")
                if pattern not in entries:
                    entries.append(pattern)
    return entries, bad


def expand_braces_checked(pattern):
    """expand_braces, but a stray '{' or '}' left over (unbalanced group) raises ValueError."""
    expanded = expand_braces(pattern)
    if any("{" in p or "}" in p for p in expanded):
        raise ValueError(f"unbalanced braces in {pattern!r}")
    return expanded


def glob_regex(pattern):
    """The regular expression for one brace-free glob, matched against a whole path. Raises ValueError if unreadable.

    `**` crosses '/' (and `**/` may match no folder at all), `*` and `?` stay within one segment, `[...]` is one
    character of a segment.
    """
    out, i = [], 0
    while i < len(pattern):
        char = pattern[i]
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif char == "*":
            out.append("[^/]*")
            i += 1
        elif char == "?":
            out.append("[^/]")
            i += 1
        elif char == "[":
            end = pattern.find("]", i + 2)
            if end < 0:
                raise ValueError(f"unclosed [ in {pattern!r}")
            body = pattern[i + 1:end]
            negate = body[:1] in ("!", "^")
            body = body[1:] if negate else body
            if "/" in body or not body:
                raise ValueError(f"unreadable [...] in {pattern!r}")
            out.append("[" + ("^/" if negate else "") + re.sub(r"([\\\[\]])", r"\\\1", body) + "]")
            i = end + 1
        else:
            out.append(re.escape(char))
            i += 1
    try:
        return re.compile("".join(out))
    except re.error as exc:
        raise ValueError(f"invalid pattern {pattern!r}: {exc}") from exc


def fence_matches(entry, path):
    """True when the fence entry covers the changed path. Raises ValueError for an entry it cannot understand.

    The rules, applied to each alternative after brace groups are expanded first (`a/{x,y}.rs` is `a/x.rs` or
    `a/y.rs`):

    - a literal file (`tools/x.py`) covers exactly that path;
    - a literal folder (`tools` or `tools/`, trailing slash optional) covers every path inside it;
    - `*` matches any characters within one path segment (never '/'), `?` one such character, `[abc]` one of them;
    - `**` matches across segments, and `a/**/b` also matches `a/b`;
    - every pattern, glob or literal, covers a path when it matches the path itself or ANY ancestor folder of the
      path, so `crates/omuzaki-*/src/` covers `crates/omuzaki-ops/src/a.rs`.

    A leading './' is ignored. Absolute paths, '..' segments, an empty entry, unbalanced braces or brackets raise
    ValueError, and the caller treats that as a reason to wait.
    """
    alternatives = expand_braces_checked(entry.strip())
    path = path.strip("/")
    # The path itself, then each ancestor folder: a/b/c.rs -> a/b/c.rs, a/b, a.
    parts = path.split("/")
    candidates = ["/".join(parts[:n]) for n in range(len(parts), 0, -1)]
    matched = False
    for alternative in alternatives:
        alternative = alternative.strip().removeprefix("./").rstrip("/")
        if not alternative or alternative.startswith("/") or ".." in alternative.split("/") or "//" in alternative:
            raise ValueError(f"unreadable fence entry {entry!r}")
        regex = glob_regex(alternative)
        if any(regex.fullmatch(c) for c in candidates):
            matched = True
    return matched


def crate_of(path):
    """The crate a changed path lies in, or None."""
    match = CRATE.match(path)
    return match.group(1) if match else None


def crates_named(entry, crate_names):
    """The crates a fence entry names: its literal crate, or every crate folder its wildcard matches.

    `crate_names` is None when the crates/ folder could not be read; a wildcard then names nothing here, and the caller
    reports it.
    """
    match = CRATE_SEGMENT.match(entry)
    if not match:
        return []
    segment = match.group(1)
    if re.search(r"[*?\[]", segment):
        return sorted(name for name in (crate_names or []) if fnmatch.fnmatchcase(name, segment))
    return [segment] if CRATE_NAME.fullmatch(segment) else []


def crates_touched(pr):
    """The set of crates whose folders a pull request changes. Empty when its file list is unknown."""
    return {c for c in (crate_of(path) for path in pr["files"] or []) if c}


def blocked_by(body):
    """The issue and PR numbers named on a `Blocked by:` or `Depends on:` line of an issue body.

    Only the line's own label counts, so prose such as "This task is not blocked by #99" records nothing.
    """
    numbers = set()
    for line in body.splitlines():
        match = BLOCKED_BY.match(line)
        if match:
            numbers.update(int(n) for n in ISSUE_REF.findall(line[match.end():]))
    return numbers


def refs(prs):
    """`#1, #2` for a list of pull requests, in number order."""
    return ", ".join(f"#{p['number']}" for p in sorted(prs, key=lambda p: p["number"]))


def check_issue(issue, prs, crate_cap, total_cap, native_blocked=frozenset(), native_blocking=frozenset(),
                crate_names=None, top_level=None):
    """The reasons a task must wait; an empty list means go.

    `native_blocked` and `native_blocking` are the issue's dependency links from load_dependencies. The body's
    `Blocked by:` line is read too, as a fallback for issues without native links. `crate_names` are the folders under
    crates/, or None when they could not be read. `top_level` are the root names, or None when they could not be read.
    """
    number = issue["number"]
    body = issue.get("body") or ""
    fence, bad = fence_entries(body, top_level)
    if not fence and not bad:
        return ["no fence"]
    reasons = [UNREADABLE_ENTRY.format(entry=e) for e in bad]
    reasons += [UNRESOLVED_WILDCARD.format(entry=e) for e in fence if WILDCARD_CRATE.match(e) and crate_names is None]
    blocked = blocked_by(body) | set(native_blocked)
    # Drafts don't count, and the task's own pull request (the one that closes it) is not a conflict with itself.
    others = [p for p in prs if not p["draft"] and number not in p["closes"]]
    reasons += [TRUNCATED_FILES.format(n=p["number"]) for p in others if p["files"] is None]
    # Crates that open PRs add do not exist on the default branch yet, so a wildcard also resolves against them.
    known = sorted(set(crate_names or []) | {c for p in others for c in crates_touched(p)}) if crate_names is not None else None
    crates = sorted({c for entry in fence for c in crates_named(entry, known)})
    for pr in others:
        if pr["files"] is None:
            continue
        hit = next((path for path in pr["files"] if any(fence_matches(e, path) for e in fence)), None)
        if hit is None:
            continue
        # Chained: this task is blocked by the work (it closes a blocker), or this task blocks the work it closes.
        if pr["number"] in blocked or pr["closes"] & blocked or pr["closes"] & set(native_blocking):
            continue
        reasons.append(f"overlap with #{pr['number']} ({hit})")
    for crate in crates:
        touching = [p for p in others if crate in crates_touched(p)]
        if len(touching) >= crate_cap:
            reasons.append(f"crate cap: {crate} has {len(touching)} open PRs ({refs(touching)}) (cap {crate_cap})")
    if len(others) >= total_cap:
        reasons.append(f"total cap: {len(others)} open PRs (cap {total_cap})")
    for crate in crates:
        if crate.endswith("-api") or crate == "omuzaki-core":
            touching = [p for p in others if crate in crates_touched(p)]
            if touching:
                reasons.append(f"contract first: {crate} has open PR(s) {refs(touching)}; a contract lands alone")
    return reasons


def lane_of(branch):
    """The lane a branch belongs to, from its prefix: cloud/, local/ or port/; anything else is other."""
    return next((lane for lane in LANES if branch.startswith(lane + "/")), "other")


def report(prs):
    """Open pull requests by lane and by crate, then oldest first, as text lines for the status post."""
    drafts = sum(p["draft"] for p in prs)
    lines = [f"open pull requests: {len(prs)} ({len(prs) - drafts} ready, {drafts} draft)"]
    lanes = defaultdict(int)
    for pr in prs:
        lanes[lane_of(pr["branch"])] += 1
    lines.append("by lane: " + ", ".join(f"{lane} {lanes[lane]}" for lane in (*LANES, "other")))
    lines.append("by crate:")
    by_crate = defaultdict(list)
    for pr in prs:
        for crate in crates_touched(pr):
            by_crate[crate].append(pr)
    for crate in sorted(by_crate):
        lines.append(f"  {crate}: {len(by_crate[crate])} ({refs(by_crate[crate])})")
    no_crate = [pr for pr in prs if not crates_touched(pr)]
    if no_crate:
        lines.append(f"  (no crate): {len(no_crate)} ({refs(no_crate)})")
    lines.append("oldest first:")
    for pr in sorted(prs, key=lambda p: p["created"]):
        state = " draft" if pr["draft"] else ""
        if pr["files"] is None:
            crates = "file list unreadable"
        else:
            crates = ", ".join(sorted(crates_touched(pr))) or "-"
        lines.append(f"  #{pr['number']} {pr['branch']} {pr['created'][:10]}{state}  crates: {crates}")
    return "\n".join(lines)


def parse_issue_numbers(raw_numbers, parser):
    """Turn `1477` or `#1477` arguments into integers, or stop with a usage error."""
    numbers = []
    for raw in raw_numbers:
        try:
            numbers.append(int(raw.lstrip("#")))
        except ValueError:
            parser.error(f"not an issue number: {raw}")
    return numbers


def main(argv):
    """Entry point: `argv` is the command line after the script name. Returns the exit status."""
    parser = argparse.ArgumentParser(description="Go or wait for tasks, by their fences and the open pull requests.")
    parser.add_argument("issues", nargs="*", help="issue numbers, with or without #")
    parser.add_argument("--report", action="store_true", help="print open PRs by crate and lane, oldest first")
    parser.add_argument("--json", action="store_true", help="print the check results as JSON")
    parser.add_argument("--crate-cap", type=int, default=3, help="open PRs per crate before a task waits (default 3)")
    parser.add_argument("--total-cap", type=int, default=40, help="open PRs in total before a task waits (default 40)")
    args = parser.parse_args(argv)
    if not args.issues and not args.report:
        parser.error("give issue numbers, or --report")
    numbers = parse_issue_numbers(args.issues, parser)
    try:
        prs = load_prs()
        if args.report:
            print(report(prs))
        if not numbers:
            return 0
        try:
            crate_names = load_crates()
        except GhError:
            crate_names = None  # a wildcard fence then waits; see check_issue
        try:
            top_level = load_top_level()
        except GhError:
            top_level = None  # a slashless fence word then waits; see fence_entries
        results = []
        for number in numbers:
            issue = load_issue(number)
            blocked, blocking = load_dependencies(number)
            reasons = check_issue(issue, prs, args.crate_cap, args.total_cap, blocked, blocking, crate_names,
                                  top_level)
            results.append({"issue": issue["number"], "go": not reasons, "reasons": reasons})
    except GhError as exc:
        print(f"dispatch-check: {exc}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(results, indent=2))
    else:
        for result in results:
            if result["go"]:
                print(f"go #{result['issue']}")
            else:
                print(f"wait #{result['issue']}: {'; '.join(result['reasons'])}")
    return 0 if all(r["go"] for r in results) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
