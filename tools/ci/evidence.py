#!/usr/bin/env python3
"""Renders the declared evidence cases at two checkouts and compares them. Standard library plus Pillow.

    evidence.py run --crates "omuzaki-char ..." [--changed FILE] --before DIR --after DIR --out OUT [--title TEXT]

Reads every `crates/*/evidence.toml` and `crates/*/evidence/*.toml` from the AFTER checkout. New cases go in their own
file, `crates/<crate>/evidence/<change>.toml`, so two branches never edit the same file; case names stay unique per crate. A case is selected when its crate is named, or when a
file it runs (a script, an example, its manifest) is listed in FILE. It runs each selected case in both checkouts
(the merge base and the PR head), then writes under OUT:

    <crate>/<case>/<image>.before.png  .after.png  .diff.png   the pictures and their amplified absolute difference
    <crate>/<case>/sheet.png                                   before | after | difference, one row per image
    sheet.png                                                  every case's sheet, stacked, for one viewable upload
    comment.md                                                 the PR comment (with the {{LINKS}} marker to fill in)
    stats.json                                                 per image: changed pixels, max difference

A manifest file is a list of cases (see crates/omuzaki-char/evidence.toml):

    [[case]]
    name = "head-part"                       # lower-case words and dashes; the folder name
    description = "what it shows"
    env = { SOME_DIR = "{out}" }             # extra environment; {out} is the case's scratch folder, {repo} the checkout
    steps = [                                # commands run in order, in the checkout
        { run = ["cargo", "test", "..."] },
        { run = ["cargo", "run", "..."], stdout = "lineup.json" },   # stdout saved as {out}/lineup.json
    ]
    images = ["front.ppm", "side.png"]       # files in {out} that the steps wrote; any format Pillow reads

Determinism is the case's job: fixed size, camera and seed. The difference is |after - before| per channel, the largest
of the three, multiplied by AMPLIFY, so a change of one level in an 8-bit channel still shows.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

AMPLIFY = 32  # a 1-level change becomes 32, a 8-level change saturates
PAPER, INK, DIM = (250, 250, 248), (20, 20, 20), (90, 90, 90)
MAX_ROW_WIDTH = 2400  # before | after | diff of one image are scaled down together to fit this
STEP_TIMEOUT = 900  # seconds; one case's step that runs longer is a failure, not a stall
NAME = re.compile(r"^[a-z0-9][a-z0-9-]*$")
MARKER = "<!-- visual-evidence -->"


def font(size):
    try:
        return ImageFont.truetype("DejaVuSans.ttf", size)
    except OSError:
        return ImageFont.load_default()


def declared_paths(crate, case, manifest="evidence.toml"):
    """Repo-relative files a case runs: script and file arguments of its commands, `--example` sources, and its own
    manifest file (`manifest` is its path under the crate folder)."""
    paths = {f"crates/{crate}/{manifest}"}
    for step in case["steps"]:
        argv = step["run"]
        paths.update(a for a in argv if "{" not in a and "/" in a)
        for flag, name in zip(argv, argv[1:]):
            if flag == "--example":
                paths.add(f"crates/{crate}/examples/{name}.rs")
    return paths


def load_cases(repo, crates, changed=()):
    """(crate, case) for the cases of the named crates, plus any case of another crate whose own script, example or
    manifest is in `changed`: a change confined to a sheet tool reaches no crate, yet it changes what the case draws.
    Raises ValueError on a malformed manifest."""
    cases = []
    crates_dir = Path(repo, "crates")
    manifests = [(p.parent.name, p.name, p, "evidence.toml") for p in crates_dir.glob("*/evidence.toml")]
    manifests += [(p.parent.parent.name, p.name, p, f"evidence/{p.name}") for p in crates_dir.glob("*/evidence/*.toml")]
    seen = {}  # (crate, case name) -> the manifest that declared it
    for crate, _, path, manifest in sorted(manifests, key=lambda m: (m[0], m[3] != "evidence.toml", m[1])):
        data = tomllib.loads(path.read_text(encoding="utf-8"))
        for case in data.get("case", []):
            name = case.get("name", "")
            if not NAME.match(name):
                raise ValueError(f"{path}: case name {name!r} must be lower-case words joined by dashes")
            if (crate, name) in seen:
                raise ValueError(f"case name {name!r} of crate {crate} is declared twice: in {seen[crate, name]} and {path}")
            seen[crate, name] = path
            if not case.get("steps") or not case.get("images"):
                raise ValueError(f"{path}: case {name} needs steps and images")
            if crate in crates or declared_paths(crate, case, manifest) & set(changed):
                cases.append((crate, case))
    return cases


def expand(text, out, repo):
    return text.replace("{out}", str(out)).replace("{repo}", str(repo))


def touch_sources(repo):
    """Mark every tracked file as just modified, so cargo rebuilds the workspace crates for THIS checkout.

    The two checkouts share one target directory (their dependencies are built once). Cargo decides a path crate is
    fresh when its sources are older than the last build's outputs, and a checkout made before the other one was built
    looks older, so without this it would reuse the other checkout's binaries and "before" would equal "after".
    """
    listing = subprocess.run(["git", "ls-files", "-z"], cwd=repo, check=False, capture_output=True)
    for name in listing.stdout.split(b"\0"):  # not a git checkout (a test's folder): nothing to touch
        if name and os.path.isfile(os.path.join(repo, name.decode())):
            os.utime(os.path.join(repo, name.decode()))


def run_case(case, repo, scratch):
    """Run the case in `repo`, leaving its files in `scratch`. Returns None on success, else a one-line reason."""
    shutil.rmtree(scratch, ignore_errors=True)
    scratch.mkdir(parents=True)
    touch_sources(repo)
    env = dict(os.environ)
    env.update({k: expand(v, scratch, repo) for k, v in case.get("env", {}).items()})
    for step in case["steps"]:
        argv = [expand(a, scratch, repo) for a in step["run"]]
        sink = open(scratch / step["stdout"], "wb") if "stdout" in step else subprocess.DEVNULL
        try:
            done = subprocess.run(argv, cwd=repo, env=env, stdout=sink, timeout=STEP_TIMEOUT, check=False)
        except subprocess.TimeoutExpired:
            return f"`{' '.join(argv[:3])}` timed out after {STEP_TIMEOUT} s"
        finally:
            if sink is not subprocess.DEVNULL:
                sink.close()
        if done.returncode != 0:
            return f"`{' '.join(argv[:3])}` exited with {done.returncode}"
    missing = [i for i in case["images"] if not (scratch / i).is_file()]
    return f"did not write {', '.join(missing)}" if missing else None


def padded(images):
    """The images on a common canvas (paper colour), so pictures of different sizes still compare pixel by pixel."""
    w, h = max(i.width for i in images), max(i.height for i in images)
    out = []
    for image in images:
        canvas = Image.new("RGB", (w, h), PAPER)
        canvas.paste(image, (0, 0))
        out.append(canvas)
    return out


def compare(before, after):
    """(before, after, diff, changed pixel count, max level difference) of two RGB images on a common canvas."""
    before, after = padded([before, after])
    delta = ImageChops.difference(before, after)
    r, g, b = delta.split()
    level = ImageChops.lighter(ImageChops.lighter(r, g), b)
    histogram = level.histogram()
    total = level.width * level.height
    changed = total - histogram[0]  # pixels that differ at all, counted before any rounding for display
    peak = max((i for i, n in enumerate(histogram) if n), default=0)
    diff = level.point(lambda v: min(255, v * AMPLIFY)).convert("RGB")
    return before, after, diff, changed, peak


def sheet_row(title, tiles):
    """One row: a caption, then the labelled tiles side by side, scaled together to fit MAX_ROW_WIDTH."""
    gap = 12
    width = sum(t.width for t in tiles) + gap * (len(tiles) - 1)
    scale = min(1.0, MAX_ROW_WIDTH / width)
    tiles = [t.resize((max(1, round(t.width * scale)), max(1, round(t.height * scale))), Image.LANCZOS) for t in tiles]
    row = Image.new("RGB", (sum(t.width for t in tiles) + gap * (len(tiles) - 1), tiles[0].height + 52), PAPER)
    draw = ImageDraw.Draw(row)
    draw.text((0, 2), title, fill=INK, font=font(16))
    x = 0
    for tile, label in zip(tiles, ("before", "after", f"difference (x{AMPLIFY})")):
        draw.text((x + 2, 28), label, fill=DIM, font=font(14))
        row.paste(tile, (x, 50))
        x += tile.width + gap
    return row


def stack(rows, title, subtitle):
    """Rows stacked under a title; a caption line names the three columns."""
    head = 64
    width = max(r.width for r in rows)
    height = head + sum(r.height + 10 for r in rows)
    img = Image.new("RGB", (width, height), PAPER)
    draw = ImageDraw.Draw(img)
    draw.text((0, 8), title, fill=INK, font=font(24))
    draw.text((0, 40), subtitle, fill=DIM, font=font(14))
    y = head
    for r in rows:
        img.paste(r, (0, y))
        y += r.height + 10
    return img


def blank_like(image):
    return Image.new("RGB", image.size, PAPER)


def evaluate(crate, case, before_root, after_root, scratch_root, out_root):
    """Run one case at both checkouts and write its pictures. Returns the case's record for the report."""
    folder = out_root / crate / case["name"]
    folder.mkdir(parents=True, exist_ok=True)
    record = {"crate": crate, "name": case["name"], "description": case.get("description", ""), "images": []}
    after_dir, before_dir = scratch_root / "after", scratch_root / "before"
    record["after_error"] = run_case(case, after_root, after_dir)
    record["before_error"] = run_case(case, before_root, before_dir)
    if record["after_error"]:
        return record
    rows = []
    for name in case["images"]:
        after = Image.open(after_dir / name).convert("RGB")
        base_ok = record["before_error"] is None
        before = Image.open(before_dir / name).convert("RGB") if base_ok else blank_like(after)
        b, a, d, changed, peak = compare(before, after)
        stem = Path(name).stem
        for tag, image in (("before", b), ("after", a), ("diff", d)):
            image.save(folder / f"{stem}.{tag}.png", optimize=True)
        row = sheet_row(f"{name}" + ("" if base_ok else "   (the base could not render this: before is blank)"), [b, a, d])
        rows.append(row)
        record["images"].append(
            {
                "name": name,
                "size": list(a.size),
                "changed_pixels": changed,
                "changed_percent": changed * 100 / (a.width * a.height),
                "max_diff": peak,
            }
        )
    sheet = stack(rows, f"{crate} / {case['name']}", f"before | after | difference.  {case.get('description', '')}")
    sheet.save(folder / "sheet.png", optimize=True)
    record["sheet"] = f"{crate}/{case['name']}/sheet.png"
    return record


