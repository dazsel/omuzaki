//! The case-insensitive form of a text (design §6): NFKC, then lowercase. Every case-insensitive match in the nodes
//! uses it, so `where` filters, chat commands and keyword matches agree on what "the same text" means.

use unicode_normalization::UnicodeNormalization;

/// `text` folded: compatibility-normalised (full-width letters become ASCII, ligatures split), then lowercased.
#[must_use]
pub fn fold(text: &str) -> String {
    text.nfkc().collect::<String>().to_lowercase()
}

/// Whether two texts are the same once folded.
#[must_use]
pub fn same(a: &str, b: &str) -> bool {
    a == b || fold(a) == fold(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_width_and_ligatures_fold_away() {
        assert!(same("Bonk", "bONK"));
        assert!(same("ＢＯＮＫ", "bonk"));
        assert!(same("ﬁre", "FIRE"));
        assert!(!same("bonk", "bonks"));
    }
}
