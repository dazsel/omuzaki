//! The sentence chunker (`ai-performer.md` §5.2): speakable text in, sentence chunks out, each sent to the voice at
//! once so synthesis of the next overlaps playback of this one.
//!
//! A chunk ends at a sentence end (`. ! ? …` followed by whitespace, with any closing quotes kept; `。！？` at once),
//! never after an abbreviation (`Mr. Dr. St. e.g. i.e. vs. etc.`) or inside a number (`3.5`). The **first** chunk may
//! also end at `, ; : —` once it has [`FIRST_CHUNK_MIN_WORDS`] words, to start audio sooner. Any chunk is forced at
//! [`CHUNK_MAX_CHARS`] on its last space. A sentence end at the very end of the text so far waits for the next
//! character (it may be `3.` of `3.5`), or for [`Chunker::finish`].

use omuzaki_performer_api::limits::{CHUNK_MAX_CHARS, FIRST_CHUNK_MIN_WORDS};

/// Words a sentence never ends after, lower case, without their final dot.
const ABBREVIATIONS: [&str; 8] = ["mr", "mrs", "ms", "dr", "st", "e.g", "i.e", "vs"];
/// `etc.` ends a sentence only when the next word starts with a capital; mid-sentence it doesn't.
const ETC: &str = "etc";

/// One chunk: its text (trimmed) and where it starts in the turn's spoken text, in characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Offset of its first character in the turn's spoken text.
    pub start: u32,
    /// The text, trimmed.
    pub text: String,
}

impl Chunk {
    /// Its length in characters.
    pub fn len(&self) -> u32 {
        u32::try_from(self.text.chars().count()).unwrap_or(u32::MAX)
    }

    /// Whether it is empty (never, for a chunk the chunker emits).
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Cuts streamed text into chunks. One per turn.
#[derive(Debug, Clone, Default)]
pub struct Chunker {
    /// Text not yet in a chunk.
    pending: Vec<char>,
    /// Offset of `pending[0]` in the turn's text.
    start: u32,
    /// Chunks emitted so far.
    emitted: u32,
}

impl Chunker {
    /// A chunker at the start of a turn.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds text; returns the chunks it completed.
    pub fn push(&mut self, text: &str) -> Vec<Chunk> {
        self.pending.extend(text.chars());
        let mut out = Vec::new();
        while let Some(end) = self.cut(false) {
            self.emit(end, &mut out);
        }
        out
    }

    /// Ends the turn: what is left becomes the last chunk.
    pub fn finish(&mut self) -> Vec<Chunk> {
        let mut out = Vec::new();
        while let Some(end) = self.cut(true) {
            self.emit(end, &mut out);
        }
        if !self.pending.is_empty() {
            let end = self.pending.len();
            self.emit(end, &mut out);
        }
        out
    }

