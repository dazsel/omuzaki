#!/usr/bin/env python3
"""The reference loop's pixel score: our render against a reference, frame by frame, as numbers and the worst frames.

    python3 tools/reference-loop/parity.py OURS REFERENCE OUT_DIR [--size 540x960] [--offset 0] [--mask x,y,w,h ...]
        [--worst 8] [--frames N]

Both videos are read through `ffmpeg` at --size (default 540x960) and compared frame for frame: frame i of ours
against frame i + --offset of the reference. --mask leaves a rectangle (in --size pixels) out of every score, such as
a caption band or a watermark that isn't part of what is being matched.

Writes to OUT_DIR:
  - frames.csv: per frame, PSNR (dB, on RGB) and SSIM (on luma, 8x8 windows), and the share of pixels more than 16
    levels off;
  - score.json: the means, the median, the 10th percentile (the bad tail) and the worst frames;
  - worst-NN.png: the worst frames, ours | reference | the difference (brighter is further off).

PSNR above 30 dB reads as the same picture at a glance; above 40 dB, differences need looking for. Needs numpy and
Pillow. A reference that isn't the project's own stays out of the repository: keep OUT_DIR outside it.
"""

import argparse
import csv
import json
import pathlib
import subprocess
import sys

import numpy as np
from PIL import Image


def frames(path: pathlib.Path, w: int, h: int, start: int = 0):
    """Yields the frames of `path` from frame `start`, as h x w x 3 uint8 arrays."""
    vf = f"select=gte(n\\,{start}),scale={w}:{h}:flags=area" if start else f"scale={w}:{h}:flags=area"
    proc = subprocess.Popen(
        ["ffmpeg", "-loglevel", "error", "-i", str(path), "-vf", vf, "-vsync", "0", "-f", "rawvideo",
         "-pix_fmt", "rgb24", "-"],
        stdout=subprocess.PIPE,
    )
    size = w * h * 3
    try:
        while True:
            data = proc.stdout.read(size)
            if len(data) < size:
                return
            yield np.frombuffer(data, np.uint8).reshape(h, w, 3)
    finally:
        proc.kill()


def psnr(a: np.ndarray, b: np.ndarray, keep: np.ndarray) -> float:
    err = ((a.astype(np.float32) - b.astype(np.float32)) ** 2)[keep].mean()
    return 99.0 if err <= 1e-10 else float(10.0 * np.log10(255.0 ** 2 / err))


def luma(x: np.ndarray) -> np.ndarray:
    return x.astype(np.float32) @ np.array([0.299, 0.587, 0.114], np.float32)


def ssim(a: np.ndarray, b: np.ndarray, keep: np.ndarray, win: int = 8) -> float:
    """Mean SSIM over the 8x8 windows that lie wholly inside the kept area."""
    x, y = luma(a), luma(b)
    h, w = x.shape
    h, w = h - h % win, w - w % win

    def blocks(m):
        return m[:h, :w].reshape(h // win, win, w // win, win).transpose(0, 2, 1, 3).reshape(h // win, w // win, -1)

    bx, by = blocks(x), blocks(y)
    whole = blocks(keep.astype(np.float32)).min(axis=2) > 0
    mx, my = bx.mean(axis=2), by.mean(axis=2)
    vx, vy = bx.var(axis=2), by.var(axis=2)
    cov = ((bx - mx[..., None]) * (by - my[..., None])).mean(axis=2)
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    s = ((2 * mx * my + c1) * (2 * cov + c2)) / ((mx ** 2 + my ** 2 + c1) * (vx + vy + c2))
    return float(s[whole].mean()) if whole.any() else 1.0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("ours", type=pathlib.Path)
    ap.add_argument("reference", type=pathlib.Path)
    ap.add_argument("out", type=pathlib.Path)
    ap.add_argument("--size", default="540x960")
    ap.add_argument("--offset", type=int, default=0, help="the reference frame that ours' first frame matches")
    ap.add_argument("--mask", action="append", default=[], help="x,y,w,h left out of every score")
    ap.add_argument("--worst", type=int, default=8)
    ap.add_argument("--frames", type=int, default=0, help="score at most this many frames")
    args = ap.parse_args()
    w, h = (int(v) for v in args.size.lower().split("x"))
    keep = np.ones((h, w), bool)
    for m in args.mask:
        x, y, mw, mh = (int(v) for v in m.split(","))
        keep[max(y, 0):y + mh, max(x, 0):x + mw] = False
    keep3 = np.repeat(keep[..., None], 3, axis=2)
    args.out.mkdir(parents=True, exist_ok=True)
    rows, worst = [], []
    for i, (a, b) in enumerate(zip(frames(args.ours, w, h), frames(args.reference, w, h, args.offset))):
        if args.frames and i >= args.frames:
            break
        p = psnr(a, b, keep3)
        s = ssim(a, b, keep)
        off = float((np.abs(a.astype(np.int16) - b.astype(np.int16)).max(axis=2) > 16)[keep].mean())
        rows.append((i, p, s, off))
        worst.append((p, i, a.copy(), b.copy()))
        worst = sorted(worst, key=lambda t: t[0])[: args.worst]
    if not rows:
        print("no frames to compare", file=sys.stderr)
        return 1
    with open(args.out / "frames.csv", "w", newline="") as f:
        out = csv.writer(f)
        out.writerow(["frame", "psnr_db", "ssim", "share_off"])
        out.writerows((i, f"{p:.3f}", f"{s:.4f}", f"{o:.4f}") for i, p, s, o in rows)
    ps = np.array([r[1] for r in rows])
    ss = np.array([r[2] for r in rows])
    score = {
        "frames": len(rows),
        "size": [w, h],
        "offset": args.offset,
        "masks": args.mask,
        "psnr_db": {"mean": float(ps.mean()), "median": float(np.median(ps)), "p10": float(np.percentile(ps, 10))},
        "ssim": {"mean": float(ss.mean()), "median": float(np.median(ss)), "p10": float(np.percentile(ss, 10))},
        "share_off_mean": float(np.mean([r[3] for r in rows])),
        "worst_frames": [i for _, i, _, _ in worst],
    }
    (args.out / "score.json").write_text(json.dumps(score, indent=2))
    for n, (p, i, a, b) in enumerate(worst):
        diff = np.clip(np.abs(a.astype(np.int16) - b.astype(np.int16)).max(axis=2) * 3, 0, 255).astype(np.uint8)
        diff = np.repeat(diff[..., None], 3, axis=2)
        Image.fromarray(np.hstack([a, b, diff])).save(args.out / f"worst-{n:02d}-frame{i}-{p:.1f}dB.png")
    print(json.dumps(score, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
