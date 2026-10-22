//! The phrase matcher: a final transcript becomes zero or more command matches. A pure function, no audio.
//!
//! **Normalising.** Both sides go through NFKC, case folding and punctuation removal (an apostrophe joins, any other
//! punctuation separates), and whitespace collapses to one space.
//!
//! **Tokens.** Space-separated languages compare word windows; Japanese, Chinese and Korean compare character windows
//! (spaces dropped), since a transcript in them may or may not have spaces.
//!
//! **Tolerance.** A window matches a phrase within one edit per five letters of the phrase (a 4-letter phrase must
//! match exactly; "sit down" takes "sit town" but not "it's down").
//!
//! **Alone.** A command with `alone = true` (the default) matches only when the whole utterance is command phrases and
//! the filler words of its language ("please", "can you", 「ください」), so "I waved at them" never waves. The
//! utterance is covered by a dynamic programme over the tokens: fewest edits, then the fewest filler tokens, then the
//! fewest phrases. When no such cover exists, commands with `alone = false` still match anywhere in the sentence.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// The most commands a phrase list holds.
pub const MAX_COMMANDS: usize = 256;
/// The most phrases one command has.
pub const MAX_PHRASES: usize = 16;
/// The longest phrase, in characters after normalising.
pub const MAX_PHRASE_CHARS: usize = 64;
/// One edit is allowed per this many letters of a phrase.
pub const LETTERS_PER_EDIT: usize = 5;
/// An utterance longer than this many tokens is never "just commands"; only `alone = false` commands are looked for.
pub const MAX_ALONE_TOKENS: usize = 256;

/// The filler words each language allows around an `alone` command, before normalising. A phrase list adds its own
/// under `[fillers]`.
const DEFAULT_FILLERS: &[(&str, &[&str])] = &[
    (
        "en",
        &[
            "please", "pls", "plz", "now", "can", "could", "would", "will", "you", "ok", "okay", "hey", "um", "uh",
            "er", "and", "then", "also", "just", "again", "go", "ahead", "right",
        ],
    ),
    (
        "ja",
        &[
            "ください",
            "下さい",
            "お願い",
            "おねがい",
            "お願いします",
            "おねがいします",
            "して",
            "ちょうだい",
            "ね",
            "よ",
            "ちょっと",
            "じゃあ",
            "えっと",
            "あの",
            "今",
            "それから",
            "そして",
            "と",
            "もう一回",
            "もういちど",
            "はい",
        ],
    ),
    ("zh", &["请", "吧", "一下", "现在", "了", "然后", "和"]),
    ("ko", &["주세요", "해줘", "좀", "지금", "그리고"]),
];

/// The languages compared on characters rather than words.
const CHARACTER_LANGUAGES: &[&str] = &["ja", "zh", "ko", "yue", "wuu"];

/// The person's voice commands: what each command id is said as, per language. TOML on disk:
///
/// ```toml
/// [[command]]
/// id = "wave"
/// say = ["wave", "say hi", "wave hello"]
/// lang = "en"
///
/// [[command]]
/// id = "wave"
/// say = ["手を振って"]
/// lang = "ja"
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PhraseList {
    /// The commands. One id may appear once per language.
    #[serde(default)]
    pub command: Vec<Command>,
    /// Extra filler words per language (`en`, `ja`), added to the built-in ones.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fillers: BTreeMap<String, Vec<String>>,
    /// The normalised form, built on first use.
    #[serde(skip)]
    #[schemars(skip)]
    compiled: OnceLock<Compiled>,
}

impl PartialEq for PhraseList {
    fn eq(&self, other: &Self) -> bool {
        self.command == other.command && self.fillers == other.fillers
    }
}

