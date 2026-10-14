#!/usr/bin/env python3
"""Decide whether a task may start now: its fence must not collide with open pull requests.

    dispatch-check.py ISSUE [ISSUE ...]    `go #N` or `wait #N: reasons` per task; exit 0 only if all are go
    dispatch-check.py --report             open pull requests by crate and by lane, oldest first
    options: --crate-cap N (default 3), --total-cap N (default 40), --json (for the checks)

Why a script: conflicts are decided when a task starts, not when it merges (ADR 0053 sections 1.2 and 2.1). The
Director or the cloud lead runs it once before starting work. Standard library plus `gh`; it only reads GitHub.

The fence is read from the issue body: the `Fence:` line (or `**Fence:**` section) up to the next blank line. Its
backtick-quoted paths and globs count; `{a,b}` braces expand. A task with no fence waits. Checks, for each task:

- overlap: an open, non-draft PR changes a file inside the fence. Allowed when that PR closes this issue (it is the
  task's own PR), or when this issue is blocked by it (`Blocked by:` names the PR or the issue it closes).
- crate cap: the crates the fence names already have `--crate-cap` or more open PRs touching them.
- total cap: `--total-cap` or more open PRs in total.
- contract first: the fence names an `*-api` crate or `omuzaki-core` and another open PR touches that crate.

Open PRs are listed in one call with a limit of PR_LIMIT. Reaching the limit means the list may be cut, so the script
stops rather than guess.
"""

import argparse
import fnmatch
import json
import re
import subprocess
import sys
from collections import defaultdict

REPO = "dazsel/omuzaki"
PR_LIMIT = 1000
LANES = ("cloud", "local", "port")
# GitHub's closing keywords, as they appear in a pull request body.
CLOSES = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s*:?\s+#(\d+)", re.IGNORECASE)
# `Fence:` or `**Fence:**` (also `May touch:`) at the start of a line, with anything after the colon on the same line.
FENCE_HEADER = re.compile(r"^\s*\*{0,2}(?:fence|may touch)\*{0,2}\s*:\*{0,2}(.*)$", re.IGNORECASE)
BLOCKED_BY = re.compile(r"blocked[- ]by", re.IGNORECASE)
ISSUE_REF = re.compile(r"#(\d+)")
CRATE = re.compile(r"crates/([A-Za-z0-9_-]+)(?:/|$)")


class GhError(Exception):
    """A `gh` call failed or printed something that is not JSON."""


def gh_json(args):
    """Run `gh ARGS` and return its parsed JSON output."""
    try:
        proc = subprocess.run(["gh", *args], capture_output=True, text=True, timeout=120, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise GhError(f"gh {args[0]} {args[1]}: {exc}") from exc
    if proc.returncode != 0:
        raise GhError(f"gh {args[0]} {args[1]} failed: {proc.stderr.strip()}")
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise GhError(f"gh {args[0]} {args[1]} printed no JSON: {exc}") from exc


def load_issue(number):
    """The issue's number, title and body."""
    return gh_json(["issue", "view", str(number), "-R", REPO, "--json", "number,title,body"])


def load_prs():
    """Every open pull request, with its draft flag, branch, creation time, changed paths and the issues it closes."""
    raw = gh_json(
        ["pr", "list", "-R", REPO, "--state", "open", "--limit", str(PR_LIMIT),
         "--json", "number,headRefName,isDraft,createdAt,files,body"]
    )
    if len(raw) >= PR_LIMIT:
        raise GhError(f"{len(raw)} open pull requests reached the list limit of {PR_LIMIT}; the list may be cut")
    return [
        {
            "number": pr["number"],
            "branch": pr["headRefName"],
            "draft": bool(pr["isDraft"]),
            "created": pr.get("createdAt") or "",
            "files": [f["path"] for f in pr.get("files") or []],
            "closes": {int(n) for n in CLOSES.findall(pr.get("body") or "")},
        }
        for pr in raw
    ]


def expand_braces(pattern):
    """Expand `{a,b}` groups in a glob, one group at a time: `src/{x,y}.rs` gives two patterns."""
    match = re.search(r"\{([^{}]*)\}", pattern)
    if not match:
        return [pattern]
    expanded = []
    for alternative in match.group(1).split(","):
        expanded.extend(expand_braces(pattern[: match.start()] + alternative + pattern[match.end():]))
    return expanded


def fence_entries(body):
    """The fence's paths and globs, from the `Fence:` section of an issue body. Empty when there is no fence."""
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
    text = "\n".join(section)
    # Prefer backtick-quoted paths. Without any, take each bullet's text up to its first ':', ';' or '('.
    spans = re.findall(r"`([^`]+)`", text)
    if not spans:
        spans = [re.split(r"[:;(]", re.sub(r"^\s*[-*]\s*", "", s), maxsplit=1)[0] for s in section]
    entries = []
    for span in spans:
        for pattern in expand_braces(span.strip()):
            pattern = pattern.strip()
            # A path has a slash or a dot; a bare word such as "none" is prose, not a fence.
            if ("/" in pattern or "." in pattern) and pattern not in entries:
                entries.append(pattern)
    return [e.removeprefix("./").rstrip("/") for e in entries]


def covers(entry, path):
    """True when the path is the entry, matches its glob, or lies inside it as a folder."""
    if fnmatch.fnmatchcase(path, entry):
        return True
    folder = entry.rstrip("*").rstrip("/")
    return bool(folder) and path.startswith(folder + "/")


def crate_of(path):
    """The crate a path or fence entry lies in (`crates/<name>/...`), or None. Crate names have no dots, so a
    file such as `crates/README.md` is not a crate."""
    match = CRATE.match(path)
    return match.group(1) if match else None


def crates_touched(pr):
    """The set of crates whose folders a pull request changes."""
    return {c for c in (crate_of(path) for path in pr["files"]) if c}


def blocked_by(body):
    """The issue and PR numbers named on the `Blocked by:` line of an issue body."""
    numbers = set()
    for line in body.splitlines():
        if BLOCKED_BY.search(line):
            numbers.update(int(n) for n in ISSUE_REF.findall(line))
    return numbers


def refs(prs):
    """`#1, #2` for a list of pull requests, in number order."""
    return ", ".join(f"#{p['number']}" for p in sorted(prs, key=lambda p: p["number"]))


def check_issue(issue, prs, crate_cap, total_cap):
    """The reasons a task must wait; an empty list means go."""
    number = issue["number"]
    body = issue.get("body") or ""
    fence = fence_entries(body)
    if not fence:
        return ["no fence"]
    blocked = blocked_by(body)
    # Drafts don't count, and the task's own pull request (the one that closes it) is not a conflict with itself.
    others = [p for p in prs if not p["draft"] and number not in p["closes"]]
    crates = sorted({c for c in map(crate_of, fence) if c})
    reasons = []
    for pr in others:
        hit = next((path for path in pr["files"] if any(covers(e, path) for e in fence)), None)
        if hit is None:
            continue
        if pr["number"] in blocked or pr["closes"] & blocked:
            continue  # chained: this task is blocked by that work, so it is meant to build on it
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
        results = []
        for number in numbers:
            issue = load_issue(number)
            reasons = check_issue(issue, prs, args.crate_cap, args.total_cap)
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
