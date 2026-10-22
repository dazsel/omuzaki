//! Our own English grapheme-to-phoneme front end for Kokoro-82M (`docs/research/2026-10-gpl-free-tts.md`): text in,
//! the IPA-like phoneme string Kokoro was trained on out. No GPL anywhere: it reads only misaki's *gold* lexicons
//! (Apache-2.0, pinned files of the model), never the silver ones, espeak-ng or the BART fallback.
//!
//! 1. **Normalise** ([`numbers::normalise`]): numbers, money, percentages, times and a few abbreviations become words.
//! 2. **Split** into words and the punctuation Kokoro hears (`, . ! ? ; : — … " ( )`); a hyphen splits a word.
//! 3. **Look up** each word: as written, then in lower case; a heteronym takes its default reading (there is no
//!    part-of-speech tagger yet). Plurals, possessives, `-ed`, `-ing`, `-ly` and `-ness` are built from their stem
//!    with the rules misaki uses (a voiceless stem takes `s` and `t`, a sibilant `ᵻz`, `t` and `d` take `ᵻd`).
//! 4. **Function words** read as they are said in a sentence: `the` is `ðə` before a consonant and `ði` before a vowel,
//!    `a` is `ɐ`.
//! 5. **Unknown words** are spelled out letter by letter, until the letter-to-sound model the research plans exists.
//!    That costs quality, never licence.

pub mod numbers;

use std::collections::HashMap;

use serde::Deserialize;

/// Kokoro's phoneme vowels, and the diphthong capitals misaki uses (`A` = eɪ, `I` = aɪ, `O` = oʊ, `W` = aʊ, `Y` = ɔɪ).
const VOWELS: &str = "AIOQWYaeiouæɑɐɒɔəɚɛɜɪʊʌᵊᵻ";
/// Punctuation Kokoro has tokens for, as written after a word.
const CLOSING: &str = ",.!?;:—…)\u{201D}";
/// Punctuation written before a word.
const OPENING: &str = "(\u{201C}";

/// A gold lexicon entry: one reading, or readings by part of speech with a default.
#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    One(String),
    ByPos(HashMap<String, Option<String>>),
}

impl Entry {
    fn reading(&self) -> Option<&str> {
        match self {
            Self::One(s) => Some(s),
            Self::ByPos(map) => map.get("DEFAULT").and_then(Option::as_deref),
        }
    }
}

/// Why a lexicon can't be read.
#[derive(Debug, thiserror::Error)]
#[error("the lexicon isn't a misaki gold dictionary: {0}")]
pub struct LexiconError(String);

/// One accent's gold lexicon: word to phonemes.
pub struct Lexicon {
    words: HashMap<String, String>,
}

impl std::fmt::Debug for Lexicon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lexicon").field("words", &self.words.len()).finish()
    }
}

/// A piece of a sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Punct(char),
}

impl Lexicon {
    /// Reads a gold lexicon (`us_gold.json`): a JSON object from words to readings, or to readings by part of speech.
    ///
    /// # Errors
    /// [`LexiconError`] when it isn't one.
    pub fn from_json(bytes: &[u8]) -> Result<Self, LexiconError> {
        let raw: HashMap<String, Entry> = serde_json::from_slice(bytes).map_err(|e| LexiconError(e.to_string()))?;
        let words: HashMap<String, String> =
            raw.into_iter().filter_map(|(word, entry)| entry.reading().map(|r| (word, r.to_owned()))).collect();
        if words.len() < 1_000 {
            return Err(LexiconError(format!("only {} readings", words.len())));
        }
        Ok(Self { words })
    }

