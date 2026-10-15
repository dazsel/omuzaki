#!/usr/bin/env python3
"""Stimulus clip for the V0 glass-to-glass latency harness (#1967).

A drawn (never human) face whose mouth snaps from closed to open at known frames, with the frame number burned in as a
row of black and white cells so the analysis can tell which stimulus frame a recording frame shows. Standard library
only; frames are written as PNG files and the layout the analysis needs goes to `layout.json`.

    python3 tools/latency/stimulus.py OUT_DIR [--fps 60] [--events 60]

Encode and play the frames with an external tool, for example `ffmpeg -framerate 60 -i OUT_DIR/frame_%05d.png`
(`docs/maintainers/latency-harness.md`). ffmpeg is called as a separate program and is never linked or shipped.
"""
import argparse
import json
import os
import struct
import sys
import zlib

WIDTH = 320
HEIGHT = 320
COUNTER_BITS = 16
CELL = 16
# Layout in clip pixels: the counter strip along the top, the mouth box inside the face.
COUNTER_ROI = (0, 0, COUNTER_BITS * CELL, CELL)
MOUTH_ROI = (112, 220, 96, 56)
MOUTH_OPEN_LUMA = "dark"
SKIN = 200
INK = 30
# One event: the mouth is closed for PERIOD - OPEN frames, then open for OPEN frames. 90 frames at 60 fps leaves the
# tracker and the avatar time to settle between events.
PERIOD = 90
OPEN = 45


def schedule(events):
    """Clip frame numbers at which the mouth opens, one per event."""
    return [PERIOD // 2 + k * PERIOD for k in range(events)]


def clip_length(events):
    """Number of frames in a clip with `events` events."""
    return events * PERIOD + PERIOD // 2


def mouth_is_open(n):
    """Whether the mouth is open in clip frame `n`."""
    return n >= PERIOD // 2 and (n - PERIOD // 2) % PERIOD < OPEN


def _fill_rect(img, x, y, w, h, value):
    row = bytes([value]) * w
    for r in range(y, y + h):
        img[r * WIDTH + x:r * WIDTH + x + w] = row


def _fill_ellipse(img, cx, cy, rx, ry, value):
    for r in range(max(0, cy - ry), min(HEIGHT, cy + ry + 1)):
        dy = (r - cy) / ry
        half = int(rx * (1 - dy * dy) ** 0.5)
        x0 = max(0, cx - half)
        x1 = min(WIDTH, cx + half + 1)
        img[r * WIDTH + x0:r * WIDTH + x1] = bytes([value]) * (x1 - x0)


def base_face(open_mouth):
    """The face without the counter, as `WIDTH * HEIGHT` grey bytes: head, eyes, brows, and the mouth in one state."""
    img = bytearray([255]) * (WIDTH * HEIGHT)
    _fill_ellipse(img, WIDTH // 2, HEIGHT // 2 + 20, 130, 140, SKIN)
    for ex in (110, 210):
        _fill_ellipse(img, ex, 150, 18, 12, 250)
        _fill_ellipse(img, ex, 150, 7, 7, INK)
        _fill_rect(img, ex - 22, 120, 44, 6, INK)
    _fill_rect(img, WIDTH // 2 - 6, 160, 12, 40, 170)
    mx, my, mw, mh = MOUTH_ROI
    if open_mouth:
        _fill_ellipse(img, mx + mw // 2, my + mh // 2, mw // 2, mh // 2, INK)
    else:
        _fill_rect(img, mx + 8, my + mh // 2 - 2, mw - 16, 4, INK)
    return img


def paint_counter(img, n):
    """Burn clip frame number `n` into the counter strip, most significant bit first, white cell = 1."""
    for bit in range(COUNTER_BITS):
        value = 255 if (n >> (COUNTER_BITS - 1 - bit)) & 1 else 0
        _fill_rect(img, bit * CELL, 0, CELL, CELL, value)


def render_frame(n, faces):
    """Clip frame `n` as grey bytes; `faces` is `(closed, open)` from `base_face`."""
    img = bytearray(faces[1 if mouth_is_open(n) else 0])
    paint_counter(img, n)
    return img


def write_png(path, img):
    """Write `WIDTH * HEIGHT` grey bytes as an 8-bit greyscale PNG."""
    raw = b"".join(b"\x00" + bytes(img[r * WIDTH:(r + 1) * WIDTH]) for r in range(HEIGHT))

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 0, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def layout(fps, events):
    """The facts the analysis needs about the clip."""
    return {
        "width": WIDTH, "height": HEIGHT, "fps": fps, "frames": clip_length(events),
        "counter_roi": list(COUNTER_ROI), "counter_bits": COUNTER_BITS,
        "mouth_roi": list(MOUTH_ROI), "mouth_open_luma": MOUTH_OPEN_LUMA,
        "open_frames": schedule(events),
    }


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("out_dir")
    ap.add_argument("--fps", type=int, default=60)
    ap.add_argument("--events", type=int, default=60, help="mouth-open events (at least 50 are needed)")
    args = ap.parse_args(argv)
    if args.events < 1 or args.fps < 1 or clip_length(args.events) >= 1 << COUNTER_BITS:
        print("stimulus: --events and --fps must be positive and the clip must fit the counter", file=sys.stderr)
        return 2
    os.makedirs(args.out_dir, exist_ok=True)
    faces = (base_face(False), base_face(True))
    for n in range(clip_length(args.events)):
        write_png(os.path.join(args.out_dir, f"frame_{n:05d}.png"), render_frame(n, faces))
    with open(os.path.join(args.out_dir, "layout.json"), "w") as f:
        json.dump(layout(args.fps, args.events), f, indent=2, sort_keys=True)
        f.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
