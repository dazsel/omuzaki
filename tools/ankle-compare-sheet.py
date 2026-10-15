#!/usr/bin/env python3
"""Before/after ankle crops, Live and Offline, of chosen presets, from the OBJ files the `dump` test writes.

    for d in live offline; do OMUZAKI_SHEET_DETAIL=$d OMUZAKI_SHEET_DIR=<dir>/before-$d cargo test -p omuzaki-char --test body -- --ignored dump; done
    (the same into <dir>/after-live and <dir>/after-offline on the changed code)
    python3 tools/ankle-compare-sheet.py <dir> <out-dir> soft-sd pear,slim,broad,curvy,hourglass,petite

Writes `<template>-ankle-<view>.png` per view (front, side, three-quarter): a column per preset and a labelled row per
(mesh detail, before or after), each a close crop of the left ankle drawn with body-kind-sheet.py's renderer.
"""

import importlib.util
import sys
from pathlib import Path

from PIL import Image, ImageDraw

spec = importlib.util.spec_from_file_location("sheet", Path(__file__).with_name("body-kind-sheet.py"))
sheet = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sheet)

ROWS = [("live", "before"), ("live", "after"), ("offline", "before"), ("offline", "after")]
CELL = 300


def crop(mesh, view, half, ppm):
    jx, jy, jz = mesh[3]["ankle"]
    window = {
        "front": (jx - half, jx + half),
        "side": (jz - half, jz + half),
        "three-quarter": ((jx + jz) * sheet.SQ - half, (jx + jz) * sheet.SQ + half),
    }[view]
    return sheet.render(mesh, view, (*window, jy - half, jy + half), ppm)


def main():
    root, out = Path(sys.argv[1]), Path(sys.argv[2])
    template, presets = sys.argv[3], sys.argv[4].split(",")
    out.mkdir(parents=True, exist_ok=True)
    meshes = {(d, s, p): sheet.load(root / f"{s}-{d}" / f"{template}-{p}.obj") for d, s in ROWS for p in presets}
    ref = meshes[("live", "before", presets[0])][3]
    half = 0.085 * sheet.unit_of(ref)
    ppm = CELL / (2 * half)
    for view in ("front", "side", "three-quarter"):
        img = Image.new("RGB", (190 + (CELL + 8) * len(presets), 90 + (CELL + 30) * len(ROWS)), sheet.PAPER)
        d = ImageDraw.Draw(img)
        d.text((14, 12), f"#1530, {template} left ankle, {view} view: Live and Offline meshes, before and after", fill=sheet.INK, font=sheet.font(22))
        for i, p in enumerate(presets):
            d.text((190 + i * (CELL + 8) + 8, 44), p, fill=sheet.INK, font=sheet.font(15))
        for r, (detail, state) in enumerate(ROWS):
            y = 60 + r * (CELL + 30)
            d.text((14, y + CELL // 2 - 10), f"{detail} {state}", fill=sheet.INK, font=sheet.font(16))
            for i, p in enumerate(presets):
                img.paste(crop(meshes[(detail, state, p)], view, half, ppm), (190 + i * (CELL + 8), y))
        img.save(out / f"{template}-ankle-{view}.png")


if __name__ == "__main__":
    main()