/// One command: its id, the phrases that trigger it, their language, and whether it must be said on its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Command {
    /// The id shows and templates refer to (`wave`, `nod`): `[a-z0-9_.-]`, at most 64 characters.
    pub id: String,
    /// What the person says, any of these.
    pub say: Vec<String>,
    /// The language of the phrases (`en`, `ja`, `en-GB`). Default `en`.
    #[serde(default = "english")]
    pub lang: String,
    /// The utterance must be just the phrase (and the language's filler words). Default true; false finds the phrase
    /// inside a longer sentence.
    #[serde(default = "yes")]
    pub alone: bool,
}

fn english() -> String {
    "en".to_owned()
}

const fn yes() -> bool {
    true
}

impl Command {
    /// The command `id`, said as any of `say`, in `lang`, alone.
    pub fn new(id: impl Into<String>, say: &[&str], lang: impl Into<String>) -> Self {
        Self { id: id.into(), say: say.iter().map(|s| (*s).to_owned()).collect(), lang: lang.into(), alone: true }
    }

    /// Whether the utterance must be just the phrase.
    #[must_use]
    pub fn with_alone(mut self, alone: bool) -> Self {
        self.alone = alone;
        self
    }
}

/// Why a phrase list is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PhraseError {
    /// The file doesn't parse.
    #[error("the phrase list doesn't read: {0}")]
    Parse(String),
    /// More than [`MAX_COMMANDS`] commands.
    #[error("a phrase list holds at most {MAX_COMMANDS} commands")]
    TooMany,
    /// A command id isn't `[a-z0-9_.-]`, 1 to 64 characters.
    #[error("`{0}` isn't a command id: use a-z, 0-9, `_`, `.` and `-`, at most 64 characters")]
    BadId(String),
    /// A command has no phrase, or more than [`MAX_PHRASES`].
    #[error("`{0}` needs 1 to {MAX_PHRASES} phrases")]
    PhraseCount(String),
    /// A phrase is empty after normalising, or longer than [`MAX_PHRASE_CHARS`].
    #[error("`{id}` has a phrase that is empty or longer than {MAX_PHRASE_CHARS} characters: {phrase:?}")]
    BadPhrase {
        /// The command.
        id: String,
        /// The phrase.
        phrase: String,
    },
    /// One id twice in one language.
    #[error("`{id}` appears twice for `{lang}`")]
    Duplicate {
        /// The command.
        id: String,
        /// The language.
        lang: String,
    },
}

impl PhraseList {
    /// A list of `commands`.
    pub fn new(commands: Vec<Command>) -> Self {
        Self { command: commands, fillers: BTreeMap::new(), compiled: OnceLock::new() }
    }

    /// With extra filler words for `lang`.
    #[must_use]
    pub fn with_fillers(mut self, lang: impl Into<String>, words: &[&str]) -> Self {
        self.fillers.entry(lang.into()).or_default().extend(words.iter().map(|w| (*w).to_owned()));
        self.compiled = OnceLock::new();
        self
    }

    /// Reads and checks a TOML phrase list.
    ///
    /// # Errors
    /// [`PhraseError::Parse`] for TOML that doesn't fit, or what [`check`](Self::check) refuses.
    pub fn from_toml(text: &str) -> Result<Self, PhraseError> {
        let list: Self = toml::from_str(text).map_err(|e| PhraseError::Parse(e.to_string()))?;
        list.check()?;
        Ok(list)
    }

    /// Checks the limits: ids, phrase counts and lengths, no id twice in a language.
    ///
    /// # Errors
    /// The first rule broken.
    pub fn check(&self) -> Result<(), PhraseError> {
        if self.command.len() > MAX_COMMANDS {
            return Err(PhraseError::TooMany);
        }
        let mut seen = std::collections::BTreeSet::new();
        for command in &self.command {
            let id_ok = !command.id.is_empty()
                && command.id.len() <= 64
                && command.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
            if !id_ok {
                return Err(PhraseError::BadId(command.id.clone()));
            }
            if command.say.is_empty() || command.say.len() > MAX_PHRASES {
                return Err(PhraseError::PhraseCount(command.id.clone()));
            }
            let lang = primary(&command.lang);
            if !seen.insert((command.id.clone(), lang.clone())) {
                return Err(PhraseError::Duplicate { id: command.id.clone(), lang });
            }
            for phrase in &command.say {
                let mut text = String::new();
                normalize(phrase, &mut text);
                let chars = text.chars().count();
                if chars == 0 || chars > MAX_PHRASE_CHARS {
                    return Err(PhraseError::BadPhrase { id: command.id.clone(), phrase: phrase.clone() });
                }
            }
        }
        Ok(())
    }

