//! Free-text matching for search (design D13): lowercased, accent-folded (NFKD for non-CJK), split into words, with character
//! bigrams for CJK, ranked exact id or title, then tag, then word prefix, then fuzzy. Naive on purpose: every query
//! re-reads the manifest, which is what makes it an oracle for an index.

use omuzaki_library_api::{ItemId, ItemManifest};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// A fuzzy match needs words at least this long (design D13: edit distance 1 for words of five letters or more).
const FUZZY_MIN_LEN: usize = 5;

/// Lowercases, and for non-CJK text decomposes (NFKD) and drops combining marks, so `Éclat` and `eclat` are one word.
/// CJK characters stay composed (NFKC): decomposing would split a Hangul syllable into Jamo and a voiced kana (`が`)
/// into a base and a mark, and the mark would then be dropped or split the word.
pub(crate) fn fold(s: &str) -> String {
    s.nfkc()
        .flat_map(|c| {
            let parts: Vec<char> = if is_cjk(c) { vec![c] } else { c.to_string().nfkd().collect() };
            parts
        })
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Kana, Han (with extension A) and Korean: Hangul syllables, Jamo and compatibility Jamo, all of which are written
/// without spaces between words and so are indexed as bigrams.
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{1100}'..='\u{11FF}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3130}'..='\u{318F}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{A960}'..='\u{A97F}'
            | '\u{AC00}'..='\u{D7FF}'
    )
}

/// The searchable words of already-folded text. A run of CJK characters has no spaces to split on, so it is indexed as
/// its character bigrams (a single character stands for itself), and a query matches without a dictionary.
fn tokens(folded: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in folded.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let chars: Vec<char> = word.chars().collect();
        if chars.iter().any(|c| is_cjk(*c)) {
            if chars.len() == 1 {
                out.push(word.to_owned());
            }
            out.extend(chars.windows(2).map(|pair| pair.iter().collect::<String>()));
        } else {
            out.push(word.to_owned());
        }
    }
    out
}

/// Whether `a` and `b` differ by at most one insertion, deletion or substitution.
fn within_one_edit(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (rest_a, rest_b) = (&a[common..], &b[common..]);
    match rest_a.len().cmp(&rest_b.len()) {
        std::cmp::Ordering::Equal => rest_a.get(1..) == rest_b.get(1..),
        std::cmp::Ordering::Greater => rest_a[1..] == *rest_b,
        std::cmp::Ordering::Less => *rest_a == rest_b[1..],
    }
}

/// How well `text` matches an item, higher is better, `None` for no match: 5 exact id or title (any locale), 4 every
/// query word is a tag word (any locale), 3 every query word is the start of a word of the title, tags, summary or
/// id, 2 as 3 but allowing one edit in words of five letters or more.
pub(crate) fn rank(text: &str, id: &ItemId, m: &ItemManifest) -> Option<u8> {
    let query = fold(text);
    let query = query.trim();
    let wanted = tokens(query);
    if wanted.is_empty() {
        return None;
    }
    let titles: Vec<String> = std::iter::once(&m.title).chain(m.titles.values()).map(|t| fold(t)).collect();
    if fold(id.as_str()) == query || titles.iter().any(|t| t == query) {
        return Some(5);
    }
    let tag_words: Vec<String> =
        m.tags.iter().chain(m.tags_i18n.values().flatten()).flat_map(|t| tokens(&fold(t))).collect();
    if wanted.iter().all(|w| tag_words.contains(w)) {
        return Some(4);
    }
    let mut index = tag_words;
    for field in titles.iter().map(String::as_str).chain([fold(&m.summary).as_str(), fold(id.as_str()).as_str()]) {
        index.extend(tokens(field));
    }
    let by_prefix = |w: &String| index.iter().any(|i| i.starts_with(w.as_str()));
    if wanted.iter().all(by_prefix) {
        return Some(3);
    }
    let fuzzy = |w: &String| {
        by_prefix(w) || (w.chars().count() >= FUZZY_MIN_LEN && index.iter().any(|i| within_one_edit(i, w)))
    };
    wanted.iter().all(fuzzy).then_some(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_drops_accents_and_case() {
        assert_eq!(fold("Éclat DORÉ"), "eclat dore");
    }

    #[test]
    fn cjk_stays_composed_while_latin_accents_fold() {
        assert_eq!(fold("푸른머리"), "푸른머리", "Hangul syllables are not decomposed into Jamo");
        assert_eq!(fold("がぎ"), "がぎ", "voiced kana keep their marks");
        assert_eq!(fold("ＥＣＬＡＴ Doré"), "eclat dore");
    }

    #[test]
    fn korean_runs_become_bigrams_like_the_rest_of_cjk() {
        assert_eq!(tokens(&fold("푸른머리")), ["푸른", "른머", "머리"]);
    }

    #[test]
    fn cjk_runs_become_bigrams() {
        assert_eq!(tokens("提灯屋"), ["提灯", "灯屋"]);
        assert_eq!(tokens("灯"), ["灯"]);
        assert_eq!(tokens("a-b c"), ["a", "b", "c"]);
    }

    #[test]
    fn one_edit_is_a_substitution_insertion_or_deletion() {
        assert!(within_one_edit("garnet", "garnat"));
        assert!(within_one_edit("garnet", "garnets"));
        assert!(within_one_edit("garnet", "garne"));
        assert!(!within_one_edit("garnet", "garnxx"));
        assert!(!within_one_edit("garnet", "arnetg"));
    }
}
