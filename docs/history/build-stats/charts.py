#!/usr/bin/env python3
"""Draw the build-stats charts as SVG files in charts/ from the CSVs next to this script (standard library only)."""
import csv
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

DIR = Path(__file__).resolve().parent
PT = ZoneInfo("America/Los_Angeles")
OUT = DIR / "charts"
OUT.mkdir(exist_ok=True)
COLORS = ["#2a6fdb", "#e0832b", "#2e9d5b", "#a64ca6", "#c9473f", "#7a7a7a", "#1aa3b8"]
W, H, L, R, T, B = 900, 360, 70, 20, 50, 60


def rows(name):
    with open(DIR / name, newline="") as f:
        return list(csv.DictReader(f))


def fmt(v):
    for d, s in ((1e9, "B"), (1e6, "M"), (1e3, "k")):
        if abs(v) >= d:
            return f"{v / d:.3g}{s}"
    return f"{v:.3g}"


def nice_max(v):
    if v <= 0:
        return 1
    p = 10 ** (len(str(int(v))) - 1)
    for m in (1, 2, 2.5, 5, 10):
        if v <= m * p:
            return m * p
    return 10 * p


def events():
    return [(r["when_utc"], r["label"]) for r in rows("events.csv")]


def chart(fname, title, xs, series, kind="bars", note="", keys=None):
    """xs: x labels (strings, evenly spaced); series: {name: [values]}; kind: bars (stacked), area (stacked) or line."""
    n = len(xs)
    if n == 0:
        return
    stacked = kind in ("bars", "area")
    tops = [sum(s[i] for s in series.values()) for i in range(n)] if stacked else [max(s[i] for s in series.values()) for i in range(n)]
    ymax = nice_max(max(tops) * 1.05)
    pw, ph = W - L - R, H - T - B
    X = lambda i: L + (pw * (i + 0.5) / n if kind == "bars" else pw * i / max(n - 1, 1))
    Y = lambda v: T + ph * (1 - v / ymax)
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" font-family="sans-serif" font-size="12">',
         f'<rect width="{W}" height="{H}" fill="#fff"/>',
         f'<text x="{L}" y="24" font-size="16" font-weight="bold" fill="#222">{title}</text>']
    if note:
        o.append(f'<text x="{L}" y="40" fill="#666">{note}</text>')
    for k in range(5):
        v = ymax * k / 4
        o.append(f'<line x1="{L}" x2="{W - R}" y1="{Y(v):.1f}" y2="{Y(v):.1f}" stroke="#e3e3e3"/>'
                 f'<text x="{L - 6}" y="{Y(v) + 4:.1f}" text-anchor="end" fill="#555">{fmt(v)}</text>')
    step = max(1, n // 8)
    for i in range(0, n, step):
        o.append(f'<text x="{X(i):.1f}" y="{H - B + 16}" text-anchor="middle" fill="#555">{xs[i]}</text>')
    names = list(series)
    base = [0.0] * n
    for si, name in enumerate(names):
        c = COLORS[si % len(COLORS)]
        vals = series[name]
        if kind == "bars":
            bw = max(pw / n * 0.8, 1)
            for i, v in enumerate(vals):
                if v:
                    o.append(f'<rect x="{X(i) - bw / 2:.1f}" y="{Y(base[i] + v):.1f}" width="{bw:.1f}" '
                             f'height="{Y(base[i]) - Y(base[i] + v):.1f}" fill="{c}"/>')
        elif kind == "area":
            top = [base[i] + vals[i] for i in range(n)]
            pts = [f"{X(i):.1f},{Y(top[i]):.1f}" for i in range(n)] + [f"{X(i):.1f},{Y(base[i]):.1f}" for i in reversed(range(n))]
            o.append(f'<polygon points="{" ".join(pts)}" fill="{c}" fill-opacity="0.85"/>')
        else:
            pts = " ".join(f"{X(i):.1f},{Y(vals[i]):.1f}" for i in range(n))
            o.append(f'<polyline points="{pts}" fill="none" stroke="{c}" stroke-width="2"/>')
            if n < 40:
                o += [f'<circle cx="{X(i):.1f}" cy="{Y(vals[i]):.1f}" r="3" fill="{c}"/>' for i in range(n)]
        if stacked:
            base = [base[i] + vals[i] for i in range(n)]
    if keys:
        ei = -1
        for when, label in events():
            idx = next((i for i, k in enumerate(keys) if k[:16] >= when[:16]), None)
            if idx is None or idx == 0:
                continue
            ei += 1
            x = X(idx) - (pw / n / 2 if kind == "bars" else 0)
            y = T + 12 + (ei % 5) * 14
            o.append(f'<line x1="{x:.1f}" x2="{x:.1f}" y1="{T}" y2="{T + ph}" stroke="#444" stroke-dasharray="4 3"/>')
            anchor = "end" if x > W - 170 else "start"
            dx = -4 if anchor == "end" else 4
            o.append(f'<text x="{x + dx:.1f}" y="{y}" text-anchor="{anchor}" font-size="11" fill="#222" '
                     f'stroke="#fff" stroke-width="3" paint-order="stroke">{label}</text>')
    for si, name in enumerate(names):
        x = L + si * 150
        o.append(f'<rect x="{x}" y="{H - 22}" width="12" height="12" fill="{COLORS[si % len(COLORS)]}"/>'
                 f'<text x="{x + 18}" y="{H - 12}" fill="#333">{name}</text>')
    o.append("</svg>")
    (OUT / fname).write_text("\n".join(o) + "\n")


def num(r, k):
    return float(r[k] or 0)


# agents at once (average per 15 minutes), grouped to hourly means to keep the chart readable
ag = rows("agents_15min.csv")
hours = defaultdict(lambda: defaultdict(list))
for r in ag:
    h = r["bucket_start_utc"][:13]
    for k in ("director", "opus_local", "sonnet_local", "haiku_local", "cloud_reported", "cloud_estimated"):
        hours[h][k].append(num(r, k))
hk = sorted(hours)
chart("agents-at-once.svg", "Agents working at once (hourly average, UTC)",
      [h[5:] for h in hk],
      {"director": [sum(hours[h]["director"]) / len(hours[h]["director"]) for h in hk],
       "local subagents": [sum(sum(hours[h][k]) / len(hours[h][k]) for k in ("opus_local", "sonnet_local", "haiku_local")) for h in hk],
       "cloud (reported)": [sum(hours[h]["cloud_reported"]) / len(hours[h]["cloud_reported"]) for h in hk],
       "cloud (estimated)": [sum(hours[h]["cloud_estimated"]) / len(hours[h]["cloud_estimated"]) for h in hk]},
      "area", "Partial after 2026-10-21 3:36 PM PT: only agent-reported cloud time is counted", [h + ":00Z" for h in hk])

ah = rows("agent_hours_daily.csv")
chart("agent-hours.svg", "Agent-hours per day (Pacific)", [r["date_pt"][5:] for r in ah],
      {"director": [num(r, "director") for r in ah],
       "local subagents": [num(r, "opus_local") + num(r, "sonnet_local") + num(r, "haiku_local") for r in ah],
       "cloud (reported)": [num(r, "cloud_reported") for r in ah],
       "cloud (estimated)": [num(r, "cloud_estimated") for r in ah]}, "bars",
      "Cloud before 10-21 is estimated from commits; after 10-21 3:36 PM only reported time is counted",
      [r["date_pt"] + "T07:00Z" for r in ah])

lo = rows("loc_hourly.csv")
chart("lines-of-code.svg", "Lines in the repository, by hour", [r["hour_pt"][5:13] for r in lo],
      {"Rust source": [num(r, "rust_src") for r in lo], "Rust tests and benches": [num(r, "rust_tests") for r in lo],
       "docs": [num(r, "docs") for r in lo], "tooling and CI": [num(r, "tooling") for r in lo],
       "other": [num(r, "other") for r in lo]}, "area",
      keys=[datetime.fromisoformat(r["hour_pt"]).astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%MZ") for r in lo])

cm = rows("commits_daily.csv")
chart("commits.svg", "Commits on main per day (Pacific)", [r["date_pt"][5:] for r in cm],
      {"commits": [num(r, "commits") for r in cm]}, "bars", keys=[r["date_pt"] + "T07:00Z" for r in cm])

pr = defaultdict(lambda: [0, 0])
for r in rows("prs_daily.csv"):
    pr[r["date_pt"]][0] += num(r, "opened")
    pr[r["date_pt"]][1] += num(r, "merged")
pd = sorted(pr)
chart("pull-requests.svg", "Pull requests per day (both repositories)", [d[5:] for d in pd],
      {"opened": [pr[d][0] for d in pd], "merged": [pr[d][1] for d in pd]}, "line", keys=[d + "T07:00Z" for d in pd])

iss = rows("issues_daily.csv")
chart("issues.svg", "Issues opened and closed per day", [r["date_pt"][5:] for r in iss],
      {"opened": [num(r, "opened") for r in iss], "closed": [num(r, "closed") for r in iss]}, "line", keys=[r["date_pt"] + "T07:00Z" for r in iss])

ci = defaultdict(lambda: [0, 0, 0])
for r in rows("ci_daily.csv"):
    c = ci[r["date_pt"]]
    c[0] += num(r, "success")
    c[1] += num(r, "failure")
    c[2] += num(r, "runs") - num(r, "success") - num(r, "failure")
cd = sorted(ci)
chart("ci-runs.svg", "CI runs per day", [d[5:] for d in cd],
      {"success": [ci[d][0] for d in cd], "failure": [ci[d][1] for d in cd], "cancelled, skipped or other": [ci[d][2] for d in cd]}, "bars", keys=[d + "T07:00Z" for d in cd])

bk = rows("backlog_snapshots.csv")
chart("backlog.svg", "Open issues and open pull requests (one point per refresh)", [r["snapshot_utc"][5:13] for r in bk],
      {"open issues": [num(r, "open_issues") for r in bk], "open PRs": [num(r, "open_prs") for r in bk]}, "line")


cu = rows("cloud_usage_snapshots.csv")
chart("cloud-tokens.svg", "Cloud project tokens, cumulative (one point per refresh)", [r["snapshot_utc"][5:13] for r in cu],
      {"output": [num(r, "output_tokens") for r in cu], "input": [num(r, "input_tokens") for r in cu]}, "line",
      "Cache-read and cache-write tokens are in cloud_usage_snapshots.csv", keys=[r["snapshot_utc"] for r in cu])

# Cumulative tokens of every kind (input, output, cache write and read) per day: the local sessions' logs, the cloud
# sessions before the move (estimated) and the cloud project's own totals (its latest snapshot each day).
tk = defaultdict(dict)
for r in rows("tokens_daily.csv"):
    tk[r["source"]][r["date_pt"]] = num(r, "cum_total")
for r in cu:
    d = datetime.fromisoformat(r["snapshot_utc"].replace("Z", "+00:00")).astimezone(PT).strftime("%Y-%m-%d")
    tk["cloud project"][d] = sum(num(r, k) for k in ("input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens"))
td = sorted({d for v in tk.values() for d in v})


def carry(v):
    run, out = 0.0, []
    for d in td:
        run = v.get(d, run)
        out.append(run)
    return out


chart("tokens-cumulative.svg", "Tokens, cumulative (all kinds)", [d[5:] for d in td],
      {"local sessions": carry(tk["local_measured"]), "cloud (est.)": carry(tk["cloud_estimated"]),
       "cloud project": carry(tk["cloud project"])}, "area",
      "Mostly cache reads; the split by kind is in tokens_daily.csv", keys=[d + "T07:00Z" for d in td])


# ---- index.html: one self-contained page with the headline numbers, the charts and the story ---------------------
import html
import json

s = json.loads((DIR / "summary.json").read_text())
loc, pr_, iss_ = s["loc_now"], s["prs"], s["issues_this_repo"]
cards = [("Agent-hours", f'{s["agent_hours_total"]:,.0f}'), ("Most agents at once", str(s["peak_parallel_agents"]["all"])),
         ("Lines of code", f'{loc["total"]:,}'), ("Commits on main", f'{s["commits_on_main"]:,}'),
         ("Pull requests", f'{pr_["this_repo_opened"] + pr_["earlier_repo_opened"]:,}'),
         ("Issues opened", f'{iss_["opened"]:,}'), ("CI runs", f'{s["ci_runs"]:,}')]
titles = {"agents-at-once": "Agents at once", "agent-hours": "Agent-hours per day", "lines-of-code": "Lines of code",
          "commits": "Commits", "pull-requests": "Pull requests", "issues": "Issues", "backlog": "Backlog", "cloud-tokens": "Cloud tokens", "tokens-cumulative": "Tokens, cumulative",
          "ci-runs": "CI runs"}
figs = "".join(f'<figure>{(OUT / (k + ".svg")).read_text()}</figure>' for k in titles if (OUT / (k + ".svg")).exists())
story = html.escape((DIR / "README.md").read_text().split("## Timeline", 1)[1].split("## The files", 1)[0].strip())
page = f"""<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>How Omuzaki was built</title>
<style>body{{font:16px/1.5 system-ui,sans-serif;margin:0;background:#f6f7f9;color:#1c1f24}}main{{max-width:960px;margin:0 auto;padding:24px 16px}}
h1{{margin:0 0 4px}}p.sub{{color:#5b6470;margin:0 0 20px}}.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:12px;margin-bottom:24px}}
.card{{background:#fff;border-radius:10px;padding:12px 14px;box-shadow:0 1px 2px #0002}}.card b{{display:block;font-size:24px}}.card span{{color:#5b6470;font-size:13px}}
figure{{margin:0 0 16px;background:#fff;border-radius:10px;padding:8px;box-shadow:0 1px 2px #0002;overflow-x:auto}}figure svg{{width:100%;height:auto;min-width:560px}}
pre{{white-space:pre-wrap;background:#fff;border-radius:10px;padding:16px;box-shadow:0 1px 2px #0002;font:14px/1.6 system-ui,sans-serif}}</style>
<main><h1>How Omuzaki was built</h1><p class="sub">Updated {html.escape(s["generated"])}. Aggregates only; the data is in the CSV files next to this page.</p>
<div class="cards">{"".join(f'<div class="card"><b>{v}</b><span>{k}</span></div>' for k, v in cards)}</div>
{figs}<h2>How this project was created</h2><pre>{story}</pre></main></html>
"""
(DIR / "index.html").write_text(page)
