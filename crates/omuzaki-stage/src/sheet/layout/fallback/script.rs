//! Which script a character belongs to, and so which fallback font families can draw it.
//!
//! This is deliberately a short table of the blocks the launch languages and emoji need (spec v3 §10), not a
//! full Unicode script database: a character outside it is reported as missing so a gap shows up instead of a box.

use super::catalog::Family;

/// Which regional form of the shared Han ideographs to draw. The same code point has a different glyph in Japanese,
/// Chinese and Korean, so the sheet's language picks the font, and the choice is recorded with the sheet.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum HanVariant {
    /// Japanese forms (Noto Sans JP).
    #[default]
    Japanese,
    /// Simplified Chinese forms (Noto Sans SC).
    SimplifiedChinese,
    /// Traditional Chinese forms (Noto Sans TC).
    TraditionalChinese,
    /// Korean forms (Noto Sans KR).
    Korean,
}

impl HanVariant {
    /// The variant for a BCP 47 language tag (`ja`, `zh-Hant-TW`, `ko-KR`), or `None` for a language that does not
    /// use Han (the caller keeps its default).
    pub fn from_language_tag(tag: &str) -> Option<Self> {
        let lower = tag.to_ascii_lowercase();
        let mut parts = lower.split(['-', '_']);
        match parts.next()? {
            "ja" => Some(Self::Japanese),
            "ko" => Some(Self::Korean),
            "zh" => {
                let traditional = parts.any(|p| matches!(p, "hant" | "tw" | "hk" | "mo"));
                Some(if traditional { Self::TraditionalChinese } else { Self::SimplifiedChinese })
            }
            _ => None,
        }
    }

    /// The family drawing this variant.
    pub(super) const fn family(self) -> Family {
        match self {
            Self::Japanese => Family::Jp,
            Self::SimplifiedChinese => Family::Sc,
            Self::TraditionalChinese => Family::Tc,
            Self::Korean => Family::Kr,
        }
    }
}

/// What kind of character this is, for choosing a face.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Class {
    /// Space, punctuation and joiners: they take the face of the text beside them.
    Neutral,
    /// The families that may draw it, best first.
    Script(Vec<Family>),
    /// A script the table does not know.
    Unknown,
}

/// Classifies `c`; `han` orders the Han fallbacks.
pub(super) fn classify(c: char, han: HanVariant) -> Class {
    let u = u32::from(c);
    let in_any = |ranges: &[(u32, u32)]| ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&u));
    if is_neutral(c) {
        return Class::Neutral;
    }
    if in_any(&[(0x3040, 0x30FF), (0x31F0, 0x31FF), (0xFF66, 0xFF9F)]) {
        return Class::Script(vec![Family::Jp, Family::Sc, Family::Tc, Family::Kr]);
    }
    if in_any(&[(0x1100, 0x11FF), (0x3130, 0x318F), (0xA960, 0xA97F), (0xAC00, 0xD7FF)]) {
        return Class::Script(vec![Family::Kr]);
    }
    if in_any(&[
        (0x2E80, 0x2FDF),
        (0x3000, 0x303F),
        (0x3200, 0x33FF),
        (0x3400, 0x4DBF),
        (0x4E00, 0x9FFF),
        (0xF900, 0xFAFF),
        (0xFE30, 0xFE4F),
        (0xFF00, 0xFFEF),
        (0x20000, 0x323AF),
    ]) {
        let mut chain = vec![han.family()];
        chain.extend([Family::Jp, Family::Sc, Family::Tc, Family::Kr].into_iter().filter(|f| *f != han.family()));
        return Class::Script(chain);
    }
    if in_any(&[(0x0600, 0x06FF), (0x0750, 0x077F), (0x0870, 0x08FF), (0xFB50, 0xFDFF), (0xFE70, 0xFEFF)]) {
        return Class::Script(vec![Family::Arabic]);
    }
    if in_any(&[(0x0900, 0x097F), (0xA8E0, 0xA8FF)]) {
        return Class::Script(vec![Family::Devanagari]);
    }
    if in_any(&[(0x0E00, 0x0E7F)]) {
        return Class::Script(vec![Family::Thai]);
    }
    if in_any(&[(0x2190, 0x21FF), (0x2300, 0x23FF), (0x2600, 0x27BF), (0x2B00, 0x2BFF), (0x1F000, 0x1FAFF)]) {
        return Class::Script(vec![Family::Emoji]);
    }
    Class::Unknown
}

/// Characters that belong to no script of their own. Digits are not among them: they stay in the Latin face so
/// numbers look the same in every language.
pub(in crate::sheet::layout) fn is_neutral(c: char) -> bool {
    c.is_whitespace() || c.is_ascii_punctuation() || is_default_ignorable(c)
}

/// Joiners and variation selectors: they shape their neighbours but draw nothing themselves.
pub(in crate::sheet::layout) fn is_default_ignorable(c: char) -> bool {
    matches!(u32::from(c), 0x200B..=0x200F | 0x2060 | 0xFE00..=0xFE0F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_tags_choose_the_han_forms() {
        use HanVariant::{Japanese, Korean, SimplifiedChinese, TraditionalChinese};
        assert_eq!(HanVariant::from_language_tag("ja"), Some(Japanese));
        assert_eq!(HanVariant::from_language_tag("ko-KR"), Some(Korean));
        assert_eq!(HanVariant::from_language_tag("zh"), Some(SimplifiedChinese));
        assert_eq!(HanVariant::from_language_tag("zh-Hans-CN"), Some(SimplifiedChinese));
        assert_eq!(HanVariant::from_language_tag("zh_Hant"), Some(TraditionalChinese));
        assert_eq!(HanVariant::from_language_tag("zh-TW"), Some(TraditionalChinese));
        assert_eq!(HanVariant::from_language_tag("pt-BR"), None);
    }

    #[test]
    fn han_prefers_the_sheets_language_and_kana_and_hangul_do_not() {
        let Class::Script(chain) = classify('漢', HanVariant::Korean) else { panic!("Han is a script") };
        assert_eq!(chain[0], Family::Kr);
        assert!(chain.contains(&Family::Jp));
        assert_eq!(
            classify('あ', HanVariant::Korean),
            Class::Script(vec![Family::Jp, Family::Sc, Family::Tc, Family::Kr])
        );
        assert_eq!(classify('한', HanVariant::Japanese), Class::Script(vec![Family::Kr]));
    }

    #[test]
    fn the_other_launch_scripts_and_emoji_are_known() {
        assert_eq!(classify('م', HanVariant::default()), Class::Script(vec![Family::Arabic]));
        assert_eq!(classify('ह', HanVariant::default()), Class::Script(vec![Family::Devanagari]));
        assert_eq!(classify('ก', HanVariant::default()), Class::Script(vec![Family::Thai]));
        assert_eq!(classify('😀', HanVariant::default()), Class::Script(vec![Family::Emoji]));
        assert_eq!(classify('ж', HanVariant::default()), Class::Unknown);
    }

    #[test]
    fn spaces_and_joiners_are_neutral_but_digits_are_not() {
        assert_eq!(classify(' ', HanVariant::default()), Class::Neutral);
        assert_eq!(classify('\u{200D}', HanVariant::default()), Class::Neutral);
        assert_eq!(classify('7', HanVariant::default()), Class::Unknown);
    }
}
