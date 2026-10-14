#!/usr/bin/env python3
"""Builds the sheet fonts: Latin subsets of Inter 4.1 (SIL OFL 1.1) with a `kern` table and tabular digits.

Usage: make_subset.py <Inter-4.1 extras/ttf dir> <output dir>

ab_glyph (the text rasteriser) reads neither GSUB nor GPOS, so the two OpenType features the sheet relies on are baked
in here instead:
  * tabular figures: the `.tf` digit glyphs are mapped at U+E030..U+E039 (a private-use alias of '0'..'9') so the
    layout code can pick aligned digits for hex codes and ratios;
  * pair kerning: the GPOS `kern` feature is flattened into a format-0 `kern` table for the retained glyphs.
Inter's licence has no Reserved Font Name, so the subset keeps the family name.
"""
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.tables._k_e_r_n import KernTable_format_0

# Basic Latin, Latin-1, and the typographic punctuation the sheet uses (dashes, quotes, bullet, ellipsis, minus,
# multiplication, arrows, comparison).
UNICODES = (
    list(range(0x20, 0x7F)) + list(range(0xA0, 0x100))
    + list(range(0x300, 0x370))   # combining diacritics, so a letter with no precomposed form still draws
    + [0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2026, 0x2192, 0x2212, 0x2264, 0x2265, 0x2248]
)
TABULAR = {0xE030 + d: name + ".tf" for d, name in enumerate(
    "zero one two three four five six seven eight nine".split())}


def subset_font(src: Path) -> TTFont:
    opts = subset.Options()
    opts.layout_features = ["kern", "case"]
    opts.glyph_names = False
    opts.notdef_outline = True
    opts.hinting = False          # the sheet rasterises unhinted at arbitrary sizes
    opts.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14]
    font = TTFont(src)
    # Alias the tabular digits first so the subsetter keeps their glyphs.
    cmap = font["cmap"].getBestCmap()
    for code, glyph in TABULAR.items():
        for table in font["cmap"].tables:
            if table.isUnicode():
                table.cmap[code] = glyph
    sub = subset.Subsetter(opts)
    sub.populate(unicodes=UNICODES + list(TABULAR))
    sub.subset(font)
    return font


def flatten_kern(font: TTFont) -> dict:
    """Evaluates the GPOS `kern` lookups for every retained glyph pair (first match wins, as in shaping)."""
    gpos = font["GPOS"].table
    lookups = set()
    for fr in gpos.FeatureList.FeatureRecord:
        if fr.FeatureTag == "kern":
            lookups.update(fr.Feature.LookupListIndex)
    glyphs = [g for g in font.getGlyphOrder() if g != ".notdef"]
    pairs = {}
    for li in sorted(lookups):
        for st in gpos.LookupList.Lookup[li].SubTable:
            if st.LookupType == 9:
                st = st.ExtSubTable
            if st.LookupType != 2:
                continue
            cov = st.Coverage.glyphs
            if st.Format == 1:
                for g1, ps in zip(cov, st.PairSet):
                    for rec in ps.PairValueRecord:
                        v = getattr(rec.Value1, "XAdvance", 0) if rec.Value1 else 0
                        if v and (g1, rec.SecondGlyph) not in pairs:
                            pairs[(g1, rec.SecondGlyph)] = v
            else:
                cd1, cd2 = st.ClassDef1.classDefs, st.ClassDef2.classDefs
                for g1 in cov:
                    c1 = cd1.get(g1, 0)
                    for g2 in glyphs:
                        if (g1, g2) in pairs:
                            continue
                        rec = st.Class1Record[c1].Class2Record[cd2.get(g2, 0)]
                        v = getattr(rec.Value1, "XAdvance", 0) if rec.Value1 else 0
                        if v:
                            pairs[(g1, g2)] = v
    return pairs


def main(src_dir: Path, out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    for style in ("Regular", "SemiBold"):
        font = subset_font(src_dir / f"Inter-{style}.ttf")
        pairs = flatten_kern(font)
        kern = newTable("kern")
        kern.version = 0
        sub = KernTable_format_0()
        sub.version, sub.coverage, sub.format = 0, 1, 0
        sub.kernTable = pairs
        kern.kernTables = [sub]
        font["kern"] = kern
        for tag in ("GPOS", "GSUB", "GDEF"):
            if tag in font:
                del font[tag]
        path = out_dir / f"Inter-{style}.subset.ttf"
        font.save(path)
        print(path.name, path.stat().st_size, "bytes,", len(pairs), "kern pairs,", len(font.getGlyphOrder()), "glyphs")


if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(sys.argv[2]))
