#!/usr/bin/env python3
"""The attested self-gate: a pull request's tests run on the session's own machine, and CI checks the claim.

    self-gate.py heads HEAD MAIN_REF [BEFORE]     print `owned=` and `skip=` lines (append them to $GITHUB_OUTPUT)
    self-gate.py check --repo R --pr N --heads "SHA SHA" [--allowed LOGINS] [--comments FILE] [--hosted FILE]

Why. A pull request no longer runs the test matrix; the merge queue does, on the exact commit that will land. In its
place the session that wrote the change runs `cargo xtask check --affected` on its own VM and says so in the ready
comment and in every fix-round comment, on a line of its own:

    Self-gate: pass at <sha7> (cargo xtask check --affected, <n> tests)

`check` fails unless the LATEST such line from a trusted commenter names one of the pull request's owned heads. Trusted
means author association OWNER, MEMBER or COLLABORATOR, or a login in LOGINS (the `PR_ALLOWED_AUTHORS` variable, comma
separated); the same rule AGENTS.md sets for rulings. Anyone else's line is ignored. It reads comments only: it needs
`issues: read` and never runs pull request code with more than that.

Owned heads. The head commit, plus each earlier head reached by undoing a merge that only brought in `main`: a merge
commit whose second parent is on `main` and whose result takes every file it changed from that parent, so the pull
request's own content is untouched. Such a merge keeps the attestation valid, and a push of just that merge needs no CI
(`skip=true`): the merge queue tests against `main` anyway. A merge that resolves a conflict, or changes a file the
pull request also changed, whichever parent's version it keeps, is not skipped and needs a new line.

Hosted run. When no author can build, a maintainer dispatches ci.yml on the PR branch with `hosted_tests=true` and then
re-runs this job. `check` then also accepts a successful hosted run, found through the API, that is ALL of: event
`workflow_dispatch`; workflow path `.github/workflows/ci.yml`; `head_sha` equal to the pull request head; conclusion
success with every `test (...)` job success (skipped or cancelled does not count); and the pull request changes nothing
under `.github/`, `tools/ci/`, `xtask/` or `.config/`, because a dispatched run executes the branch's own workflow, CI
scripts and planner, so a PR that edits them could vouch for itself. Such a PR needs a human `Self-gate:` line.
Needs `actions: read` besides `pull-requests: read`.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.request

TRUSTED_ASSOCIATIONS = {"OWNER", "MEMBER", "COLLABORATOR"}
# The line starts the line: a quoted copy ("> Self-gate: ...") or prose mentioning it does not attest.
ATTESTATION = re.compile(r"^Self-gate: pass at ([0-9a-f]{7,40})\b", re.MULTILINE)
MIN_SHA = 7


def git(*args):
    """Stdout of a git command; a non-zero exit raises."""
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def is_ancestor(commit, ref):
    """Whether `commit` is reachable from `ref`."""
    return subprocess.run(["git", "merge-base", "--is-ancestor", commit, ref], capture_output=True).returncode == 0


def changed_paths(left, right):
    """The paths whose content differs between two commits (no rename detection, so both sides of a move appear)."""
    return set(git("diff", "--name-only", "--no-renames", "-z", left, right).split("\0")) - {""}


def main_only_parent(commit, main_ref):
    """The first parent of `commit` when `commit` is a merge that only brought in `main`, else None."""
    parents = git("rev-list", "--parents", "-n", "1", commit).split()[1:]
    if len(parents) != 2:
        return None
    first, second = parents
    if not is_ancestor(second, main_ref):
        return None
    base = git("merge-base", first, second).strip()
    from_pr, from_main = changed_paths(base, first), changed_paths(base, second)
    taken_from_first, taken_from_second = changed_paths(first, commit), changed_paths(commit, second)
    # Nothing of the pull request's own may be touched: the merge changes only files `main` changed, and takes each of
    # them whole from `main`. Checked from both sides, because a resolution that picks one parent equals it there and
    # so shows no difference against it:
    #  - a file the merge changed that is neither parent's version (a combined or hand-edited result);
    #  - a file the PR changed that the merge replaced (a conflict resolved to main's version, dropping PR work);
    #  - a file `main` changed that the merge did not take (a conflict resolved to the PR's version, dropping main's).
    if taken_from_first & taken_from_second or taken_from_first & from_pr or taken_from_second & from_main:
        return None
    return first


def owned_heads(head, main_ref):
    """`head`, then each earlier head behind a merge of only `main`, newest first."""
    heads = [head]
    while (parent := main_only_parent(heads[-1], main_ref)) is not None:
        heads.append(parent)
    return heads


def attested_sha(comments, allowed):
    """The sha named by the latest trusted attestation line, or None."""
    latest = None
    for comment in sorted(comments, key=lambda c: c["id"]):
        trusted = comment.get("author_association") in TRUSTED_ASSOCIATIONS or comment["user"]["login"] in allowed
        if not trusted:
            continue
        for sha in ATTESTATION.findall(comment.get("body") or ""):
            latest = sha
    return latest


CI_WORKFLOW = ".github/workflows/ci.yml"
# What the dispatched run executes from the PR branch to decide its own verdict: the workflow files and the actions they
# use, the scripts that plan, aggregate and attest, xtask (which computes the affected set) and the nextest profile. A PR
# touching any of them could make its own hosted run green, so it needs a human `Self-gate:` line.
PROTECTED = (".github/", "tools/ci/", "xtask/", ".config/")


def workflow_file(run):
    """A run's workflow path without the `@ref` suffix GitHub may append."""
    return (run.get("path") or "").split("@")[0]