    fn compiled(&self) -> &Compiled {
        self.compiled.get_or_init(|| Compiled::new(self))
    }
}

/// One command heard: its id, the phrase-list entry that matched (never the transcript) and how closely.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Match {
    /// The command id.
    pub id: String,
    /// The entry of the phrase list that matched, as written there.
    pub phrase: String,
    /// 1 for an exact match, down to about 0.8 at the tolerance.
    pub score: f32,
}

/// The commands `text` (a final transcript) says, in the order it says them.
pub fn match_transcript(list: &PhraseList, text: &str) -> Vec<Match> {
    let compiled = list.compiled();
    if compiled.groups.is_empty() {
        return Vec::new();
    }
    let mut norm = String::with_capacity(text.len());
    normalize(text, &mut norm);
    if norm.is_empty() {
        return Vec::new();
    }
    let words = compiled.groups.iter().any(|g| !g.characters).then(|| Tokens::words(&norm));
    let chars = compiled.groups.iter().any(|g| g.characters).then(|| Tokens::characters(&norm));
    let mut found: Vec<(usize, Match)> = Vec::new();
    for group in &compiled.groups {
        let tokens = if group.characters { chars.as_ref() } else { words.as_ref() };
        let Some(tokens) = tokens else { continue };
        let hits = group.alone(tokens).unwrap_or_else(|| group.anywhere(tokens));
        found.extend(hits.into_iter().map(|hit| {
            let phrase = &group.phrases[hit.phrase];
            let command = &list.command[phrase.command];
            let score = 1.0 - hit.edits as f32 / phrase.text.len().max(1) as f32;
            (tokens.origin[hit.start], Match { id: command.id.clone(), phrase: command.say[phrase.say].clone(), score })
        }));
    }
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, m)| m).collect()
}

/// Normalises `text` into `out`: NFKC, lower case, apostrophes dropped, other punctuation and symbols as spaces, one
/// space between words, none at the ends.
pub fn normalize(text: &str, out: &mut String) {
    out.clear();
    let mut space = false;
    for c in text.nfkc() {
        if matches!(c, '\'' | '\u{2019}' | '\u{02BC}' | '`') {
            continue;
        }
        if c.is_alphanumeric() || is_mark(c) {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(c.to_lowercase());
        } else {
            space = true;
        }
    }
}

/// Combining marks that belong to the letter before them (the kana voicing marks, Indic and Thai vowel signs).
fn is_mark(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036F}' | '\u{0483}'..='\u{0489}' | '\u{0591}'..='\u{05BD}' | '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}' | '\u{0900}'..='\u{0DFF}' | '\u{0E31}'..='\u{0E4E}' | '\u{3099}'..='\u{309A}')
}

/// The primary language subtag, lower case: `ja` for `ja-JP`.
fn primary(lang: &str) -> String {
    lang.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase()
}

/// A 64-bit set of the characters in `text` (hashed; a collision only weakens the filter).
fn mask(text: impl IntoIterator<Item = char>) -> u64 {
    text.into_iter().fold(0, |m, c| m | 1 << (u32::from(c).wrapping_mul(0x9E37_79B1) >> 26))
}

