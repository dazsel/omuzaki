#!/usr/bin/env python3
"""Renders the body kind's contact sheets from the OBJ files the `dump` test writes.

    OMUZAKI_SHEET_DIR=$TMPDIR/body-kind-sheet/obj cargo test -p omuzaki-char --test body -- --ignored dump
    python3 tools/body-kind-sheet.py $TMPDIR/body-kind-sheet/obj $OMUZAKI_ASSETS/renders/body-kind

Writes, per template, `<template>-front-side.png` (the ten presets, front above side) and `<template>-<joint>.png`
(close crops of one joint on every preset, front view, then side view). The renderer is a small orthographic z-buffer
with smooth per-pixel normals, a two-band toon shade and a dark outline where the depth jumps, so a step in the
silhouette or a crease at a junction shows as it would in a toon render.
"""

import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

SKIN, SHADE, LINE, PAPER = (249, 211, 197), (232, 168, 160), (150, 90, 84), (246, 246, 244)
INK = (20, 20, 20)
SQ = 0.5**0.5  # sin and cos of the three-quarter view's 45 degrees
UNIT = [1.0]  # the body's size over soft-sd's, for the depth jump that draws an outline
PRESETS = ["average", "slim", "broad", "curvy", "athletic", "stocky", "pear", "hourglass", "lean-tall", "petite"]
TEMPLATES = ["soft-sd", "mini", "standard", "tall"]
JOINTS = ["hip", "shoulder", "knee", "elbow", "ankle"]


def font(size):
    try:
        return ImageFont.truetype("DejaVuSans.ttf", size)
    except OSError:
        return ImageFont.load_default()


def load(path):
    """Positions, normals, triangles (all regions) and the joint table of one OBJ."""
    verts, norms, tris, joints = [], [], [], {}
    for line in path.read_text().splitlines():
        parts = line.split()
        if not parts:
            continue
        if parts[0] == "v":
            verts.append([float(x) for x in parts[1:4]])
        elif parts[0] == "vn":
            norms.append([float(x) for x in parts[1:4]])
        elif parts[0] == "f":
            tris.append([int(p.split("//")[0]) - 1 for p in parts[1:4]])
        elif parts[0] == "#" and parts[1] == "joint":
            joints[parts[2]] = [float(x) for x in parts[3:6]]
    return np.array(verts), np.array(norms), np.array(tris), joints


def project(v, view):
    """Screen x, screen y and nearness (larger is nearer the camera) of points seen from the front or from the right."""
    if view == "front":
        return v[:, 0], v[:, 1], v[:, 2]
    if view == "three-quarter":
        return (v[:, 0] + v[:, 2]) * SQ, v[:, 1], (v[:, 2] - v[:, 0]) * SQ
    return v[:, 2], v[:, 1], -v[:, 0]


def rotate_normal(n, view):
    if view == "three-quarter":
        return np.stack([(n[:, 0] + n[:, 2]) * SQ, n[:, 1], (n[:, 2] - n[:, 0]) * SQ], axis=1)
    return n if view == "front" else np.stack([n[:, 2], n[:, 1], -n[:, 0]], axis=1) * np.array([1, 1, 1])


def render(mesh, view, window, px_per_m):
    """RGB image of the mesh in `window` = (x0, x1, y0, y1) metres."""
    v, n, tris, joints = mesh
    UNIT[0] = unit_of(joints)
    x0, x1, y0, y1 = window
    w, h = round((x1 - x0) * px_per_m), round((y1 - y0) * px_per_m)
    sx, sy, depth = project(v, view)
    px, py = (sx - x0) * px_per_m, (y1 - sy) * px_per_m
    if view == "front":
        nrm = n
    else:
        nrm = np.stack([n[:, 2], n[:, 1], -n[:, 0]], axis=1)
    zbuf = np.full((h, w), -1e9)
    normal = np.zeros((h, w, 3))
    for a, b, c in tris:
        xs, ys = px[[a, b, c]], py[[a, b, c]]
        lo_x, hi_x = max(int(np.floor(xs.min())), 0), min(int(np.ceil(xs.max())), w - 1)
        lo_y, hi_y = max(int(np.floor(ys.min())), 0), min(int(np.ceil(ys.max())), h - 1)
        if lo_x > hi_x or lo_y > hi_y:
            continue
        den = (ys[1] - ys[2]) * (xs[0] - xs[2]) + (xs[2] - xs[1]) * (ys[0] - ys[2])
        if abs(den) < 1e-12:
            continue
        gx, gy = np.meshgrid(np.arange(lo_x, hi_x + 1) + 0.5, np.arange(lo_y, hi_y + 1) + 0.5)
        l0 = ((ys[1] - ys[2]) * (gx - xs[2]) + (xs[2] - xs[1]) * (gy - ys[2])) / den
        l1 = ((ys[2] - ys[0]) * (gx - xs[2]) + (xs[0] - xs[2]) * (gy - ys[2])) / den
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
        nn = l0[..., None] * nrm[a] + l1[..., None] * nrm[b] + l2[..., None] * nrm[c]
        normal[lo_y : hi_y + 1, lo_x : hi_x + 1][win] = nn[win]
    covered = zbuf > -1e8
    nrm_len = np.linalg.norm(normal, axis=2, keepdims=True)
    normal = normal / np.maximum(nrm_len, 1e-9)
    light = np.array([-0.45, 0.55, 0.70])
    light /= np.linalg.norm(light)
    lam = (normal @ light)
    band = np.clip((lam - 0.05) / 0.30, 0, 1)
    band = band * band * (3 - 2 * band)
    img = np.empty((h, w, 3))
    for k in range(3):
        img[..., k] = SHADE[k] + (SKIN[k] - SHADE[k]) * band
    # outline: where coverage ends or depth jumps by more than a little
    edge = np.zeros_like(covered)
    for dy, dx in ((0, 1), (1, 0)):
        a = covered[: h - dy, : w - dx]
        b = covered[dy:, dx:]
        jump = np.abs(zbuf[: h - dy, : w - dx] - zbuf[dy:, dx:]) > 0.012 * UNIT[0]
        d = (a != b) | (a & b & jump)
        edge[: h - dy, : w - dx] |= d
        edge[dy:, dx:] |= d
    out = np.empty((h, w, 3), dtype=np.uint8)
    out[:] = PAPER
    out[covered] = img[covered].astype(np.uint8)
    out[edge & ~(~covered & ~edge)] = LINE
    return Image.fromarray(out)


