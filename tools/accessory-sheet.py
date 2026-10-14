#!/usr/bin/env python3
"""Renders the accessory contact sheets from the OBJ files the `dump` test writes.

    OMUZAKI_SHEET_DIR=$TMPDIR/accessory-sheet/obj cargo test -p omuzaki-char --test accessory_rigid -- --ignored dump
    python3 tools/accessory-sheet.py $TMPDIR/accessory-sheet/obj $OMUZAKI_ASSETS/renders/accessories

Writes `<template>-sheet.png`: one row per sample accessory, six tiles each: at rest in front, side and three-quarter view,
then posed in the same three views. The window of a row is fixed, so the move between rest and posed shows. Each OBJ holds
`g <group>` blocks and `# colour <group> r g b` lines; the renderer is a small orthographic z-buffer with smooth normals,
a two-band toon shade and a dark outline where coverage or depth jumps, as in `tools/body-kind-sheet.py`.
"""

import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

PAPER, INK, LINE = (246, 246, 244), (20, 20, 20), (70, 54, 52)
VIEWS = [("front", 0.0), ("side", 90.0), ("three-quarter", 40.0)]
STATES = ["rest", "posed"]
TILE = 300


def font(size):
    try:
        return ImageFont.truetype("DejaVuSans.ttf", size)
    except OSError:
        return ImageFont.load_default()


def load(path):
    """Welded positions, smooth normals, triangles, the group of each triangle and the group colours of one OBJ."""
    verts, tris, tri_group, colours, groups, collider = [], [], [], {}, [], []
    for line in path.read_text().splitlines():
        parts = line.split()
        if not parts:
            continue
        if parts[0] == "v":
            verts.append([float(x) for x in parts[1:4]])
        elif parts[0] == "g":
            groups.append(parts[1])
        elif parts[0] == "f":
            tris.append([int(p) - 1 for p in parts[1:4]])
            tri_group.append(len(groups) - 1)
        elif parts[0] == "#" and parts[1] == "collider":
            collider[:] = [float(x) for x in parts[2:6]]
        elif parts[0] == "#" and parts[1] == "colour":
            colours[parts[2]] = [int(x) for x in parts[3:6]]
    v = np.array(verts)
    tris = np.array(tris)
    # Weld coincident vertices (wrapped seams, group borders) so the normals are smooth across them.
    key = np.round(v * 1e5).astype(np.int64)
    _, first, inverse = np.unique(key, axis=0, return_index=True, return_inverse=True)
    inverse = inverse.reshape(-1)
    w = v[first]
    t = inverse[tris]
    face = np.cross(w[t[:, 1]] - w[t[:, 0]], w[t[:, 2]] - w[t[:, 0]])
    n = np.zeros_like(w)
    for k in range(3):
        np.add.at(n, t[:, k], face)
    n /= np.maximum(np.linalg.norm(n, axis=1, keepdims=True), 1e-12)
    return w, n, t, np.array(tri_group), groups, colours, collider


def turn(p, degrees):
    """Points or normals turned about +Y so the camera looks along the chosen direction."""
    a = np.radians(degrees)
    c, s = np.cos(a), np.sin(a)
    return np.stack([p[:, 0] * c + p[:, 2] * s, p[:, 1], -p[:, 0] * s + p[:, 2] * c], axis=1)