    /// A lexicon over `pairs`, for tests.
    #[must_use]
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self { words: pairs.into_iter().map(|(w, p)| (w.to_owned(), p.to_owned())).collect() }
    }

    /// How many words it reads.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Whether it reads no word.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// `text` as Kokoro's phonemes: words separated by spaces, punctuation kept where it is heard.
    #[must_use]
    pub fn phonemize(&self, text: &str) -> String {
        let tokens = tokenize(&numbers::normalise(text));
        let readings: Vec<Option<String>> = tokens
            .iter()
            .map(|t| match t {
                Token::Word(w) => Some(self.word(w)),
                Token::Punct(_) => None,
            })
            .collect();
        let mut out = String::new();
        for (i, token) in tokens.iter().enumerate() {
            match token {
                Token::Punct(c) if CLOSING.contains(*c) => {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push(*c);
                    out.push(' ');
                }
                Token::Punct(c) => {
                    out.push(*c);
                }
                Token::Word(word) => {
                    let next = readings[i + 1..].iter().flatten().next().map(String::as_str);
                    let reading = match word.to_lowercase().as_str() {
                        "the" => {
                            if next.is_some_and(starts_with_vowel) {
                                "ði".to_owned()
                            } else {
                                "ðə".to_owned()
                            }
                        }
                        "a" if word != "A" || i + 1 < tokens.len() => "ɐ".to_owned(),
                        _ => readings[i].clone().unwrap_or_default(),
                    };
                    out.push_str(&reading);
                    out.push(' ');
                }
            }
        }
        out.trim_end().to_owned()
    }

    /// One word's reading, from the lexicon, its stem, or its letters.
    fn word(&self, word: &str) -> String {
        if let Some(reading) = self.lookup(word) {
            return reading;
        }
        let lower = word.to_lowercase();
        if let Some(reading) = self.inflected(&lower) {
            return reading;
        }
        self.spell(word)
    }

    fn lookup(&self, word: &str) -> Option<String> {
        if let Some(r) = self.words.get(word) {
            return Some(r.clone());
        }
        let lower = word.to_lowercase();
        if let Some(r) = self.words.get(&lower) {
            return Some(r.clone());
        }
        let mut title = lower.clone();
        if let Some(first) = title.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        self.words.get(&title).cloned()
    }

    /// A word built from a stem the lexicon knows.
    fn inflected(&self, word: &str) -> Option<String> {
        let stem = |s: &str| -> Option<String> { (s.len() >= 2).then(|| self.lookup(s)).flatten() };
        if let Some(base) = word.strip_suffix("'s").or_else(|| word.strip_suffix("s'")) {
            return stem(base).map(|r| plural(&r));
        }
        if let Some(base) = word.strip_suffix("ies") {
            if let Some(r) = stem(&format!("{base}y")) {
                return Some(plural(&r));
            }
        }
        if let Some(base) = word.strip_suffix("es") {
            if let Some(r) = stem(base) {
                return Some(plural(&r));
            }
        }
        if let Some(base) = word.strip_suffix('s')
            && !base.ends_with('s')
            && let Some(r) = stem(base)
        {
            return Some(plural(&r));
        }
        if let Some(base) = word.strip_suffix("ied") {
            if let Some(r) = stem(&format!("{base}y")) {
                return Some(past(&r));
            }
        }
        if let Some(base) = word.strip_suffix("ed") {
            for candidate in [base.to_owned(), format!("{base}e"), undouble(base)] {
                if let Some(r) = stem(&candidate) {
                    return Some(past(&r));
                }
            }
        }
        if let Some(base) = word.strip_suffix("ing") {
            for candidate in [base.to_owned(), format!("{base}e"), undouble(base)] {
                if let Some(r) = stem(&candidate) {
                    return Some(format!("{r}ɪŋ"));
                }
            }
        }
        if let Some(base) = word.strip_suffix("ly") {
            if let Some(r) = stem(base).or_else(|| base.strip_suffix('i').and_then(|b| stem(&format!("{b}y")))) {
                return Some(format!("{r}li"));
            }
        }
        if let Some(base) = word.strip_suffix("ness") {
            if let Some(r) = stem(base).or_else(|| base.strip_suffix('i').and_then(|b| stem(&format!("{b}y")))) {
                return Some(format!("{r}nəs"));
            }
        }
        None
    }

    /// Letter by letter, as the lexicon reads each capital letter.
    fn spell(&self, word: &str) -> String {
        word.chars()
            .filter(char::is_ascii_alphanumeric)
            .filter_map(|c| self.words.get(&c.to_ascii_uppercase().to_string()).cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// `stopped` → `stop`: a doubled final consonant undone.
fn undouble(base: &str) -> String {
    let bytes = base.as_bytes();
    if bytes.len() >= 3 && bytes[bytes.len() - 1] == bytes[bytes.len() - 2] {
        base[..base.len() - 1].to_owned()
    } else {
        base.to_owned()
    }
}

/// The last sound of a reading, stress marks and length skipped.
fn last_sound(reading: &str) -> Option<char> {
    reading.chars().rev().find(|c| !matches!(c, 'ˈ' | 'ˌ' | 'ː'))
}

/// The plural or third-person `-s` (misaki's rule).
fn plural(stem: &str) -> String {
    match last_sound(stem) {
        Some(c) if "ptkfθ".contains(c) => format!("{stem}s"),
        Some(c) if "szʃʒʧʤ".contains(c) => format!("{stem}ᵻz"),
        _ => format!("{stem}z"),
    }
}

/// The past `-ed` (misaki's rule, US).
fn past(stem: &str) -> String {
    match last_sound(stem) {
        Some(c) if "pkfθʃsʧ".contains(c) => format!("{stem}t"),
        Some('t' | 'd') => format!("{stem}ᵻd"),
        _ => format!("{stem}d"),
    }
}

fn starts_with_vowel(reading: &str) -> bool {
    reading.chars().find(|c| !matches!(c, 'ˈ' | 'ˌ')).is_some_and(|c| VOWELS.contains(c))
}

/// Words and the punctuation Kokoro hears; everything else is a separator.
fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, tokens: &mut Vec<Token>| {
        let trimmed = word.trim_matches('\'');
        if !trimmed.is_empty() {
            tokens.push(Token::Word(trimmed.to_owned()));
        }
        word.clear();
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_alphanumeric() => word.push(c),
            '\'' if !word.is_empty() && chars.peek().is_some_and(|n| n.is_alphabetic()) => word.push(c),
            '\'' if word.ends_with('s') => word.push(c),
            '.' if chars.peek() == Some(&'.') => {
                flush(&mut word, &mut tokens);
                while chars.peek() == Some(&'.') {
                    chars.next();
                }
                tokens.push(Token::Punct('…'));
            }
            '-' if chars.peek() == Some(&'-') => {
                flush(&mut word, &mut tokens);
                chars.next();
                tokens.push(Token::Punct('—'));
            }
            '"' => {
                let opening = word.is_empty() && tokens.last().is_none_or(|t| !matches!(t, Token::Word(_)));
                flush(&mut word, &mut tokens);
                tokens.push(Token::Punct(if opening { '\u{201C}' } else { '\u{201D}' }));
            }
            c if CLOSING.contains(c) || OPENING.contains(c) => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Punct(c));
            }
            _ => flush(&mut word, &mut tokens),
        }
    }
    flush(&mut word, &mut tokens);
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lexicon() -> Lexicon {
        Lexicon::from_pairs([
            ("the", "ði"),
            ("a", "A"),
            ("A", "ˈA"),
            ("hello", "həlˈO"),
            ("chat", "ʧˈæt"),
            ("cat", "kˈæt"),
            ("dog", "dˈɔɡ"),
            ("box", "bˈɑks"),
            ("walk", "wˈɔk"),
            ("want", "wˈɑnt"),
            ("play", "plˈA"),
            ("stop", "stˈɑp"),
            ("make", "mˈAk"),
            ("happy", "hˈæpi"),
            ("apple", "ˈæpᵊl"),
            ("three", "θɹˈi"),
            ("I", "ˈI"),
            ("O", "ˈO"),
            ("K", "kˈA"),
            ("Z", "zˈi"),
            ("ok", "ˌOkˈA"),
        ])
    }

    #[test]
    fn words_and_punctuation_come_out_as_kokoro_reads_them() {
        let l = lexicon();
        assert_eq!(l.phonemize("Hello, chat!"), "həlˈO, ʧˈæt!");
        assert_eq!(l.phonemize("The cat... the apple"), "ðə kˈæt… ði ˈæpᵊl");
        assert_eq!(l.phonemize("A dog"), "ɐ dˈɔɡ");
        assert_eq!(l.phonemize("\"Hello\" (chat)"), "\u{201C}həlˈO\u{201D} (ʧˈæt)");
    }

    #[test]
    fn inflections_are_built_from_their_stems() {
        let l = lexicon();
        assert_eq!(l.phonemize("cats dogs boxes"), "kˈæts dˈɔɡz bˈɑksᵻz");
        assert_eq!(l.phonemize("walked wanted played stopped"), "wˈɔkt wˈɑntᵻd plˈAd stˈɑpt");
        assert_eq!(l.phonemize("making walking stopping"), "mˈAkɪŋ wˈɔkɪŋ stˈɑpɪŋ");
        assert_eq!(l.phonemize("happily happiness"), "hˈæpili hˈæpinəs");
        assert_eq!(l.phonemize("the cat's"), "ðə kˈæts");
    }

    #[test]
    fn numbers_become_words_and_unknown_words_are_spelled() {
        let l = lexicon();
        assert_eq!(l.phonemize("3 cats"), "θɹˈi kˈæts");
        assert_eq!(l.phonemize("OK Zoo"), "ˌOkˈA zˈi ˈO ˈO");
    }
}
