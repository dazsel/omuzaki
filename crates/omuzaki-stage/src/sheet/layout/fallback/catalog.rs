//! The pinned list of fallback fonts: where each is fetched from and the SHA-256 it must have.
//!
//! Every file is Noto, SIL OFL 1.1 (`docs/licensing/register.md`, `assets/fonts/NOTICES.md`). None is committed:
//! they are fetched on first need through [`FontSpec::pin`] and the shared verified cache, because the CJK ones alone are
//! 4-8 MB each and the repo takes nothing over about 2 MB. URLs name an exact commit, so a file never changes under its
//! hash.

use omuzaki_ai_api::{PinnedFile, Sha256};

use crate::sheet::layout::text::Weight;

/// A font family the fallback chain can draw with.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    /// Noto Sans JP: kana and Japanese-form Han.
    Jp,
    /// Noto Sans SC: Simplified Chinese Han.
    Sc,
    /// Noto Sans TC: Traditional Chinese Han.
    Tc,
    /// Noto Sans KR: hangul and Korean-form Han.
    Kr,
    /// Noto Sans Arabic.
    Arabic,
    /// Noto Sans Devanagari: Hindi, Marathi, Nepali.
    Devanagari,
    /// Noto Sans Thai.
    Thai,
    /// Noto Emoji: monochrome outlines, so emoji take the sheet's ink colour.
    Emoji,
}

/// One downloadable font file.
#[derive(Debug, PartialEq, Eq)]
pub struct FontSpec {
    /// The family it belongs to.
    pub family: Family,
    /// The cut it stands in for.
    pub weight: Weight,
    /// The file name, also what a [`super::DirSource`] looks for.
    pub file_name: &'static str,
    /// Where it is fetched from, in order (immutable commit URLs).
    pub urls: &'static [&'static str],
    /// The SHA-256 the bytes must have.
    pub sha256: Sha256,
    /// The exact size in bytes; a download of any other length is refused before it is hashed.
    pub size: u64,
}

impl FontSpec {
    /// The file as the shared verified cache pins it: the same URLs, hash and size.
    pub fn pin(&self) -> PinnedFile {
        PinnedFile::new(self.urls.iter().copied(), self.sha256, self.size)
    }
}

/// Parses a catalogue hash at compile time: a malformed pin fails the build, not a download.
const fn pinned_hash(hex: &str) -> Sha256 {
    match Sha256::from_hex(hex) {
        Some(hash) => hash,
        None => panic!("a catalogue pin must be 64 lower-case hex characters"),
    }
}

macro_rules! spec {
    ($family:ident, $weight:ident, $file:literal, $base:literal, $url_file:literal, $size:literal, $sha:literal) => {
        FontSpec {
            family: Family::$family,
            weight: Weight::$weight,
            file_name: $file,
            urls: &[concat!($base, "/", $url_file)],
            sha256: pinned_hash($sha),
            size: $size,
        }
    };
}

