#!/usr/bin/env python3
"""Draws the templates side by side at their real-world heights with a 1 m grid.

    cargo run -p omuzaki-char --example stature_lineup > lineup.json
    python3 tools/stature_lineup.py lineup.json out.png
"""
import json
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import Ellipse, Polygon


def main(src, dst):
    templates = json.load(open(src))
    # Left to right by height, so the lineup reads as a growth chart.
    templates.sort(key=lambda t: t["height_m"])
    fig, ax = plt.subplots(figsize=(11, 7.5), dpi=130)
    gap, x = 0.35, 0.0
    for t in templates:
        half = max(abs(p[0]) for region in t["regions"] for p in region) + 0.05
        x += half
        for region in t["regions"]:
            ax.add_patch(Polygon([(x + px, py) for px, py in region], closed=True, fc="#c9d6ea", ec="#33415c", lw=0.9))
        h = t["head"]
        ax.add_patch(Ellipse((x + h["centre"][0], h["centre"][1] + (h["half_up"] - h["half_down"]) / 2),
                             2 * h["half_width"], h["half_up"] + h["half_down"], fc="#f2d7c9", ec="#33415c", lw=0.9))
        ax.plot([x - half, x + half], [t["height_m"]] * 2, color="#c0392b", lw=0.8, ls="--")
        ax.text(x, -0.13, f'{t["id"]}\n{t["height_m"]:.2f} m  ({t["heads_tall"]:g} heads)', ha="center", va="top", fontsize=9)
        x += half + gap
    top = max(t["height_m"] for t in templates) + 0.2
    ax.set_xlim(-0.2, x)
    ax.set_ylim(-0.4, top)
    ax.set_aspect("equal")
    ax.set_xticks([i for i in range(int(x) + 1)])
    ax.set_yticks(list(range(int(top) + 1)))
    ax.grid(True, color="#bbbbbb", lw=0.7)
    ax.axhline(0, color="black", lw=1.2)
    ax.set_xlabel("metres (grid every 1 m)")
    ax.set_title("Body templates at their real-world stature")
    fig.tight_layout()
    fig.savefig(dst)


if __name__ == "__main__":
    main(*sys.argv[1:3])