def comment(records, title, note):
    """The PR comment: marker, one stats table, failures and the {{LINKS}} slot the workflow fills with artifact links."""
    lines = [MARKER, f"### Visual evidence {title}".rstrip(), ""]
    if note:
        lines += [note, ""]
    if not records:
        lines += ["No declared cases for the crates this change reaches (see `crates/*/evidence.toml`).", ""]
    else:
        lines += ["{{LINKS}}", "", "| case | image | size | changed pixels | max difference (0-255) |", "|---|---|---|---|---|"]
        for r in records:
            if r["after_error"]:
                lines.append(f"| `{r['crate']}/{r['name']}` | | | **failed after the change:** {r['after_error']} | |")
                continue
            for i in r["images"]:
                # "identical" only when no pixel differs; a lone changed pixel in a big picture still reads as a change.
                verdict = "identical" if i["changed_pixels"] == 0 else f"{max(i['changed_percent'], 0.001):.3f} % ({i['changed_pixels']} px)"
                lines.append(
                    f"| `{r['crate']}/{r['name']}` | {i['name']} | {i['size'][0]}x{i['size'][1]} | {verdict} | {i['max_diff']} |"
                )
        notes = [f"- `{r['crate']}/{r['name']}`: the base could not render it ({r['before_error']}); before is blank."
                 for r in records if r["before_error"] and not r["after_error"]]
        lines += [""] + notes if notes else []
    lines += ["", "<sub>Same camera, size and seed on both sides, so any difference is the change itself. "
              "The difference panel is |after - before| per pixel, amplified; black means identical.</sub>"]
    return "\n".join(lines) + "\n"