def unit_of(joints):
    return joints["head"][1] / 0.56


def sheet_overview(template, src, dst):
    meshes = {p: load(src / f"{template}-{p}.obj") for p in PRESETS}
    top = max(m[0][:, 1].max() for m in meshes.values())
    half = max(np.abs(m[0][:, 0]).max() for m in meshes.values()) + 0.03
    depth = max(np.abs(m[0][:, 2]).max() for m in meshes.values()) + 0.03
    height = top + 0.12 * unit_of(meshes["average"][3])
    ppm = 420 / height
    tile_w, tile_h = round(2 * half * ppm), 420
    side_w = round(2 * depth * ppm)
    cell = max(tile_w, side_w) + 8
    img = Image.new("RGB", (160 + cell * len(PRESETS), 90 + 2 * (tile_h + 30)), PAPER)
    d = ImageDraw.Draw(img)
    d.text((14, 12), f"body kind, template {template}: front above side, the ten presets", fill=INK, font=font(22))
    for i, p in enumerate(PRESETS):
        m = meshes[p]
        front = render(m, "front", (-half, half, -0.005, height), ppm)
        side = render(m, "side", (-depth, depth, -0.005, height), ppm)
        x = 160 + i * cell
        img.paste(front, (x + (cell - front.width) // 2, 60))
        img.paste(side, (x + (cell - side.width) // 2, 60 + tile_h + 30))
        d.text((x + 8, 62 + 2 * tile_h + 30 - 28 + 28), "", fill=INK)
        d.text((x + 8, 44), p, fill=INK, font=font(15))
    d.text((14, 60 + tile_h // 2), "front", fill=INK, font=font(16))
    d.text((14, 60 + tile_h + 30 + tile_h // 2), "side", fill=INK, font=font(16))
    img.save(dst / f"{template}-front-side.png")


def sheet_joint(template, joint, src, dst):
    """Front crops (top row) and side crops (bottom row) of `joint` for every preset."""
    meshes = {p: load(src / f"{template}-{p}.obj") for p in PRESETS}
    ref = meshes["average"][3]
    half = 0.085 * unit_of(ref)
    ppm = 300 / (2 * half)
    cell = 300 + 8
    img = Image.new("RGB", (150 + cell * len(PRESETS), 90 + 3 * (300 + 30)), PAPER)
    d = ImageDraw.Draw(img)
    d.text((14, 12), f"body kind, template {template}: {joint} (left side), front, side, then three-quarter", fill=INK, font=font(22))
    for i, p in enumerate(PRESETS):
        m = meshes[p]
        jx, jy, jz = m[3][joint]
        x = 150 + i * cell
        front = render(m, "front", (jx - half, jx + half, jy - half, jy + half), ppm)
        side = render(m, "side", (jz - half, jz + half, jy - half, jy + half), ppm)
        img.paste(front, (x, 60))
        img.paste(side, (x, 60 + 300 + 30))
        quarter = render(m, "three-quarter", ((jx + jz) * SQ - half, (jx + jz) * SQ + half, jy - half, jy + half), ppm)
        img.paste(quarter, (x, 60 + 2 * (300 + 30)))
        d.text((x + 8, 44), p, fill=INK, font=font(15))
    d.text((14, 60 + 150), "front", fill=INK, font=font(16))
    d.text((14, 60 + 300 + 30 + 150), "side", fill=INK, font=font(16))
    d.text((14, 60 + 2 * (300 + 30) + 150), "3/4", fill=INK, font=font(16))
    img.save(dst / f"{template}-{joint}.png")


def main():
    src, dst = Path(sys.argv[1]), Path(sys.argv[2])
    dst.mkdir(parents=True, exist_ok=True)
    templates = sys.argv[3].split(",") if len(sys.argv) > 3 else TEMPLATES
    joints = sys.argv[4].split(",") if len(sys.argv) > 4 else JOINTS
    for t in templates:
        sheet_overview(t, src, dst)
        for j in joints:
            sheet_joint(t, j, src, dst)


if __name__ == "__main__":
    main()