/// The edit distance between `a` and `b` when it is at most `bound`.
fn bounded_distance(a: &[char], b: &str, bound: usize) -> Option<usize> {
    let width = a.len().min(MAX_PHRASE_CHARS);
    let mut row = [0usize; MAX_PHRASE_CHARS + 1];
    for (j, cell) in row.iter_mut().enumerate().take(width + 1) {
        *cell = j;
    }
    for (i, cb) in b.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        let mut best = row[0];
        for j in 0..width {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(a[j] != cb)).min(above + 1).min(row[j] + 1);
            diagonal = above;
            best = best.min(row[j + 1]);
        }
        if best > bound {
            return None;
        }
    }
    (row[width] <= bound).then_some(row[width])
}

/// A transcript cut into tokens for one way of comparing.
struct Tokens {
    /// The text windows are cut from: words joined by one space, or the characters with no spaces.
    text: String,
    /// Each token's byte range in `text`.
    spans: Vec<(usize, usize)>,
    /// Each token's byte offset in the normalised transcript, to order matches across languages.
    origin: Vec<usize>,
    /// Each token's character set.
    masks: Vec<u64>,
    /// Characters before each token (`chars[i]`), and in all (`chars[n]`), spaces excluded.
    chars: Vec<usize>,
    /// Whether tokens are joined by spaces.
    spaced: bool,
}

impl Tokens {
    fn words(norm: &str) -> Self {
        let mut tokens = Self::empty(norm.to_owned(), true);
        let mut start = 0;
        for word in norm.split(' ') {
            tokens.push(start, start + word.len(), start, word);
            start += word.len() + 1;
        }
        tokens
    }

    fn characters(norm: &str) -> Self {
        let mut tokens = Self::empty(String::with_capacity(norm.len()), false);
        for (at, c) in norm.char_indices().filter(|(_, c)| *c != ' ') {
            let start = tokens.text.len();
            tokens.text.push(c);
            let end = tokens.text.len();
            tokens.spans.push((start, end));
            tokens.origin.push(at);
            tokens.masks.push(mask([c]));
            tokens.chars.push(tokens.chars.last().copied().unwrap_or(0) + 1);
        }
        tokens.chars.insert(0, 0);
        tokens
    }

    fn empty(text: String, spaced: bool) -> Self {
        Self { text, spans: Vec::new(), origin: Vec::new(), masks: Vec::new(), chars: Vec::new(), spaced }
    }

    fn push(&mut self, start: usize, end: usize, origin: usize, word: &str) {
        if self.chars.is_empty() {
            self.chars.push(0);
        }
        self.spans.push((start, end));
        self.origin.push(origin);
        self.masks.push(mask(word.chars()));
        let before = self.chars.last().copied().unwrap_or(0);
        self.chars.push(before + word.chars().count());
    }

    fn len(&self) -> usize {
        self.spans.len()
    }

    fn token(&self, i: usize) -> &str {
        &self.text[self.spans[i].0..self.spans[i].1]
    }

    /// The window of tokens `i..j` as text.
    fn window(&self, i: usize, j: usize) -> &str {
        &self.text[self.spans[i].0..self.spans[j - 1].1]
    }

    /// The window's length in characters, its spaces included.
    fn window_chars(&self, i: usize, j: usize) -> usize {
        let spaces = if self.spaced { j - i - 1 } else { 0 };
        self.chars[j] - self.chars[i] + spaces
    }

    fn window_mask(&self, i: usize, j: usize) -> u64 {
        let spaces = if self.spaced && j - i > 1 { mask([' ']) } else { 0 };
        self.masks[i..j].iter().fold(spaces, |m, t| m | t)
    }
}

/// A phrase list normalised and grouped by language.
#[derive(Clone, Debug, Default)]
struct Compiled {
    groups: Vec<Group>,
}

/// The commands of one language.
#[derive(Clone, Debug)]
struct Group {
    /// Compared on characters.
    characters: bool,
    /// The filler words, each as its tokens.
    fillers: Vec<Vec<String>>,
    phrases: Vec<Phrase>,
}

