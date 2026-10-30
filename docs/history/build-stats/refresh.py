#!/usr/bin/env python3
"""Refresh the git- and GitHub-derived build-stats files for the days since they were last complete.

Usage: python3 docs/history/build-stats/refresh.py   (needs `git` and an authenticated `gh`)

Rewrites only the rows for the current Pacific day (and new hours), so a run is cheap. Aggregates only.
The agent-hour files are not touched here: they need the cloud project's own session records.
"""
import csv, json, subprocess, sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

PT = ZoneInfo("America/Los_Angeles")
DIR = Path(__file__).resolve().parent
REPO = "dazsel/omuzaki"
now = datetime.now(PT)
SINCE_DAY = sys.argv[1] if len(sys.argv) > 1 else now.date().isoformat()


def sh(*a, **k):
    # Run from the repository root so path-relative commands (git grep -- .) see the whole tree.
    return subprocess.run(a, check=True, capture_output=True, text=True, cwd=DIR.parents[2], **k).stdout


def read(name):
    with open(DIR / name, newline="") as f:
        r = csv.reader(f)
        head = next(r)
        return head, [row for row in r]


def write(name, head, rows):
    with open(DIR / name, "w", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(head)
        w.writerows(rows)


def merge_days(name, key_cols, new_rows):
    """Replace rows with date >= SINCE_DAY by new_rows (sorted)."""
    head, rows = read(name)
    rows = [r for r in rows if r[0] < SINCE_DAY] + [[str(x) for x in r] for r in new_rows if r[0] >= SINCE_DAY]
    rows.sort(key=lambda r: tuple(r[:key_cols]))
    write(name, head, rows)


def pt_day(iso):
    return datetime.fromisoformat(iso.replace("Z", "+00:00")).astimezone(PT).date().isoformat()


def api(path):
    out, page = [], 1
    while True:
        sep = "&" if "?" in path else "?"
        data = json.loads(sh("gh", "api", f"{path}{sep}per_page=100&page={page}"))
        items = data if isinstance(data, list) else next(v for v in data.values() if isinstance(v, list))
        out += items
        if len(items) < 100:
            return out
        page += 1


# ---- commits_daily.csv ------------------------------------------------------------------------------------------
sh("git", "fetch", "-q", "origin", "main")
log = sh("git", "log", "origin/main", "--no-merges", f"--since={SINCE_DAY}T00:00:00-07:00", "--numstat",
         "--format=@@%cI|%(trailers:key=Agent-Lane,valueonly,separator=;)|%(trailers:key=Agent-Model,valueonly,separator=;)")
days = defaultdict(lambda: defaultdict(int))
cur = None
for line in log.splitlines():
    if line.startswith("@@"):
        ts, lane, model = (line[2:].split("|") + ["", ""])[:3]
        d = pt_day(ts)
        cur = days[d]
        cur["commits"] += 1
        lane, model = lane.strip().lower(), model.strip().lower()
        cur["lane_" + (lane if lane in ("local", "cloud") else "unreported")] += 1
        m = next((x for x in ("opus", "sonnet", "haiku", "codex") if x in model), "unreported")
        cur["model_" + m] += 1
    elif line.strip() and cur is not None:
        a, b, _ = line.split("\t", 2)
        if a != "-":
            cur["added"] += int(a)
            cur["deleted"] += int(b)
cols = ["commits", "added", "deleted", "lane_local", "lane_cloud", "lane_unreported", "model_opus", "model_sonnet",
        "model_haiku", "model_codex", "model_unreported"]
merge_days("commits_daily.csv", 1, [[d] + [v[c] for c in cols] for d, v in sorted(days.items())])

# ---- loc_hourly.csv ---------------------------------------------------------------------------------------------
# Same method as before: lines at the last commit of each hour. Categories are classified by path; tests are
# crates/**/tests/, benches/, tests.rs-like files and src paths containing "test".
head, rows = read("loc_hourly.csv")
last = datetime.fromisoformat(rows[-1][0])
hours = sh("git", "log", "origin/main", f"--since={last.isoformat()}", "--format=%H %cI").split("\n")
by_hour = {}
for l in hours:
    if l.strip():
        h, ts = l.split()
        by_hour.setdefault(datetime.fromisoformat(ts).astimezone(PT).replace(minute=0, second=0, microsecond=0), h)
# git log is newest first, so setdefault keeps the newest commit of each hour; the first seen is the last commit


def classify(p):
    if p.endswith(".rs"):
        if p.startswith(("crates/", "spikes/", "xtask/", "tools/", "fuzz/")):
            if p.startswith("xtask/") or p.startswith("tools/"):
                return "tooling"
            if "/tests/" in p or "/benches/" in p or p.rsplit("/", 1)[-1] in ("tests.rs", "test.rs") \
                    or p.rsplit("/", 1)[-1].endswith(("_test.rs", "_tests.rs")) or ("/src/" in p and "test" in p):
                return "rust_tests"
            if p.startswith("crates/"):
                return "rust_src"
        return "other"
    if p.startswith(("docs/",)) or p.endswith(".md"):
        return "docs"
    if p.startswith((".github/", "tools/", "dist/", ".changes/")):
        return "tooling"
    return "other"


new = []
for hr, h in sorted(by_hour.items()):
    if hr <= last:
        continue
    tot = defaultdict(int)
    for l in sh("git", "grep", "-I", "-c", "", h, "--", ".").splitlines():
        path, n = l.split(":", 2)[1:]
        tot[classify(path)] += int(n)
    s = sum(tot.values())
    new.append([hr.isoformat(), tot["rust_src"], tot["rust_tests"], tot["docs"], tot["tooling"], tot["other"], s])
write("loc_hourly.csv", head, rows + new)

# ---- prs_daily.csv, issues_daily.csv, ci_daily.csv (this repository) ----------------------------------------
since_utc = f"{SINCE_DAY}T00:00:00Z"
prs = api(f"repos/{REPO}/pulls?state=all&sort=created&direction=desc")
opened, merged = defaultdict(int), defaultdict(int)
for p in prs:
    opened[pt_day(p["created_at"])] += 1
    if p.get("merged_at"):
        merged[pt_day(p["merged_at"])] += 1
head, rows = read("prs_daily.csv")
rows = [r for r in rows if not (r[1] == "this" and r[0] >= SINCE_DAY)]
for d in sorted(set(opened) | set(merged)):
    if d >= SINCE_DAY:
        rows.append([d, "this", opened[d], merged[d]])
rows.sort(key=lambda r: (r[0], r[1]))
write("prs_daily.csv", head, rows)

issues = api(f"repos/{REPO}/issues?state=all&since={since_utc}")
io, ic = defaultdict(int), defaultdict(int)
for i in issues:
    if "pull_request" in i:
        continue
    io[pt_day(i["created_at"])] += 1
    if i.get("closed_at"):
        ic[pt_day(i["closed_at"])] += 1
merge_days("issues_daily.csv", 1, [[d, io[d], ic[d]] for d in sorted(set(io) | set(ic))])

runs = api(f"repos/{REPO}/actions/runs?created=%3E%3D{SINCE_DAY}")
ci = defaultdict(lambda: [0, 0, 0, 0, 0, 0.0])
for r in runs:
    c = ci[(pt_day(r["created_at"]), r["name"])]
    c[0] += 1
    k = {"success": 1, "failure": 2, "cancelled": 3}.get(r.get("conclusion"), 4)
    c[k] += 1
    a, b = (datetime.fromisoformat(r[x].replace("Z", "+00:00")) for x in ("run_started_at", "updated_at"))
    c[5] += max((b - a).total_seconds(), 0) / 60
head, rows = read("ci_daily.csv")
rows = [r for r in rows if not (r[1] == "this" and r[0] >= SINCE_DAY)]
rows += [[d, "this", w] + [str(x) if i < 5 else f"{x:.1f}" for i, x in enumerate(v)] for (d, w), v in ci.items()
         if d >= SINCE_DAY]
rows.sort(key=lambda r: (r[0], r[1], r[2]))
write("ci_daily.csv", head, rows)

# ---- summary.json totals from the CSVs ----------------------------------------------------------------------
s = json.loads((DIR / "summary.json").read_text())
col = lambda name, i, flt=lambda r: True: sum(float(r[i]) for r in read(name)[1] if flt(r))
s["commits_on_main"] = int(col("commits_daily.csv", 1))
for k, repo in (("this_repo", "this"), ("earlier_repo", "earlier")):
    s["prs"][k + "_opened"] = int(col("prs_daily.csv", 2, lambda r: r[1] == repo))
    s["prs"][k + "_merged"] = int(col("prs_daily.csv", 3, lambda r: r[1] == repo))
s["issues_this_repo"] = {"opened": int(col("issues_daily.csv", 1)), "closed": int(col("issues_daily.csv", 2))}
s["ci_runs"] = int(col("ci_daily.csv", 3))
loc = read("loc_hourly.csv")[1][-1]
s["loc_now"] = dict(zip(["Rust source", "Rust tests and benches", "Docs", "Tooling and CI", "Other", "total"],
                        map(int, loc[1:7])))
stamp = datetime.now(PT)
s["generated"] = stamp.strftime("%Y-%m-%d %-I:%M %p PT")
cov = s["coverage"]
cov["loc"]["complete_through"] = loc[0]
cov["commits"]["complete_through"] = s["generated"]
cov["prs"]["this_repo_complete_through"] = s["generated"]
cov["issues"]["complete_through"] = s["generated"]
cov["ci"]["this_repo_complete_through"] = s["generated"]
(DIR / "summary.json").write_text(json.dumps(s, indent=1) + "\n")
print(json.dumps(s, indent=1))

# ---- backlog_snapshots.csv: open issues and open PRs now (one row per run, so the history has the run's granularity)
open_issues = [i for i in api(f"repos/{REPO}/issues?state=open") if "pull_request" not in i]
open_prs = api(f"repos/{REPO}/pulls?state=open")
p = DIR / "backlog_snapshots.csv"
if not p.exists():
    p.write_text("snapshot_utc,open_issues,open_prs\n")
with open(p, "a") as f:
    f.write(f"{datetime.now(timezone.utc):%Y-%m-%dT%H:%MZ},{len(open_issues)},{len(open_prs)}\n")

import runpy
runpy.run_path(str(DIR / "charts.py"))