def is_test_job(job):
    """Whether `job` is a test shard that must have succeeded.

    When the plan has no tests the whole matrix is skipped before it expands, and GitHub records one skipped job named
    with the raw `${{ matrix.name }}` expression. That job is no shard (an empty plan is a legitimate pass); a skipped
    real shard, named `test (3)`, still counts and fails the proof.
    """
    return job["name"].startswith("test (") and "${{" not in job["name"]


def hosted_proof(runs, head, files):
    """The id of a hosted run that proves `head` (rules in the module doc), or None.

    `runs` are workflow runs, each with its `jobs`; `files` are the (current and previous) paths the pull request changes.
    """
    if any(path.startswith(PROTECTED) for path in files):
        return None
    for run in sorted(runs, key=lambda r: r["id"], reverse=True):
        tests = [job for job in run.get("jobs", []) if is_test_job(job)]
        if (run.get("event") == "workflow_dispatch" and workflow_file(run) == CI_WORKFLOW
                and run.get("head_sha") == head and run.get("conclusion") == "success"
                and all(job["conclusion"] == "success" for job in tests)):
            return run["id"]
    return None


def verdict(comments, heads, allowed, runs=(), files=()):
    """(ok, message) for a pull request whose owned heads are `heads`."""
    sha = attested_sha(comments, allowed)
    if sha is not None and any(head.startswith(sha) for head in heads):
        return True, f"self-gate: pass attested at {sha}"
    hosted = hosted_proof(runs, heads[0], files)
    if hosted is not None:
        return True, f"self-gate: hosted run {hosted} on {heads[0][:MIN_SHA]}"
    seen = f"the latest trusted line names {sha}" if sha else "no trusted comment has one"
    short = heads[0][:MIN_SHA]
    return False, (
        f"self-gate: no trusted `Self-gate: pass at <sha7>` line names the head {short} ({seen}). Run "
        f"`cargo xtask check --affected` on this head, post `Self-gate: pass at {short} (cargo xtask check --affected, "
        f"<n> tests)` on its own line in a comment, then re-run this job. Or, if the PR changes nothing under "
        f".github/, tools/ci/, xtask/ or .config/, dispatch ci.yml with hosted_tests=true on the branch and re-run this job."
    )