/// One phrase of one command, normalised.
#[derive(Clone, Debug)]
struct Phrase {
    /// The command's index in the list.
    command: usize,
    /// The phrase's index in the command's `say`.
    say: usize,
    /// The text compared: words joined by spaces, or the characters with no spaces.
    text: Vec<char>,
    /// How many tokens it has.
    tokens: usize,
    /// The edits allowed.
    tolerance: usize,
    /// Its character set.
    mask: u64,
    alone: bool,
}

/// How a cover reached a token: its cost (edits, filler tokens, phrases), where it came from, and the phrase it took.
type Step = ((usize, usize, usize), usize, Option<Hit>);

/// A phrase found in a window of tokens.
#[derive(Copy, Clone, Debug)]
struct Hit {
    start: usize,
    end: usize,
    phrase: usize,
    edits: usize,
}

impl Compiled {
    fn new(list: &PhraseList) -> Self {
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        let mut norm = String::new();
        for (index, command) in list.command.iter().enumerate() {
            let lang = primary(&command.lang);
            let group = groups.entry(lang.clone()).or_insert_with(|| Group::new(&lang, list));
            for (say, phrase) in command.say.iter().enumerate() {
                normalize(phrase, &mut norm);
                let letters = norm.chars().filter(|c| *c != ' ').count();
                if letters == 0 || norm.chars().count() > MAX_PHRASE_CHARS {
                    continue;
                }
                let text: Vec<char> = if group.characters {
                    norm.chars().filter(|c| *c != ' ').collect()
                } else {
                    norm.chars().collect()
                };
                let tokens = if group.characters { letters } else { norm.split(' ').count() };
                group.phrases.push(Phrase {
                    command: index,
                    say,
                    mask: mask(text.iter().copied()),
                    text,
                    tokens,
                    tolerance: letters / LETTERS_PER_EDIT,
                    alone: command.alone,
                });
            }
        }
        Self { groups: groups.into_values().filter(|g| !g.phrases.is_empty()).collect() }
    }
}

impl Group {
    fn new(lang: &str, list: &PhraseList) -> Self {
        let characters = CHARACTER_LANGUAGES.contains(&lang);
        let builtin = DEFAULT_FILLERS.iter().filter(|(l, _)| *l == lang).flat_map(|(_, words)| words.iter().copied());
        let extra =
            list.fillers.iter().filter(|(l, _)| primary(l) == lang).flat_map(|(_, w)| w.iter().map(String::as_str));
        let mut norm = String::new();
        let mut fillers: Vec<Vec<String>> = Vec::new();
        for word in builtin.chain(extra) {
            normalize(word, &mut norm);
            let tokens: Vec<String> = if characters {
                norm.chars().filter(|c| *c != ' ').map(String::from).collect()
            } else {
                norm.split(' ').filter(|w| !w.is_empty()).map(str::to_owned).collect()
            };
            if !tokens.is_empty() && !fillers.contains(&tokens) {
                fillers.push(tokens);
            }
        }
        Self { characters, fillers, phrases: Vec::new() }
    }

    /// The phrases that match the window `i..j`, with their edits.
    fn hits_at(&self, tokens: &Tokens, i: usize, mut keep: impl FnMut(Hit)) {
        let n = tokens.len();
        for (index, phrase) in self.phrases.iter().enumerate() {
            let t = phrase.tolerance;
            let shortest = phrase.tokens.saturating_sub(t).max(1);
            let longest = (phrase.tokens + t).min(n - i);
            for j in (i + shortest)..=(i + longest) {
                if tokens.window_chars(i, j).abs_diff(phrase.text.len()) > t {
                    continue;
                }
                let window = tokens.window_mask(i, j);
                let missing = (phrase.mask & !window).count_ones().max((window & !phrase.mask).count_ones());
                if missing as usize > t {
                    continue;
                }
                if let Some(edits) = bounded_distance(&phrase.text, tokens.window(i, j), t) {
                    keep(Hit { start: i, end: j, phrase: index, edits });
                }
            }
        }
    }

