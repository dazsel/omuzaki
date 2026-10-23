//! The search index's text (design D13): lowercased, accent-folded (NFKD for non-CJK) words, character bigrams for
//! CJK, and the ranking: exact id or title, then tag, then word prefix, then fuzzy (one edit, words of five letters or
//! more). Each item's words are folded once, when it is indexed ([`Indexed::of`]); a query folds only itself.

use omuzaki_library_api::{ItemId, ItemManifest};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// A fuzzy match needs words at least this long.
const FUZZY_MIN_LEN: usize = 5;

/// Lowercases, and for non-CJK text decomposes (NFKD) and drops combining marks, so `Éclat` and `eclat` are one word.
/// CJK characters stay composed (NFKC): decomposing would split a Hangul syllable into Jamo and a voiced kana into a
/// base and a mark.
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

/// Kana, Han (with extension A) and Korean, which are written without spaces between words.
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

/// The searchable words of folded text; a run of CJK characters becomes its bigrams (one character stands for itself).
pub(crate) fn tokens(folded: &str) -> Vec<String> {
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
    // A char is one to four bytes, so byte lengths further apart than four per char can't be one edit apart; most
    // words are rejected here without collecting.
    if a.len().abs_diff(b.len()) > 4 {
        return false;
    }
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

/// One item's text, folded once when the item is indexed.
#[derive(Debug, Clone)]
pub(crate) struct Indexed {
    id: String,
    titles: Vec<String>,
    tag_words: Vec<String>,
    /// Every word: tags, titles, summary and id.
    words: Vec<String>,
}

impl Indexed {
    pub(crate) fn of(id: &ItemId, m: &ItemManifest) -> Self {
        let titles: Vec<String> = std::iter::once(&m.title).chain(m.titles.values()).map(|t| fold(t)).collect();
        let tag_words: Vec<String> =
            m.tags.iter().chain(m.tags_i18n.values().flatten()).flat_map(|t| tokens(&fold(t))).collect();
        let id = fold(id.as_str());
        let mut words = tag_words.clone();
        for field in titles.iter().map(String::as_str).chain([fold(&m.summary).as_str(), id.as_str()]) {
            words.extend(tokens(field));
        }
        words.sort_unstable();
        words.dedup();
        Self { id, titles, tag_words, words }
    }

    /// How well the folded query matches, higher is better, `None` for no match: 5 exact id or title (any locale), 4
    /// every query word is a tag word, 3 every query word starts a word, 2 as 3 allowing one edit in long words.
    pub(crate) fn rank(&self, query: &Query) -> Option<u8> {
        if query.wanted.is_empty() {
            return None;
        }
        if self.id == query.folded || self.titles.contains(&query.folded) {
            return Some(5);
        }
        if query.wanted.iter().all(|w| self.tag_words.contains(w)) {
            return Some(4);
        }
        // `words` is sorted, so the words a prefix starts are one contiguous run.
        let by_prefix = |w: &String| {
            let start = self.words.partition_point(|i| i.as_str() < w.as_str());
            self.words.get(start).is_some_and(|i| i.starts_with(w.as_str()))
        };
        if query.wanted.iter().all(by_prefix) {
            return Some(3);
        }
        let fuzzy = |w: &String| {
            by_prefix(w) || (w.chars().count() >= FUZZY_MIN_LEN && self.words.iter().any(|i| within_one_edit(i, w)))
        };
        query.wanted.iter().all(fuzzy).then_some(2)
    }
}

/// A query's text, folded once.
pub(crate) struct Query {
    folded: String,
    wanted: Vec<String>,
}

impl Query {
    pub(crate) fn new(text: &str) -> Self {
        let folded = fold(text).trim().to_owned();
        let wanted = tokens(&folded);
        Self { folded, wanted }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_drops_accents_and_case_but_keeps_cjk_composed() {
        assert_eq!(fold("Éclat DORÉ"), "eclat dore");
        assert_eq!(fold("푸른머리"), "푸른머리");
        assert_eq!(tokens(&fold("提灯屋")), ["提灯", "灯屋"]);
    }

    #[test]
    fn one_edit_is_a_substitution_insertion_or_deletion() {
        assert!(within_one_edit("garnet", "garnat"));
        assert!(within_one_edit("garnet", "garnets"));
        assert!(!within_one_edit("garnet", "garnxx"));
    }
}
