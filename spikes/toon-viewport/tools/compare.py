#!/usr/bin/env python3
"""Compare the JS prototype's render with the Rust viewport's render of the same view.

usage: compare.py js.png rust.png out.png [--zoom X0 Y0 X1 Y1] [--scale N]

Writes one image: JS | Rust | amplified difference (x4, grey = identical), and prints the pixel-difference numbers.
Numbers are over the whole frame and over the character box (the bounding box of everything that differs from the
backdrop in either image, padded), because the flat backdrop would otherwise dilute them. "Blurred" compares both
images after a 5x5 box blur, which ignores one-pixel edge differences (MSAA patterns, outline rasterisation) and
shows shading differences only.
"""
import sys

import numpy as np
from PIL import Image, ImageDraw


def load(path):
    return np.asarray(Image.open(path).convert("RGB"), dtype=np.float32)


def box_blur(a, r=2):
    pad = np.pad(a, ((r, r), (r, r), (0, 0)), mode="edge")
    out = np.zeros_like(a)
    for dy in range(2 * r + 1):
        for dx in range(2 * r + 1):
            out += pad[dy : dy + a.shape[0], dx : dx + a.shape[1]]
    return out / (2 * r + 1) ** 2


def character_box(a, b, pad=12):
    bg = a[0, 0]
    mask = (np.abs(a - bg).max(axis=2) > 6) | (np.abs(b - bg).max(axis=2) > 6)
    ys, xs = np.where(mask)
    return max(xs.min() - pad, 0), max(ys.min() - pad, 0), min(xs.max() + pad, a.shape[1]), min(ys.max() + pad, a.shape[0])


def metrics(a, b):
    d = a - b
    mae = float(np.abs(d).mean())
    rmse = float(np.sqrt((d**2).mean()))
    psnr = float(20 * np.log10(255.0 / rmse)) if rmse > 0 else float("inf")
    over16 = float((np.abs(d).max(axis=2) > 16).mean() * 100)
    return mae, rmse, psnr, over16


def report(name, a, b):
    mae, rmse, psnr, over16 = metrics(a, b)
    bmae, brmse, bpsnr, _ = metrics(box_blur(a), box_blur(b))
    print(f"{name:10s} MAE {mae:6.3f}  RMSE {rmse:6.3f}  PSNR {psnr:6.2f} dB  >16: {over16:5.2f}%   | blurred MAE {bmae:6.3f}  RMSE {brmse:6.3f}  PSNR {bpsnr:6.2f} dB")


def main():
    args = sys.argv[1:]
    js, rust, out = load(args[0]), load(args[1]), args[2]
    if js.shape != rust.shape:
        sys.exit(f"size mismatch: {js.shape} vs {rust.shape}")
    x0, y0, x1, y1 = character_box(js, rust)
    report("frame", js, rust)
    report("character", js[y0:y1, x0:x1], rust[y0:y1, x0:x1])
    diff = np.clip(128 + 4 * (rust - js), 0, 255)
    panels = [js, rust, diff]
    if "--zoom" in args:
        i = args.index("--zoom")
        zx0, zy0, zx1, zy1 = (int(v) for v in args[i + 1 : i + 5])
        panels = [p[zy0:zy1, zx0:zx1] for p in panels]
        if "--scale" in args:
            k = int(args[args.index("--scale") + 1])
            panels = [np.repeat(np.repeat(p, k, axis=0), k, axis=1) for p in panels]
    else:
        panels = [p[y0:y1, x0:x1] for p in panels]
    sheet = np.concatenate(panels, axis=1).astype(np.uint8)
    img = Image.fromarray(sheet)
    draw = ImageDraw.Draw(img)
    w = panels[0].shape[1]
    for i, label in enumerate(["JS (three.js)", "Rust (wgpu)", "difference x4"]):
        draw.text((i * w + 6, 4), label, fill=(255, 255, 255))
    img.save(out)


if __name__ == "__main__":
    main()
