#!/usr/bin/env python3
"""Makes the MJPG fixtures for tests/convert.rs: a 64x48 synthetic gradient with a checker patch, saved as baseline JPEG
(quality 90) at each chroma subsampling, plus Pillow's own decode of each as raw RGB8 (the reference).

Run from anywhere: `python3 make_fixtures.py` (needs Pillow). Outputs sit next to this script.
"""
import io
from pathlib import Path

from PIL import Image

W, H = 64, 48
# Pillow's `subsampling` argument: 0 = 4:4:4, 1 = 4:2:2, 2 = 4:2:0.
SUBSAMPLINGS = {"444": 0, "422": 1, "420": 2}


def synthetic() -> Image.Image:
    img = Image.new("RGB", (W, H))
    px = img.load()
    for y in range(H):
        for x in range(W):
            px[x, y] = (x * 255 // (W - 1), y * 255 // (H - 1), 255 - (x + y) * 255 // (W + H - 2))
    # An 8x8-cell checker over the middle: hard chroma and luma edges.
    for y in range(16, 40):
        for x in range(16, 48):
            on = ((x // 8) + (y // 8)) % 2 == 0
            px[x, y] = (230, 40, 40) if on else (30, 60, 220)
    return img


def main() -> None:
    here = Path(__file__).resolve().parent
    img = synthetic()
    for name, sub in SUBSAMPLINGS.items():
        buf = io.BytesIO()
        img.save(buf, format="JPEG", quality=90, subsampling=sub)
        jpeg = buf.getvalue()
        (here / f"{W}x{H}_{name}.jpg").write_bytes(jpeg)
        decoded = Image.open(io.BytesIO(jpeg)).convert("RGB")
        (here / f"{W}x{H}_{name}.rgb").write_bytes(decoded.tobytes())


if __name__ == "__main__":
    main()