    /// The end (exclusive) of the next chunk in `pending`, if one is complete.
    fn cut(&self, at_end: bool) -> Option<usize> {
        let p = &self.pending;
        let first = self.emitted == 0;
        let mut words = 0;
        let mut in_word = false;
        for i in 0..p.len() {
            let c = p[i];
            if c.is_whitespace() {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
            if i + 1 > CHUNK_MAX_CHARS {
                break;
            }
            if matches!(c, '。' | '！' | '？') {
                return Some(after_closers(p, i + 1));
            }
            let end_mark = matches!(c, '.' | '!' | '?' | '…');
            let soft_mark = first && matches!(c, ',' | ';' | ':' | '—') && words >= FIRST_CHUNK_MIN_WORDS;
            if !(end_mark || soft_mark) {
                continue;
            }
            let after = after_closers(p, i + 1);
            match p.get(after) {
                None if !at_end => return None,
                None => return Some(after),
                Some(next) if next.is_whitespace() => {
                    if c == '.' && !sentence_dot(p, i, after) {
                        continue;
                    }
                    return Some(after);
                }
                Some(_) => {}
            }
        }
        if p.len() > CHUNK_MAX_CHARS {
            // Forced: the last space inside the limit, or the limit itself.
            let space = p[..=CHUNK_MAX_CHARS].iter().rposition(|c| c.is_whitespace()).filter(|s| *s > 0);
            return Some(space.unwrap_or(CHUNK_MAX_CHARS));
        }
        None
    }

    fn emit(&mut self, end: usize, out: &mut Vec<Chunk>) {
        let piece: Vec<char> = self.pending.drain(..end).collect();
        let lead = piece.iter().take_while(|c| c.is_whitespace()).count();
        let text: String = piece.iter().collect::<String>().trim().to_owned();
        if !text.is_empty() {
            out.push(Chunk { start: self.start + u32::try_from(lead).unwrap_or(0), text });
            self.emitted += 1;
        }
        self.start += u32::try_from(end).unwrap_or(0);
    }
}

/// The index after any closing quotes or brackets that follow `i`.
fn after_closers(p: &[char], mut i: usize) -> usize {
    while p.get(i).is_some_and(|c| matches!(c, '"' | '\'' | '”' | '’' | ')' | '」' | '』' | '»')) {
        i += 1;
    }
    i
}

/// Whether the `.` at `dot` ends a sentence: not after an abbreviation, and `etc.` only before a capital.
fn sentence_dot(p: &[char], dot: usize, after: usize) -> bool {
    let word_start = p[..dot].iter().rposition(|c| c.is_whitespace()).map_or(0, |s| s + 1);
    let word: String = p[word_start..dot].iter().collect::<String>().to_lowercase();
    let word = word.trim_start_matches(|c: char| !c.is_alphanumeric());
    if ABBREVIATIONS.contains(&word) {
        return false;
    }
    if word == ETC {
        let next = p[after..].iter().find(|c| !c.is_whitespace());
        return next.is_some_and(|c| c.is_uppercase());
    }
    true
}

/// Chunks a whole text at once.
pub fn chunk_all(text: &str) -> Vec<Chunk> {
    let mut chunker = Chunker::new();
    let mut out = chunker.push(text);
    out.extend(chunker.finish());
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn texts(chunks: &[Chunk]) -> Vec<&str> {
        chunks.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn sentences_split_and_keep_their_offsets() {
        let c = chunk_all("Hi chat! Fold it and see!");
        assert_eq!(texts(&c), ["Hi chat!", "Fold it and see!"]);
        assert_eq!(c[1].start, 9);
        let c = chunk_all("He said \"stop.\" Then left… OK? Yes");
        assert_eq!(texts(&c), ["He said \"stop.\"", "Then left…", "OK?", "Yes"]);
    }

    #[test]
    fn abbreviations_and_numbers_do_not_split() {
        let c = chunk_all("Mr. Fox paid 3.5 coins, e.g. for tea etc. and Dr. Owl said hi. Then etc. Fine.");
        assert_eq!(texts(&c), ["Mr. Fox paid 3.5 coins, e.g. for tea etc. and Dr. Owl said hi.", "Then etc.", "Fine."]);
    }

    #[test]
    fn cjk_ends_at_once() {
        let c = chunk_all("こんにちは。元気？「はい！」うん");
        assert_eq!(texts(&c), ["こんにちは。", "元気？", "「はい！」", "うん"]);
    }

    #[test]
    fn the_first_chunk_may_end_at_a_comma_after_eight_words() {
        let c = chunk_all("one two three four five six seven, eight nine ten eleven, twelve. Done, done.");
        assert_eq!(texts(&c), ["one two three four five six seven, eight nine ten eleven,", "twelve.", "Done, done."]);
    }

    #[test]
    fn long_runs_are_forced_at_a_space() {
        let words = "word ".repeat(100);
        let c = chunk_all(&words);
        assert!(c.iter().all(|c| c.len() as usize <= CHUNK_MAX_CHARS));
        let no_space = "x".repeat(500);
        let c = chunk_all(&no_space);
        assert_eq!(c.iter().map(Chunk::len).collect::<Vec<_>>(), [220, 220, 60]);
    }

    #[test]
    fn a_dot_at_the_end_waits_for_the_next_character() {
        let mut chunker = Chunker::new();
        assert_eq!(chunker.push("It costs 3."), []);
        assert_eq!(chunker.push("5 coins"), []);
        assert_eq!(texts(&chunker.push(". Then")), ["It costs 3.5 coins."]);
        assert_eq!(texts(&chunker.finish()), ["Then"]);
    }

    proptest! {
        /// No text is lost or duplicated, every chunk fits, and offsets point at the chunk's text, however the text
        /// arrives.
        #[test]
        fn nothing_is_lost_and_every_chunk_fits(
            text in "[a-zA-Z .!?,;:3]{0,600}",
            cuts in proptest::collection::vec(0usize..700, 0..10),
        ) {
            let chars: Vec<char> = text.chars().collect();
            let mut points: Vec<usize> = cuts.iter().map(|c| c % (chars.len() + 1)).collect();
            points.sort_unstable();
            let mut chunker = Chunker::new();
            let mut chunks = Vec::new();
            let mut last = 0;
            for p in points.into_iter().chain([chars.len()]) {
                chunks.extend(chunker.push(&chars[last..p].iter().collect::<String>()));
                last = p;
            }
            chunks.extend(chunker.finish());
            let joined: String = chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join("");
            let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            prop_assert_eq!(joined.chars().filter(|c| !c.is_whitespace()).collect::<String>(), squeezed);
            for c in &chunks {
                prop_assert!(c.len() as usize <= CHUNK_MAX_CHARS);
                prop_assert!(!c.text.is_empty());
                let at: String = chars[c.start as usize..].iter().take(c.text.chars().count()).collect();
                prop_assert_eq!(&at, &c.text);
            }
        }
    }
}
