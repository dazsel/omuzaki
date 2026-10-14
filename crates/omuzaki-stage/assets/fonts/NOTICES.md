# Notices for the fonts embedded in omuzaki-stage

The character-sheet layout engine (`src/sheet/layout/text.rs`) compiles these two files into the crate.

| What | Where | Licence | Copyright |
|---|---|---|---|
| Inter 4.1 Regular and SemiBold, Latin subset | `Inter-Regular.subset.ttf`, `Inter-SemiBold.subset.ttf` | SIL OFL 1.1, text in `OFL.txt` | 2016 The Inter Project Authors (https://github.com/rsms/inter) |

- **Source:** the `extras/ttf/Inter-Regular.ttf` and `Inter-SemiBold.ttf` files of the Inter 4.1 release
  (https://github.com/rsms/inter/releases/tag/v4.1).
- **Modified, how:** `make_subset.py` (fontTools) keeps Basic Latin, Latin-1 and a few typographic symbols (about 330
  glyphs, 56 and 59 KB, including the combining diacritics U+0300..036F), drops hinting, aliases the tabular digits at U+E030..U+E039, and flattens the GPOS `kern`
  feature into a `kern` table, because `ab_glyph` reads neither GPOS nor GSUB. Run it with
  `python3 make_subset.py <Inter-4.1>/extras/ttf <this directory>`; the output names are the ones above.
- **Reserved Font Name:** the Inter licence declares none, so the subset keeps the family name. The font is not sold on
  its own.
- **Scope:** Latin text only. Other scripts use the lazily downloaded Noto fallback fonts (`src/sheet/layout/fallback/`,
  register rows in `docs/licensing/register.md`, test subsets in `tests/fixtures/fonts/`); a character no font draws is
  listed by `SheetLayout::font_report` instead of silently drawn as a box.
- **Coverage table:** `coverage.bin` (31 KB, made by `make_coverage.py` from the cmaps of the pinned Noto fonts) says which
  code points each fallback font draws, so the layout downloads only the font a character needs.
