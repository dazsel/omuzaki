#!/usr/bin/env python3
"""Builds the tiny test subsets of the pinned Noto fallback fonts.

    python3 make_subsets.py <directory holding the downloaded catalog files> <output directory>

Each output keeps the catalog file name, so a `DirSource::unverified` can stand in for the real download. Only the
glyphs of the sample names below are kept (a few KB each); layout tables are kept so shaping works. The names are
neutral samples, nobody's real name.
"""
import subprocess
import sys
from pathlib import Path

SAMPLES = {
    "ja": "山田 さくら カタカナ 日本語 漢字・ー",
    "zh": "王小明 你好，世界 汉字 们",
    "ko": "김하늘 한국어 이름",
    "ar": "مريم الأحمد العربية () مَر ٠١٢٣٤٥٦٧٨٩",
    "hi": "आरव शर्मा हिन्दी क्षत्रिय",
    "th": "สมชาย ใจดี ภาษาไทย",
    "emoji": "\U0001F600\U0001F3A8✨☀",
}
FILES = {
    "NotoSansJP-Regular.otf": "ja", "NotoSansJP-Medium.otf": "ja",
    "NotoSansSC-Regular.otf": "zh", "NotoSansSC-Medium.otf": "zh",
    "NotoSansKR-Regular.otf": "ko", "NotoSansKR-Medium.otf": "ko",
    "NotoSansArabic-Regular.ttf": "ar", "NotoSansArabic-Medium.ttf": "ar",
    "NotoSansDevanagari-Regular.ttf": "hi", "NotoSansDevanagari-Medium.ttf": "hi",
    "NotoSansThai-Regular.ttf": "th", "NotoSansThai-Medium.ttf": "th",
    "NotoEmoji-Regular.ttf": "emoji",
}
# The downloaded emoji file is NotoEmoji[wght].ttf upstream; the catalog calls it NotoEmoji-Regular.ttf.

src, out = Path(sys.argv[1]), Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
for name, key in FILES.items():
    # The sample text of every script goes in every font: a font simply lacks the ones it cannot draw.
    text = "".join(SAMPLES.values()) if key != "emoji" else SAMPLES["emoji"]
    subprocess.run(
        ["pyftsubset", str(src / name), f"--text={text} ", f"--output-file={out / name}",
         "--layout-features=*", "--notdef-outline", "--no-hinting"],
        check=True,
    )
