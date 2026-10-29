#!/usr/bin/env python3
"""The reference loop's compare step (#2399): our render beside a reference, as contact sheets and numbers.

    python3 tools/reference-loop/compare.py OURS.webm REFERENCE.mp4 OUT_DIR [--fps 2] [--columns 6]

Writes to OUT_DIR:
  - sheet-NN.png: rows of frame pairs at --fps, ours on the left of each pair, the reference on the right, each pair
    labelled with its time, so a person or an agent reads them shot for shot;
  - measures.json: for each video, its length, its cuts (frames where the picture changes a lot: a cut or a punch-in)
    and cuts per minute, and the share of near-black pixels (a dark backdrop), side by side.

It reads the videos through the `ffmpeg` and `ffprobe` command-line tools on the machine; nothing is linked or shipped.
A reference that isn't the project's own stays out of the repository: keep OUT_DIR outside it.
"""

import argparse
import json
import pathlib
import subprocess
import sys

CUT_THRESHOLD = 0.3  # ffmpeg's scene score above which a frame counts as a cut
THUMB_HEIGHT = 480


def probe_duration(path: pathlib.Path) -> float:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "json", str(path)],
        check=True, capture_output=True, text=True,
    ).stdout
    return float(json.loads(out)["format"]["duration"])


def cuts(path: pathlib.Path) -> list[float]:
    """Times of the frames whose scene score passes CUT_THRESHOLD."""
    run = subprocess.run(
        ["ffmpeg", "-hide_banner", "-nostats", "-i", str(path), "-vf",
         f"select='gt(scene,{CUT_THRESHOLD})',showinfo", "-an", "-f", "null", "-"],
        capture_output=True, text=True,
    )
    times = []
    for line in run.stderr.splitlines():
        if "showinfo" in line and "pts_time:" in line:
            times.append(float(line.split("pts_time:")[1].split()[0]))
    return times


def dark_share(path: pathlib.Path) -> float:
    """The mean share of pixels darker than 8% luma, over frames sampled at 1 fps."""
    run = subprocess.run(
        ["ffmpeg", "-hide_banner", "-nostats", "-i", str(path), "-vf",
         "fps=1,scale=64:-2,format=gray", "-f", "rawvideo", "-"],
        capture_output=True,
    )
    data = run.stdout
    if not data:
        return 0.0
    return sum(1 for b in data if b < 20) / len(data)


def measures(path: pathlib.Path) -> dict:
    duration = probe_duration(path)
    found = cuts(path)
    return {
        "file": path.name,
        "duration_s": round(duration, 2),
        "cuts": len(found),
        "cuts_per_minute": round(len(found) * 60.0 / duration, 1) if duration else 0.0,
        "dark_share": round(dark_share(path), 3),
    }


def frames(path: pathlib.Path, out: pathlib.Path, fps: float) -> list[pathlib.Path]:
    out.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(path), "-vf",
         f"fps={fps},scale=-2:{THUMB_HEIGHT}", str(out / "f%04d.png")],
        check=True,
    )
    return sorted(out.glob("f*.png"))


def sheets(ours: list[pathlib.Path], theirs: list[pathlib.Path], out: pathlib.Path, fps: float, columns: int) -> list[str]:
    """Pairs laid out `columns` pairs to a row, eight rows to a sheet, with the time drawn on each pair."""
    pairs = max(len(ours), len(theirs))
    per_sheet = columns * 8
    written = []
    for sheet in range(0, pairs, per_sheet):
        inputs, filters, labels = [], [], []
        for k, i in enumerate(range(sheet, min(sheet + per_sheet, pairs))):
            for side, (frames_, tag) in enumerate(((ours, "o"), (theirs, "r"))):
                src = frames_[min(i, len(frames_) - 1)]
                inputs += ["-i", str(src)]
                n = len(inputs) // 2 - 1
                filters.append(f"[{n}:v]scale=-2:{THUMB_HEIGHT},pad=iw+4:ih+4:2:2:white[{tag}{k}]")
            t = i / fps
            filters.append(
                f"[o{k}][r{k}]hstack=inputs=2,drawtext=text='{t:.1f}s':x=8:y=8:fontsize=28:fontcolor=white:"
                f"box=1:boxcolor=black@0.6[p{k}]"
            )
            labels.append(f"[p{k}]")
        count = len(labels)
        # xstack wants positions as sums of earlier widths and heights; every pair is the same size, so use the first.
        layout = "|".join(
            ("+".join(["w0"] * (k % columns)) or "0") + "_" + ("+".join(["h0"] * (k // columns)) or "0")
            for k in range(count)
        )
        filters.append(f"{''.join(labels)}xstack=inputs={count}:layout={layout}:fill=black[out]" if count > 1
                       else f"{labels[0]}null[out]")
        target = out / f"sheet-{sheet // per_sheet + 1:02d}.png"
        subprocess.run(
            ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *inputs, "-filter_complex", ";".join(filters),
             "-map", "[out]", "-frames:v", "1", str(target)],
            check=True,
        )
        written.append(target.name)
    return written


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("ours", type=pathlib.Path)
    parser.add_argument("reference", type=pathlib.Path)
    parser.add_argument("out", type=pathlib.Path)
    parser.add_argument("--fps", type=float, default=2.0, help="frame pairs per second of video (default 2)")
    parser.add_argument("--columns", type=int, default=6, help="pairs per row (default 6)")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    ours = frames(args.ours, args.out / "frames-ours", args.fps)
    theirs = frames(args.reference, args.out / "frames-reference", args.fps)
    written = sheets(ours, theirs, args.out, args.fps, args.columns)
    report = {"ours": measures(args.ours), "reference": measures(args.reference), "sheets": written}
    (args.out / "measures.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