def render(mesh, yaw, window, px_per_m):
    """RGB image of the mesh seen after turning by `yaw`, in `window` = (x0, x1, y0, y1) metres."""
    v, n, tris, tri_group, groups, colours, collider = mesh
    v, n = turn(v, yaw), turn(n, yaw)
    x0, x1, y0, y1 = window
    w, h = round((x1 - x0) * px_per_m), round((y1 - y0) * px_per_m)
    px, py, depth = (v[:, 0] - x0) * px_per_m, (y1 - v[:, 1]) * px_per_m, v[:, 2]
    # Keep only the triangles whose box touches the tile.
    xs, ys = px[tris], py[tris]
    keep = (xs.max(1) >= 0) & (xs.min(1) <= w) & (ys.max(1) >= 0) & (ys.min(1) <= h)
    base = np.array([colours.get(g, [200, 200, 200]) for g in groups], dtype=float)
    zbuf = np.full((h, w), -1e9)
    normal = np.zeros((h, w, 3))
    ident = np.full((h, w), -1, dtype=int)
    for idx in np.nonzero(keep)[0]:
        a, b, c = tris[idx]
        tx, ty = px[[a, b, c]], py[[a, b, c]]
        lo_x, hi_x = max(int(np.floor(tx.min())), 0), min(int(np.ceil(tx.max())), w - 1)
        lo_y, hi_y = max(int(np.floor(ty.min())), 0), min(int(np.ceil(ty.max())), h - 1)
        if lo_x > hi_x or lo_y > hi_y:
            continue
        den = (ty[1] - ty[2]) * (tx[0] - tx[2]) + (tx[2] - tx[1]) * (ty[0] - ty[2])
        if abs(den) < 1e-12:
            continue
        gx, gy = np.meshgrid(np.arange(lo_x, hi_x + 1) + 0.5, np.arange(lo_y, hi_y + 1) + 0.5)
        l0 = ((ty[1] - ty[2]) * (gx - tx[2]) + (tx[2] - tx[1]) * (gy - ty[2])) / den
        l1 = ((ty[2] - ty[0]) * (gx - tx[2]) + (tx[0] - tx[2]) * (gy - ty[2])) / den
        l2 = 1 - l0 - l1
        inside = (l0 >= -1e-6) & (l1 >= -1e-6) & (l2 >= -1e-6)
        if not inside.any():
            continue
        z = l0 * depth[a] + l1 * depth[b] + l2 * depth[c]
        sub = zbuf[lo_y : hi_y + 1, lo_x : hi_x + 1]
        win = inside & (z > sub)
        if not win.any():
            continue
        sub[win] = z[win]
        nn = l0[..., None] * n[a] + l1[..., None] * n[b] + l2[..., None] * n[c]
        normal[lo_y : hi_y + 1, lo_x : hi_x + 1][win] = nn[win]
        ident[lo_y : hi_y + 1, lo_x : hi_x + 1][win] = tri_group[idx]
    covered = zbuf > -1e8
    normal /= np.maximum(np.linalg.norm(normal, axis=2, keepdims=True), 1e-9)
    light = np.array([-0.45, 0.55, 0.70])
    light /= np.linalg.norm(light)
    band = np.clip(((normal @ light) - 0.05) / 0.30, 0, 1)
    band = band * band * (3 - 2 * band)
    colour = base[np.maximum(ident, 0)]
    shade = colour * np.array([0.78, 0.68, 0.70])
    img = shade + (colour - shade) * band[..., None]
    edge = np.zeros_like(covered)
    for dy, dx in ((0, 1), (1, 0)):
        a_, b_ = covered[: h - dy, : w - dx], covered[dy:, dx:]
        jump = np.abs(zbuf[: h - dy, : w - dx] - zbuf[dy:, dx:]) > 0.006
        group = ident[: h - dy, : w - dx] != ident[dy:, dx:]
        d = (a_ != b_) | (a_ & b_ & (jump | group))
        edge[: h - dy, : w - dx] |= d
        edge[dy:, dx:] |= d
    out = np.empty((h, w, 3), dtype=np.uint8)
    out[:] = PAPER
    out[covered] = img[covered].astype(np.uint8)
    out[edge & covered] = LINE
    image = Image.fromarray(out)
    if collider:
        # The head collider, drawn as a dashed blue circle: a sphere seen from any side is a circle of its radius.
        c = turn(np.array([collider[:3]]), yaw)[0]
        cx, cy, r = (c[0] - x0) * px_per_m, (y1 - c[1]) * px_per_m, collider[3] * px_per_m
        draw = ImageDraw.Draw(image)
        for k in range(0, 360, 6):
            if (k // 6) % 2 == 0:
                a0, a1 = np.radians(k), np.radians(k + 6)
                draw.line([(cx + r * np.cos(a0), cy - r * np.sin(a0)), (cx + r * np.cos(a1), cy - r * np.sin(a1))], fill=(40, 90, 220), width=2)
    return image


def centre_of(mesh, group, yaw):
    v, _, tris, tri_group, groups, _, _ = mesh
    wanted = [i for i, g in enumerate(groups) if g.startswith(group)]
    sel = np.isin(tri_group, wanted)
    return turn(v[np.unique(tris[sel])], yaw).mean(axis=0)


def sheet(template, cases, src, dst):
    rows = len(cases)
    label_w, head_h = 250, 80
    img = Image.new("RGB", (label_w + 6 * (TILE + 6), head_h + rows * (TILE + 30)), PAPER)
    d = ImageDraw.Draw(img)
    d.text((14, 12), f"accessories on template {template}: rest (left three) and posed (right three)", fill=INK, font=font(22))
    col = 0
    for state in STATES:
        for view, _ in VIEWS:
            d.text((label_w + col * (TILE + 6) + 6, 50), f"{state}, {view}", fill=INK, font=font(15))
            col += 1
    for r, (case, label, half) in enumerate(cases):
        meshes = {s: load(src / f"{template}-{case}-{s}.obj") for s in STATES}
        y = head_h + r * (TILE + 30)
        for i, line in enumerate(label.split("\n")):
            d.text((10, y + 8 + 20 * i), line, fill=INK, font=font(14))
        col = 0
        for state in STATES:
            for view, yaw in VIEWS:
                centres = [centre_of(meshes[s], "acc", yaw) for s in STATES]
                c = np.mean(centres, axis=0)
                ppm = TILE / (2 * half)
                tile = render(meshes[state], yaw, (c[0] - half, c[0] + half, c[1] - half, c[1] + half), ppm)
                img.paste(tile, (label_w + col * (TILE + 6), y))
                col += 1
    img.save(dst / f"{template}-sheet.png")


CASES = [
    ("pin", "hairpin: flower on the\nhead's top anchor\nposed: head yaw 25, pitch 12", 0.13),
    ("halo", "halo over the head's top\nanchor, bobbing\nposed: head yaw 25, halo tilt 10", 0.20),
    ("choker", "choker with a drop pendant\non the neck section\nposed: neck yaw 25", 0.20),
    ("bell", "bell on a cord at ear height,\ndangling; dashed: head collider\nposed: head yaw 25, swung 25\ntowards the head", 0.16),
    ("charm", "gem charm on the left\nhand bone\nposed: arm raised 70", 0.42),
]


def main():
    src, dst = Path(sys.argv[1]), Path(sys.argv[2])
    dst.mkdir(parents=True, exist_ok=True)
    for template in (sys.argv[3].split(",") if len(sys.argv) > 3 else ["standard", "soft-sd"]):
        sheet(template, CASES, src, dst)


if __name__ == "__main__":
    main()
