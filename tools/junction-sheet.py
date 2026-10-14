#!/usr/bin/env python3
"""Composes the hip and shoulder junction sheets from the pictures the `pictures` test writes.

    OMUZAKI_JUNCTION_DIR=$TMPDIR/junction-sheet/pics cargo test -p omuzaki-char --test junctions -- --ignored pictures
    python3 tools/junction-sheet.py $TMPDIR/junction-sheet/pics sheet.png [shape]

One row per template for `shape` (default `template`, the body as authored): the front view, the side view, then
crops of the front view at the left shoulder and at the left hip, enlarged. Regions are coloured apart so a limb
that does not blend into the torso shows as a colour boundary; the black line is the outline of their union, so a
step or a kink in it is the defect.
"""

import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

COLOURS = [(0, 0, 0), (214, 120, 120), (230, 190, 110), (110, 160, 210), (110, 160, 210),
           (120, 190, 130), (120, 190, 130), (170, 130, 200), (170, 130, 200)]
PAPER, INK = (250, 250, 248), (20, 20, 20)
TILE, CROP, ZOOM = 0.7, 150, 3  # tile scale; crop half-size in pixels; crop enlargement


def font(size):
    try:
        return ImageFont.truetype("DejaVuSans.ttf", size)
    except OSError:
        return ImageFont.load_default()


def colour(labels):
    """The labelled picture as a coloured image with the union's outline drawn over it."""
    out = Image.new("RGB", labels.size, PAPER)
    for k, c in enumerate(COLOURS[1:], start=1):
        out.paste(Image.new("RGB", labels.size, c), mask=labels.point(lambda v, k=k: 255 if v == k else 0))
    union = labels.point(lambda v: 255 if v else 0)
    edge = ImageChops.subtract(union.filter(ImageFilter.MaxFilter(3)), union.filter(ImageFilter.MinFilter(3)))
    out.paste(Image.new("RGB", labels.size, INK), mask=edge)
    return out


def crop_at(img, labels, at):
    """A square crop of `img` around the torso's `at` corner: `shoulder` (top, outer) or `hip` (bottom, outer)."""
    w, h = labels.size
    px = labels.load()
    rows = [y for y in range(h) if any(px[x, y] == 1 for x in range(0, w, 2))]
    y = rows[0] if at == "shoulder" else rows[-1]
    xs = [x for x in range(w) if px[x, y] == 1 or px[x, min(h - 1, y + 4)] == 1]
    x = max(xs) if xs else w // 2
    box = (x - CROP // 2, y - CROP // 2 + (CROP // 3 if at == "shoulder" else -CROP // 3), x + CROP // 2 + CROP // 2,
           y + CROP // 2 + (CROP // 3 if at == "shoulder" else -CROP // 3))
    return img.crop(box).resize((CROP * 3 // 2 * ZOOM, CROP * ZOOM), Image.LANCZOS)


def main(picdir, out, shape="template"):
    rows = []
    for line in Path(picdir, "manifest.tsv").read_text().splitlines():
        t, name, front, side, *_ = line.split("\t")
        if name != shape:
            continue
        f, s = (Image.open(Path(picdir, p)).convert("L") for p in (front, side))
        fc, sc = colour(f), colour(s)
        scaled = [im.resize((round(im.width * TILE), round(im.height * TILE)), Image.LANCZOS) for im in (fc, sc)]
        rows.append((t, scaled + [crop_at(fc, f, "shoulder"), crop_at(fc, f, "hip")]))
    width = sum(im.width for im in rows[0][1]) + 40
    height = sum(max(im.height for im in r[1]) + 30 for r in rows)
    sheet = Image.new("RGB", (width, height), PAPER)
    d = ImageDraw.Draw(sheet)
    y = 0
    for t, ims in rows:
        d.text((8, y + 6), f"{t} / {shape}: front, side, shoulder crop, hip crop", fill=INK, font=font(16))
        x = 0
        for im in ims:
            sheet.paste(im, (x, y + 30))
            x += im.width + 10
        y += max(im.height for im in ims) + 30
    sheet.save(out)


if __name__ == "__main__":
    main(*sys.argv[1:])
