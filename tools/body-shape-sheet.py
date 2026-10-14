#!/usr/bin/env python3
"""Composes the body-shape contact sheets from the pictures the `sheet` test writes.

    OMUZAKI_SHEET_DIR=$TMPDIR/body-shape-sheet cargo test -p omuzaki-char --test body_shape -- --ignored sheet
    python3 tools/body-shape-sheet.py $TMPDIR/body-shape-sheet $OMUZAKI_ASSETS/renders/body-shape

Writes `contact-sheet.png` (every axis at -1, 0 and +1, front and side, then four corners of the cube) and
`presets-sheet.png` (the ten presets on every template, front and side). Each tile is one silhouette: grey is the
template as it is, blue the shaped body, so growth shows as blue past the grey and shrinkage as grey past the blue.
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

SCALE = 0.5  # the pictures are 1 mm per pixel
LABEL = 190
GREY, BLUE, PAPER = (205, 205, 205), (47, 99, 176), (250, 250, 248)
INK, DIM = (20, 20, 20), (90, 90, 90)


def font(size):
    try:
        return ImageFont.truetype("DejaVuSans.ttf", size)
    except OSError:
        return ImageFont.load_default()


def load(path):
    return Image.open(path).convert("L").point(lambda v: 255 if v > 127 else 0)


def tile(shaped, base):
    """Grey where only the template is, blue where the shaped body is."""
    out = Image.new("RGB", shaped.size, PAPER)
    out.paste(Image.new("RGB", shaped.size, GREY), mask=base)
    out.paste(Image.new("RGB", shaped.size, BLUE), mask=shaped)
    w, h = out.size
    return out.resize((round(w * SCALE), round(h * SCALE)), Image.LANCZOS)


def read(src):
    groups = {}
    for line in (src / "manifest.tsv").read_text().splitlines():
        group, label, front, side = line.split("\t")
        groups.setdefault(group, []).append((label, front, side))
    return groups


def sheet(title, subtitle, rows, columns, src, base, tw, th):
    """`rows` is a list of (row label, [(caption, front file, side file)], per_tile); each entry takes two tiles side
    by side. The caption goes on each tile with its view (`per_tile`) or once above the pair, for long captions."""
    title_h, row_h = 70, th + 34
    img = Image.new("RGB", (LABEL + columns * tw, title_h + len(rows) * row_h + 10), PAPER)
    draw = ImageDraw.Draw(img)
    draw.text((16, 14), title, fill=INK, font=font(26))
    draw.text((16, 46), subtitle, fill=DIM, font=font(15))
    for r, (name, entries, per_tile, *own) in enumerate(rows):
        row_base = own[0] if own else base
        y = title_h + r * row_h
        draw.text((14, y + th // 2 - 10), name, fill=INK, font=font(22))
        for i, (caption, front, side) in enumerate(entries):
            for j, (view, file) in enumerate((("front", front), ("side", side))):
                x = LABEL + (i * 2 + j) * tw
                img.paste(tile(load(src / file), row_base[view]), (x, y + 30))
                if per_tile:
                    draw.text((x + 8, y + 6), f"{caption} {view}", fill=(40, 40, 40), font=font(17))
            if not per_tile:
                draw.text((LABEL + i * 2 * tw + 8, y + 6), f"{caption}  (front, side)", fill=(40, 40, 40), font=font(15))
    return img


def main(src, dst):
    src, dst = Path(src), Path(dst)
    dst.mkdir(parents=True, exist_ok=True)
    groups = read(src)
    base = {v: load(src / f) for _, f, s in [e for e in groups["height"] if e[0] == "0"] for v, f in (("front", f), ("side", s))}
    tw, th = (round(x * SCALE) for x in base["front"].size)

    axes = [(g.replace("_", "/"), e, True) for g, e in groups.items() if not g.startswith("preset:") and g not in ("corner", "corners32")]
    corners = ("corners", groups["corner"], False)
    img = sheet(
        "soft-sd body-shape space: every axis at -1 / 0 / +1, front and side, then four corners of the cube",
        "grey: the template as it is. blue: the shaped body (CPU silhouettes of the section lofts).",
        axes + [corners], 8, src, base, tw, th,
    )
    img.save(dst / "contact-sheet.png", optimize=True)
    print(dst / "contact-sheet.png", img.size)

    # All 32 corners of the cube (H height, B build, C curves, M musculature, S shoulder/hip), eight to a row.
    entries = groups["corners32"]
    rows = [("corners", entries[i : i + 8], False) for i in range(0, 32, 8)]
    img = sheet("the 32 corners of the body-shape cube (soft-sd)", "H height, B build, C curves, M musculature, S shoulder/hip; + is +1, - is -1.", rows, 16, src, base, tw, th)
    img.save(dst / "corners-sheet.png", optimize=True)
    print(dst / "corners-sheet.png", img.size)

    # Each template takes two rows of five presets, each preset as a front and a side view.
    rows = []
    for group, entries in groups.items():
        if group.startswith("preset:"):
            # The grey is the template's own neutral body: the `average` preset on that template.
            own = next(e for e in entries if e[0] == "average")
            own_base = {"front": load(src / own[1]), "side": load(src / own[2])}
            rows += [(group.split(":", 1)[1], entries[:5], False, own_base), ("", entries[5:], False, own_base)]
    img = sheet(
        "body-shape presets on every template (front, side)",
        "grey: the template as it is. blue: the preset.",
        rows, 10, src, base, tw, th,
    )
    img.save(dst / "presets-sheet.png", optimize=True)
    print(dst / "presets-sheet.png", img.size)


if __name__ == "__main__":
    main(*sys.argv[1:3])
