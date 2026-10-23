#!/usr/bin/env python3
"""Renders the head-placement sheets from the OBJ files the `dump` test writes (#825).

    OMUZAKI_SHEET_DIR=$TMPDIR/head-placement/obj cargo test -p omuzaki-char --test head_placement -- --ignored dump
    python3 tools/head-placement-sheet.py $TMPDIR/head-placement/obj <output folder>

Writes `figures.png` (every template, whole figure, front and side, before and after) and `necks.png` (the same,
cropped to the neck and head). Rows are templates; columns are before front, before side, after front, after side.
"Before" is the head at soft-sd's centre, where every template put it until #825; "after" is the head as the head part
seats it. Reuses the renderer of `tools/body-kind-sheet.py`.
"""

import importlib.util
import sys
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).parent
spec = importlib.util.spec_from_file_location("sheet", HERE / "body-kind-sheet.py")
sheet = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sheet)

TEMPLATES = ["soft-sd", "mini", "standard", "tall"]
COLUMNS = [("before", "front"), ("before", "side"), ("after", "front"), ("after", "side")]


def build_sheet(src, crop):
    rows = []
    for template in TEMPLATES:
        meshes = {state: sheet.load(src / f"{template}-{state}.obj") for state in ("before", "after")}
        after = meshes["after"]
        neck_top = after[3]["head"][1]
        top = max(m[0][:, 1].max() for m in meshes.values()) + 0.03
        if crop:
            window_y = (neck_top - 0.30, top)
            half = min((window_y[1] - window_y[0]) * 0.5, 0.30)
            px = 900 if template in ("soft-sd", "mini") else 1100
        else:
            window_y = (after[0][:, 1].min() - 0.02, top)
            half = max(abs(after[0][:, 0]).max() + 0.03, 0.45)
            px = 330
        window = (-half, half, *window_y)
        rows.append((template, [sheet.render(meshes[state], view, window, px) for state, view in COLUMNS]))
    # Each column is as wide as its widest tile over all rows (the crops differ in width by template).
    widths = [max(row[k].size[0] for _, row in rows) for k in range(len(COLUMNS))]
    label_h, pad, left = 22, 6, 80
    xs = [left + sum(widths[:k]) + pad * k for k in range(len(COLUMNS))]
    width = xs[-1] + widths[-1] + pad
    height = 28 + sum(row[0].size[1] + label_h + pad for _, row in rows)
    out = Image.new("RGB", (width, height), sheet.PAPER)
    draw = ImageDraw.Draw(out)
    f = sheet.font(14)
    for k, (state, view) in enumerate(COLUMNS):
        draw.text((xs[k] + 4, 6), f"{state} - {view}", fill=sheet.INK, font=f)
    y = 28
    for template, row in rows:
        draw.text((4, y + row[0].size[1] // 2), template, fill=sheet.INK, font=f)
        for k, img in enumerate(row):
            out.paste(img, (xs[k], y))
        y += row[0].size[1] + label_h + pad
    return out


def main():
    src, dst = Path(sys.argv[1]), Path(sys.argv[2])
    dst.mkdir(parents=True, exist_ok=True)
    build_sheet(src, False).save(dst / "figures.png")
    build_sheet(src, True).save(dst / "necks.png")


if __name__ == "__main__":
    main()