def cmd_run(args):
    crates = args.crates.split()
    before, after, out = Path(args.before).resolve(), Path(args.after).resolve(), Path(args.out).resolve()
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    changed = Path(args.changed).read_text(encoding="utf-8").split() if args.changed else []
    cases = load_cases(after, crates, changed)
    scratch = Path(os.environ.get("RUNNER_TEMP", "/tmp")) / "evidence-scratch"
    records, sheets = [], []
    for crate, case in cases:
        print(f"== {crate}/{case['name']}", flush=True)
        record = evaluate(crate, case, before, after, scratch / crate / case["name"], out)
        records.append(record)
        if record.get("sheet"):
            sheets.append(Image.open(out / record["sheet"]).convert("RGB"))
    if sheets:
        stack(sheets, "Visual evidence", args.title).save(out / "sheet.png", optimize=True)
    (out / "stats.json").write_text(json.dumps(records, indent=2), encoding="utf-8")
    (out / "comment.md").write_text(comment(records, args.title, args.note), encoding="utf-8")
    failed = [r for r in records if r["after_error"]]
    for r in failed:
        print(f"::error::{r['crate']}/{r['name']} failed after the change: {r['after_error']}")
    return 1 if failed else 0


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run")
    run.add_argument("--crates", required=True, help="space separated crate names whose cases all run (may be empty)")
    run.add_argument("--before", required=True)
    run.add_argument("--after", required=True)
    run.add_argument("--out", required=True)
    run.add_argument("--changed", help="file listing the PR's changed paths, one per line; selects cases by their tools")
    run.add_argument("--title", default="")
    run.add_argument("--note", default="")
    args = parser.parse_args(argv)
    return cmd_run(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
