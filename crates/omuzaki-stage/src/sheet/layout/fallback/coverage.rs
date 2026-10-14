//! Which characters each pinned fallback font draws, read from `assets/fonts/coverage.bin` (made from the fonts'
//! cmaps by `make_coverage.py`). It lets [`super::Fallback`] choose the one font that covers a character before any
//! download, instead of loading every CJK candidate in turn to ask (about 23 MB for the four of them).

use std::sync::OnceLock;

use super::catalog::Family;

static TABLE: &[u8] = include_bytes!("../../../../assets/fonts/coverage.bin");

/// The order the table lists the families in (the order of `FAMILIES` in `make_coverage.py`).
const ORDER: [Family; 8] =
    [Family::Jp, Family::Sc, Family::Tc, Family::Kr, Family::Arabic, Family::Devanagari, Family::Thai, Family::Emoji];

/// Inclusive code point ranges, sorted, per family in [`ORDER`].
fn ranges() -> &'static [Vec<(u32, u32)>] {
    static DECODED: OnceLock<Vec<Vec<(u32, u32)>>> = OnceLock::new();
    DECODED.get_or_init(|| decode(TABLE))
}

/// Reads one LEB128 number, advancing `at`. A truncated table reads as zeros, which leaves a family with no coverage:
/// the layout then reports its characters missing instead of panicking (the table is committed and tested).
fn varint(bytes: &[u8], at: &mut usize) -> u32 {
    let (mut value, mut shift) = (0_u32, 0);
    while let Some(&byte) = bytes.get(*at) {
        *at += 1;
        value |= u32::from(byte & 0x7F).checked_shl(shift).unwrap_or(0);
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    value
}

fn decode(bytes: &[u8]) -> Vec<Vec<(u32, u32)>> {
    let mut at = 0;
    ORDER
        .iter()
        .map(|_| {
            let count = varint(bytes, &mut at);
            let mut end = 0_u32;
            (0..count)
                .map(|_| {
                    let lo = end + varint(bytes, &mut at);
                    let hi = lo + varint(bytes, &mut at);
                    end = hi + 1;
                    (lo, hi)
                })
                .collect()
        })
        .collect()
}

/// Whether the pinned font of `family` has a glyph for `c`.
pub(super) fn covers(family: Family, c: char) -> bool {
    let Some(index) = ORDER.iter().position(|f| *f == family) else { return false };
    let u = u32::from(c);
    let table = &ranges()[index];
    let after = table.partition_point(|&(lo, _)| lo <= u);
    after > 0 && table[after - 1].1 >= u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_decodes_to_every_family_and_stays_small() {
        assert!(TABLE.len() < 64 * 1024, "{} bytes", TABLE.len());
        assert!(ranges().iter().all(|r| !r.is_empty()));
        assert!(ranges().iter().all(|r| r.windows(2).all(|w| w[0].1 < w[1].0)), "sorted and disjoint");
    }

    #[test]
    fn shared_han_is_in_every_cjk_font_but_forms_differ_by_region() {
        for family in [Family::Jp, Family::Sc, Family::Tc, Family::Kr] {
            assert!(covers(family, '明'), "{family:?}");
        }
        // 们 is a simplified form: Chinese-Simplified has it, the Japanese JIS set does not.
        assert!(covers(Family::Sc, '们') && !covers(Family::Jp, '们'));
        assert!(covers(Family::Jp, 'さ') && covers(Family::Kr, '늘') && !covers(Family::Thai, '늘'));
        assert!(covers(Family::Arabic, 'م') && covers(Family::Devanagari, 'र') && covers(Family::Emoji, '😀'));
    }
}