    /// The commands when the utterance is nothing but phrases and fillers: the cover with the fewest edits, then the
    /// fewest filler tokens, then the fewest phrases. `None` when there is no such cover or it holds no phrase.
    fn alone(&self, tokens: &Tokens) -> Option<Vec<Hit>> {
        let n = tokens.len();
        if n == 0 || n > MAX_ALONE_TOKENS {
            return None;
        }
        // best[i]: the cost of covering tokens 0..i, and how the cover reached i.
        let mut best: Vec<Option<Step>> = vec![None; n + 1];
        best[0] = Some(((0, 0, 0), 0, None));
        for i in 0..n {
            let Some(((edits, fill, pieces), _, _)) = best[i] else { continue };
            let mut relax = |j: usize, cost: (usize, usize, usize), hit: Option<Hit>| {
                if best[j].is_none_or(|(c, _, _)| cost < c) {
                    best[j] = Some((cost, i, hit));
                }
            };
            for filler in &self.fillers {
                let j = i + filler.len();
                if j <= n && filler.iter().enumerate().all(|(k, f)| tokens.token(i + k) == f) {
                    relax(j, (edits, fill + filler.len(), pieces), None);
                }
            }
            self.hits_at(tokens, i, |hit| {
                relax(hit.end, (edits + hit.edits, fill, pieces + 1), Some(hit));
            });
        }
        let mut hits = Vec::new();
        let mut at = n;
        while at > 0 {
            let (_, from, hit) = best[at]?;
            hits.extend(hit);
            at = from;
        }
        hits.reverse();
        (!hits.is_empty()).then_some(hits)
    }

    /// The `alone = false` phrases anywhere in the utterance: at each start the closest, longest one; matches don't
    /// overlap.
    fn anywhere(&self, tokens: &Tokens) -> Vec<Hit> {
        if self.phrases.iter().all(|p| p.alone) {
            return Vec::new();
        }
        let mut hits = Vec::new();
        let mut free = 0;
        for i in 0..tokens.len() {
            if i < free {
                continue;
            }
            let mut chosen: Option<Hit> = None;
            self.hits_at(tokens, i, |hit| {
                if self.phrases[hit.phrase].alone {
                    return;
                }
                let better = chosen
                    .is_none_or(|c| (hit.edits, std::cmp::Reverse(hit.end)) < (c.edits, std::cmp::Reverse(c.end)));
                if better {
                    chosen = Some(hit);
                }
            });
            if let Some(hit) = chosen {
                free = hit.end;
                hits.push(hit);
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalising_folds_case_width_and_punctuation() {
        let mut out = String::new();
        normalize("  Wave,  HELLO!! ", &mut out);
        assert_eq!(out, "wave hello");
        normalize("It's down.", &mut out);
        assert_eq!(out, "its down");
        normalize("「手を振って」、ください。", &mut out);
        assert_eq!(out, "手を振って ください");
        normalize("ＷＡＶＥ", &mut out);
        assert_eq!(out, "wave");
        normalize("ﾃﾞｽ", &mut out);
        assert_eq!(out, "デス");
    }

    #[test]
    fn the_bounded_distance_agrees_with_the_full_one() {
        let pairs =
            [("sit down", "sit town"), ("sit down", "its down"), ("wave", "waved"), ("wave", "wave"), ("", "ab")];
        for (a, b) in pairs {
            let full = omuzaki_core::text::edit_distance(a, b);
            let chars: Vec<char> = a.chars().collect();
            for bound in 0..4 {
                assert_eq!(bounded_distance(&chars, b, bound), (full <= bound).then_some(full), "{a} {b} {bound}");
            }
        }
    }
}
