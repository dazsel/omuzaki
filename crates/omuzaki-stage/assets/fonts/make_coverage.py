#!/usr/bin/env python3
"""Builds `coverage.bin`: which code points each pinned fallback font draws, so the layout can pick the one font that
covers a character without downloading the others.

Usage: make_coverage.py <directory holding the downloaded catalog files> <output file>

The Regular cut of each family is read (the Medium cut covers the same characters). Format, per family in the order of
`FAMILIES` below (the same order as `fallback/coverage.rs`): a varint count of ranges, then for each range the varint
gap since the end of the previous range and the varint length minus one. Varints are LEB128. The file is a few KB.
"""
import sys
from pathlib import Path

from fontTools.ttLib import TTFont

FAMILIES = [
    "NotoSansJP-Regular.otf", "NotoSansSC-Regular.otf", "NotoSansTC-Regular.otf", "NotoSansKR-Regular.otf",
    "NotoSansArabic-Regular.ttf", "NotoSansDevanagari-Regular.ttf", "NotoSansThai-Regular.ttf",
    "NotoEmoji-Regular.ttf",
]


def varint(n: int) -> bytes:
    out = bytearray()
    while True:
        byte = n & 0x7F
        n >>= 7
        if n:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def ranges(codes):
    runs, start, prev = [], None, None
    for c in sorted(codes):
        if start is None:
            start = prev = c
        elif c == prev + 1:
            prev = c
        else:
            runs.append((start, prev))
            start = prev = c
    if start is not None:
        runs.append((start, prev))
    return runs


src, out = Path(sys.argv[1]), Path(sys.argv[2])
blob = bytearray()
for name in FAMILIES:
    font = TTFont(src / name, lazy=True)
    codes = [c for c in font.getBestCmap() if c > 0x20]
    runs = ranges(codes)
    blob += varint(len(runs))
    end = 0
    for lo, hi in runs:
        blob += varint(lo - end) + varint(hi - lo)
        end = hi + 1
    print(f"{name}: {len(codes)} code points in {len(runs)} ranges")
out.write_bytes(blob)
print(f"{out}: {len(blob)} bytes")