/// Every fallback font, in no particular order. Text in the `SemiBold` role (names, headings) is drawn in the Medium cut of every
/// script, so a sheet of mixed names reads as one typeface; the CJK families have no `SemiBold`.
pub static CATALOG: &[FontSpec] = &[
    spec!(
        Jp,
        Regular,
        "NotoSansJP-Regular.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF",
        "JP/NotoSansJP-Regular.otf",
        4_533_028,
        "dff723ba59d57d136764a04b9b2d03205544f7cd785a711442d6d2d085ac5073"
    ),
    spec!(
        Jp,
        SemiBold,
        "NotoSansJP-Medium.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF",
        "JP/NotoSansJP-Medium.otf",
        4_549_132,
        "f396a3b57256e4515be9cb41f7aac54766d654890082a9f1b5c2451b5c093d8a"
    ),
    spec!(
        Sc,
        Regular,
        "NotoSansSC-Regular.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF",
        "SC/NotoSansSC-Regular.otf",
        8_331_336,
        "faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9"
    ),
    spec!(
        Sc,
        SemiBold,
        "NotoSansSC-Medium.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF",
        "SC/NotoSansSC-Medium.otf",
        8_345_436,
        "7633f5a016d4dd95e685a69633d818aabc4644c4b08e26bd35b1b30c45ed5dda"
    ),
    spec!(
        Tc,
        Regular,
        "NotoSansTC-Regular.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF",
        "TC/NotoSansTC-Regular.otf",
        5_683_368,
        "5bab0cb3c1cf89dde07c4a95a4054b195afbcfe784d69d75c340780712237537"
    ),
    spec!(
        Tc,
        SemiBold,
        "NotoSansTC-Medium.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF",
        "TC/NotoSansTC-Medium.otf",
        5_695_744,
        "bf206dca0975779bac71cb49a037a364156ca98a0c431b1b7d6b29fb8952ac7e"
    ),
    spec!(
        Kr,
        Regular,
        "NotoSansKR-Regular.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF",
        "KR/NotoSansKR-Regular.otf",
        4_644_748,
        "69975a0ac8472717870aefeab0a4d52739308d90856b9955313b2ad5e0148d68"
    ),
    spec!(
        Kr,
        SemiBold,
        "NotoSansKR-Medium.otf",
        "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/SubsetOTF",
        "KR/NotoSansKR-Medium.otf",
        4_669_816,
        "b46988ef13e8bac08f3933af686eaf770972994f9b6d335be0184d60169b5431"
    ),
    spec!(
        Arabic,
        Regular,
        "NotoSansArabic-Regular.ttf",
        "https://raw.githubusercontent.com/notofonts/arabic/43674fa5a3ad7e1a8e1b9249319b51b1ee68be26/fonts/NotoSansArabic/hinted/ttf",
        "NotoSansArabic-Regular.ttf",
        234_116,
        "acc087b31aabed3c2d821bc981416dea362407a81f235c9a4966463cc7ef7bbe"
    ),
    spec!(
        Arabic,
        SemiBold,
        "NotoSansArabic-Medium.ttf",
        "https://raw.githubusercontent.com/notofonts/arabic/43674fa5a3ad7e1a8e1b9249319b51b1ee68be26/fonts/NotoSansArabic/hinted/ttf",
        "NotoSansArabic-Medium.ttf",
        255_716,
        "578a8e23cb7bcb34eaf3b45e5001ee13f845ecf27f8a884b4f04c08713004d39"
    ),
    spec!(
        Devanagari,
        Regular,
        "NotoSansDevanagari-Regular.ttf",
        "https://raw.githubusercontent.com/notofonts/devanagari/46287b3d75984b89a46b66c77b370da4ff5909a6/fonts/NotoSansDevanagari/hinted/ttf",
        "NotoSansDevanagari-Regular.ttf",
        244_104,
        "87e37c8bc43fac042a8afe9a1dde8ef7c05baa9c6ab8eb20e5758893d52b928e"
    ),
    spec!(
        Devanagari,
        SemiBold,
        "NotoSansDevanagari-Medium.ttf",
        "https://raw.githubusercontent.com/notofonts/devanagari/46287b3d75984b89a46b66c77b370da4ff5909a6/fonts/NotoSansDevanagari/hinted/ttf",
        "NotoSansDevanagari-Medium.ttf",
        244_324,
        "300a84b3e22fa5abc3db1bfb47c3b85ce8b06a892bcd1c22f105151255c3ade8"
    ),
    spec!(
        Thai,
        Regular,
        "NotoSansThai-Regular.ttf",
        "https://raw.githubusercontent.com/notofonts/thai/e149ed24fedacabd818ecf537932da4de8360398/fonts/NotoSansThai/hinted/ttf",
        "NotoSansThai-Regular.ttf",
        37_672,
        "06d73c03228f7678cb5a42dfad5f42b091ac58309040622e7652a3628957fbce"
    ),
    spec!(
        Thai,
        SemiBold,
        "NotoSansThai-Medium.ttf",
        "https://raw.githubusercontent.com/notofonts/thai/e149ed24fedacabd818ecf537932da4de8360398/fonts/NotoSansThai/hinted/ttf",
        "NotoSansThai-Medium.ttf",
        38_188,
        "7aa643eac3ddf23fd7b6ffbc2b4a9b85050ba2bd1ede6d410bcee74fa44e00b1"
    ),
    // A variable font whose default instance (wght 400) is drawn for every weight.
    spec!(
        Emoji,
        Regular,
        "NotoEmoji-Regular.ttf",
        "https://raw.githubusercontent.com/google/fonts/bd8f81ddb5c74d5c8897b36ad88b440266245103/ofl/notoemoji",
        "NotoEmoji%5Bwght%5D.ttf",
        1_982_596,
        "de6c18832938afc99caf132b39d6a30a19bac7f2e812e28db2535b4608d27551"
    ),
];

/// The index in [`CATALOG`] of the file that draws `family` at `weight`: the exact cut, else the family's first.
pub(super) fn index_of(family: Family, weight: Weight) -> Option<usize> {
    let exact = CATALOG.iter().position(|s| s.family == family && s.weight == weight);
    exact.or_else(|| CATALOG.iter().position(|s| s.family == family))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_has_a_regular_cut_and_every_pin_is_well_formed() {
        for family in [
            Family::Jp,
            Family::Sc,
            Family::Tc,
            Family::Kr,
            Family::Arabic,
            Family::Devanagari,
            Family::Thai,
            Family::Emoji,
        ] {
            assert!(CATALOG.iter().any(|s| s.family == family && s.weight == Weight::Regular), "{family:?}");
        }
        for s in CATALOG {
            let pin = s.pin();
            assert!(!pin.urls.is_empty() && s.size > 0, "{}", s.file_name);
            assert!(pin.urls.iter().all(|url| url.starts_with("https://")), "{}", s.file_name);
            assert_eq!((pin.sha256, pin.size), (s.sha256, s.size), "{}", s.file_name);
        }
    }

    #[test]
    fn urls_are_pinned_to_a_commit_not_a_branch() {
        for s in CATALOG {
            for url in s.urls {
                assert!(!url.contains("/main/") && !url.contains("/master/"), "{} floats", s.file_name);
            }
        }
    }

    #[test]
    fn a_missing_cut_falls_back_to_the_familys_regular() {
        let regular = index_of(Family::Emoji, Weight::Regular);
        assert!(regular.is_some() && regular == index_of(Family::Emoji, Weight::SemiBold));
        assert_ne!(index_of(Family::Jp, Weight::Regular), index_of(Family::Jp, Weight::SemiBold));
    }
}
