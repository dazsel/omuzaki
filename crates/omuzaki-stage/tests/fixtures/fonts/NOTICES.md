# Notices for the test font subsets

These files exist only for tests (`tests/sheet_fonts.rs`). They are **not compiled into the crate** and never shipped:
the product downloads the full pinned fonts on first need (`src/sheet/layout/fallback/catalog.rs`).

Each is a glyph subset (3 to 29 KB) of a Noto font, made by `make_subsets.py` (fontTools `pyftsubset`; keeps the
sample names' glyphs and the layout tables; drops hinting). Subsetting is a modification under the OFL; the name table
still carries the original copyright and licence, and the licence texts are beside the files.

| Files | Source (pinned in `catalog.rs`) | Licence | Copyright |
|---|---|---|---|
| `NotoSansJP/SC/KR-Regular/Medium.otf` | notofonts/noto-cjk `Sans/SubsetOTF`, commit f8d1575 | SIL OFL 1.1, `OFL-noto-cjk.txt` | © 2014-2021 Adobe (http://www.adobe.com/). |
| `NotoSansArabic-Regular/Medium.ttf` | notofonts/arabic, commit 43674fa | SIL OFL 1.1, `OFL-noto-sans-arabic.txt` | Copyright 2022 The Noto Project Authors (https://github.com/notofonts/arabic) |
| `NotoSansDevanagari-Regular/Medium.ttf` | notofonts/devanagari, commit 46287b3 | SIL OFL 1.1, `OFL-noto-sans-devanagari.txt` | Copyright 2022 The Noto Project Authors (https://github.com/notofonts/devanagari) |
| `NotoSansThai-Regular/Medium.ttf` | notofonts/thai, commit e149ed2 | SIL OFL 1.1, `OFL-noto-sans-thai.txt` | Copyright 2022 The Noto Project Authors (https://github.com/notofonts/thai) |
| `NotoEmoji-Regular.ttf` | google/fonts `ofl/notoemoji/NotoEmoji[wght].ttf`, commit bd8f81d | SIL OFL 1.1, `OFL-noto-emoji.txt` | Copyright 2013 Google LLC |

Noto is a trademark of Google Inc. The fonts are not sold on their own.