def fetch_comments(repo, number):
    """Every issue comment of a pull request, oldest first, through the REST API with the job's own token."""
    api = os.environ.get("GITHUB_API_URL", "https://api.github.com")
    url = f"{api}/repos/{repo}/issues/{number}/comments?per_page=100"
    comments = []
    while url:
        request = urllib.request.Request(url, headers={
            "Authorization": f"Bearer {os.environ['GITHUB_TOKEN']}",
            "Accept": "application/vnd.github+json",
        })
        with urllib.request.urlopen(request, timeout=30) as response:
            comments += json.load(response)
            url = None
            for part in response.headers.get("Link", "").split(","):
                if 'rel="next"' in part:
                    url = part[part.index("<") + 1:part.index(">")]
    return comments


def api_get(url):
    """JSON from one REST call with the job's own token."""
    request = urllib.request.Request(url, headers={
        "Authorization": f"Bearer {os.environ['GITHUB_TOKEN']}",
        "Accept": "application/vnd.github+json",
    })
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def fetch_hosted(repo, number, head):
    """(dispatch runs of ci.yml for `head`, each with its jobs; the paths the pull request changes)."""
    api = os.environ.get("GITHUB_API_URL", "https://api.github.com")
    base = f"{api}/repos/{repo}"
    runs = api_get(f"{base}/actions/runs?event=workflow_dispatch&head_sha={head}&per_page=100")["workflow_runs"]
    runs = [dict(run, jobs=api_get(f"{base}/actions/runs/{run['id']}/jobs?per_page=100&filter=latest")["jobs"])
            for run in runs if workflow_file(run) == CI_WORKFLOW and run.get("conclusion") == "success"]
    files, page = set(), 1
    while True:
        batch = api_get(f"{base}/pulls/{number}/files?per_page=100&page={page}")
        for entry in batch:
            files |= {entry["filename"], entry.get("previous_filename", "")}
        if len(batch) < 100:
            return runs, files - {""}
        page += 1


def command_heads(args):
    heads = owned_heads(args.head, args.main_ref)
    # A push of only a main merge: the pushed-over head is one of the earlier owned heads.
    skip = bool(args.before) and args.before in heads[1:]
    print(f"owned={' '.join(heads)}")
    print(f"skip={str(skip).lower()}")
    return 0


def command_check(args):
    if args.comments:
        with open(args.comments, encoding="utf-8") as handle:
            comments = json.load(handle)
    else:
        comments = fetch_comments(args.repo, args.pr)
    allowed = {login for login in args.allowed.split(",") if login}
    heads = args.heads.split()
    runs, files = (), ()
    if args.hosted:
        with open(args.hosted, encoding="utf-8") as handle:
            hosted = json.load(handle)
        runs, files = hosted["runs"], hosted["files"]
    elif not args.comments:
        # Only needed when no attestation names the head; the cheap comment check goes first.
        if not verdict(comments, heads, allowed)[0]:
            runs, files = fetch_hosted(args.repo, args.pr, heads[0])
    ok, message = verdict(comments, heads, allowed, runs, files)
    print(message, file=sys.stdout if ok else sys.stderr)
    return 0 if ok else 1


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    heads = sub.add_parser("heads")
    heads.add_argument("head")
    heads.add_argument("main_ref")
    heads.add_argument("before", nargs="?", default="")
    heads.set_defaults(run=command_heads)
    check = sub.add_parser("check")
    check.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""))
    check.add_argument("--pr", type=int, default=0)
    check.add_argument("--heads", required=True)
    check.add_argument("--allowed", default="")
    check.add_argument("--comments")
    check.add_argument("--hosted", help="JSON {runs: [...with jobs], files: [...]} instead of the API (tests)")
    check.set_defaults(run=command_check)
    args = parser.parse_args(argv[1:])
    return args.run(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
