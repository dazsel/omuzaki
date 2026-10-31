//! Reading a description: words to the lexicon's entries.
//!
//! The text is lower-cased and split into words (letters, digits and `#` colours; a hyphen splits, so `silver-haired`
//! is `silver haired`). At each word the longest phrase the lexicon knows is taken, so `twin tails` wins over `tails`.
//! What it reads:
//!
//! - **Choices** (`round glasses`, `hoodie`, `sleepy eyes`): one option of a facet each. A facet that takes one
//!   option keeps the first one named; a later one is reported as ignored, with the reason.
//! - **Colours** (`silver`, `#c0c8d8`) bind to the next thing that can take a colour within [`COLOUR_REACH`] words
//!   (`silver twin tails`, `green eyes`, `a red dress`). Colours joined by `and`, `to` or `into` before it bind
//!   together, as a gradient from the first to the last (`blonde fading to pink hair`). With nothing after them, a
//!   colour binds back across `is` or `are` (`her hair is silver`). A colour that binds to nothing is reported.
//! - **Negation**: `no`, `without` or `not` before a choice drops it (`no glasses`), and says so.
//! - **The name**: `named` or `called` takes the next word, as written.
//!
//! Every word that is neither read nor a common little word comes back as unknown, so the caller (a person, or an
//! agent that can rephrase) sees exactly what was not understood.

use std::collections::BTreeSet;

use omuzaki_core::colour::Colour;

use crate::lexicon::{ColourTarget, Lexicon};
use crate::shades;

/// How many words after a colour its target may be (`silver long twin tails`: three).
pub const COLOUR_REACH: usize = 4;
/// How many words before a colour, across `is` or `are`, a target may be (`her hair is silver`).
const BACK_REACH: usize = 3;

/// Words that say nothing about the character, never reported as unknown.
const LITTLE_WORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "and",
    "or",
    "with",
    "wearing",
    "wears",
    "in",
    "of",
    "on",
    "her",
    "his",
    "their",
    "she",
    "he",
    "they",
    "is",
    "are",
    "has",
    "have",
    "who",
    "that",
    "very",
    "quite",
    "really",
    "bit",
    "little",
    "some",
    "to",
    "into",
    "fading",
    "fades",
    "plus",
    "also",
    "by",
    "for",
    "at",
    "as",
    "its",
    "it",
    "s",
    "like",
    "looks",
    "look",
    "kind",
    "sort",
    "style",
    "styled",
    "pair",
    "set",
    "one",
    "two",
    "both",
    "over",
    "under",
    "from",
    "this",
    "girl",
    "boy",
    "character",
    "vtuber",
    "person",
    "someone",
    "wear",
    "dressed",
    "named",
    "called",
    "no",
    "without",
    "not",
    "colour",
    "color",
    "coloured",
    "colored",
    "tone",
    "toned",
];

/// A word of the description: lower case for matching, as written for a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// Lower case.
    pub text: String,
    /// As written.
    pub raw: String,
}

/// Splits `description` into words.
pub fn words(description: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut raw = String::new();
    let flush = |raw: &mut String, out: &mut Vec<Word>| {
        if !raw.is_empty() {
            out.push(Word { text: raw.to_lowercase(), raw: std::mem::take(raw) });
        }
    };
    for c in description.chars() {
        if c.is_alphanumeric() || (c == '#' && raw.is_empty()) {
            raw.push(c);
        } else if c == '\'' {
            // `librarian's` reads as `librarian`.
            flush(&mut raw, &mut out);
            out.push(Word { text: "s".into(), raw: "'".into() });
        } else {
            flush(&mut raw, &mut out);
        }
    }
    flush(&mut raw, &mut out);
    out
}

/// What one run of words was read as.
#[derive(Clone, Debug, PartialEq)]
pub enum Reading {
    /// An option of a facet, by the lexicon's ids.
    Choice {
        /// The facet.
        facet: String,
        /// The option.
        option: String,
        /// A hint (`mage` for a robe) rather than the thing itself (`robe`): a hint chooses only when nothing
        /// stronger in the description chooses for the same facet or part.
        hint: bool,
    },
    /// A thing that takes a colour, named on its own (`hair`, `eyes`, `outfit`).
    Target(ColourTarget),
    /// A colour.
    Colour(Colour),
}

/// A phrase found in the description.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The words, as written.
    pub words: String,
    /// The first word's index.
    pub at: usize,
    /// How many words.
    pub len: usize,
    /// What they mean.
    pub reading: Reading,
}

/// A choice the description makes, with the words that made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    /// The facet.
    pub facet: String,
    /// The option.
    pub option: String,
    /// The words that chose it.
    pub because: String,
}

/// Colours the description gives a target, in order (two or more make a gradient), with the words that gave them.
#[derive(Clone, Debug, PartialEq)]
pub struct Coloured {
    /// What takes the colours: a plain target, or the option a colour was bound to.
    pub target: Bound,
    /// The colours, first to last.
    pub colours: Vec<Colour>,
    /// The words that gave them.
    pub because: String,
    /// The index of the word after them, so a gradient can carry on (`pink hair fading to blonde`).
    pub ends_at: usize,
}

/// What a colour is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bound {
    /// A target named on its own.
    Target(ColourTarget),
    /// A chosen option (`a red dress`, `round red glasses`).
    Option {
        /// The option's facet.
        facet: String,
        /// The option.
        option: String,
    },
}

/// Words the reading set aside, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aside {
    /// The words.
    pub words: String,
    /// Why they were set aside.
    pub reason: String,
}

/// Everything read from one description.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Read {
    /// The choices, in the order they were named.
    pub picked: Vec<Picked>,
    /// The colours, by target.
    pub coloured: Vec<Coloured>,
    /// The name, when the description gives one.
    pub name: Option<String>,
    /// Words read but not used (a second choice for a one-option facet, a negated choice, a colour with no target).
    pub aside: Vec<Aside>,
    /// Words not understood, in order, each once.
    pub unknown: Vec<String>,
    /// The words before the first `with`, `in`, `wearing` or comma-like break that read as nothing else: what the
    /// character is (`sleepy librarian`), for a name when none is given.
    pub what: Vec<String>,
}

/// Reads `description` with `lexicon`.
pub fn read(description: &str, lexicon: &Lexicon) -> Read {
    let words = words(description);
    let found = find(&words, lexicon);
    let mut out = Read::default();
    let mut used = vec![false; words.len()];
    for f in &found {
        used[f.at..f.at + f.len].iter_mut().for_each(|u| *u = true);
    }
    take_name(&words, &mut used, &mut out);
    let negated =
        |f: &Found| f.at > 0 && matches!(words.get(f.at - 1).map(|w| w.text.as_str()), Some("no" | "without" | "not"));
    // Choices first, so a colour can bind to the option its words name: things before hints, each in the order
    // named. Every choice, kept or not, stays a place a colour can bind to, so `a mage in a purple cloak` never hands
    // the purple to whatever comes next.
    let mut chosen: Vec<(usize, usize, Bound)> = Vec::new();
    let mut kept: Vec<(Picked, bool)> = Vec::new();
    for pass_hints in [false, true] {
        for f in &found {
            let Reading::Choice { facet, option, hint } = &f.reading else { continue };
            if *hint != pass_hints {
                continue;
            }
            chosen.push((f.at, f.len, Bound::Option { facet: facet.clone(), option: option.clone() }));
            let reason = if negated(f) {
                Some("it follows a no, without or not".to_owned())
            } else {
                match lexicon.option(facet, option).and_then(|o| o.near.as_ref()) {
                    Some(near) if !is_near(&words, &found, f, near, lexicon) => {
                        Some(format!("it counts only next to the {}", target_noun(near)))
                    }
                    _ => clash(lexicon, &kept, facet, option, *hint),
                }
            };
            match reason {
                Some(reason) if !reason.is_empty() => out.aside.push(Aside { words: f.words.clone(), reason }),
                Some(_) => {}
                None => kept
                    .push((Picked { facet: facet.clone(), option: option.clone(), because: f.words.clone() }, *hint)),
            }
        }
    }
    kept.sort_by_key(|(p, _)| found.iter().position(|f| f.words == p.because));
    out.picked = kept.into_iter().map(|(p, _)| p).collect();
    for f in &found {
        if let Reading::Target(t) = &f.reading {
            chosen.push((f.at, f.len, Bound::Target(t.clone())));
        }
    }
    bind_colours(&words, &found, &chosen, lexicon, &mut out);
    out.what = what(&words);
    let mut seen = BTreeSet::new();
    for (w, used) in words.iter().zip(&used) {
        let little = LITTLE_WORDS.contains(&w.text.as_str()) || w.text.chars().all(|c| c.is_ascii_digit());
        if !used && !little && seen.insert(w.text.clone()) {
            out.unknown.push(w.text.clone());
        }
    }
    out
}

/// Words that change the colour after them (`dark brown`, `pastel pink`), and how: a shade toward dark, or a mix
/// toward white.
const MODIFIERS: [(&str, Shift); 9] = [
    ("dark", Shift::Darker(0.35)),
    ("deep", Shift::Darker(0.2)),
    ("light", Shift::Lighter(0.35)),
    ("pale", Shift::Lighter(0.45)),
    ("pastel", Shift::Lighter(0.35)),
    ("soft", Shift::Lighter(0.25)),
    ("bright", Shift::Lighter(0.0)),
    ("vivid", Shift::Lighter(0.0)),
    ("dusty", Shift::Darker(0.15)),
];

/// How a modifier moves a colour.
#[derive(Clone, Copy, Debug)]
enum Shift {
    Darker(f32),
    Lighter(f32),
}

/// Every phrase the lexicon knows, longest first at each word, never overlapping. A modifier just before a colour
/// (`dark brown`) is read with it, as one shifted colour.
pub fn find(words: &[Word], lexicon: &Lexicon) -> Vec<Found> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < words.len() {
        let shift = MODIFIERS.iter().find(|(m, _)| words[at].text == *m).map(|(_, s)| *s);
        if let Some(shift) = shift
            && let Some(Found { reading: Reading::Colour(c), len, .. }) = longest_at(words, at + 1, lexicon)
        {
            let colour = match shift {
                Shift::Darker(t) => shades::shade(c, t),
                Shift::Lighter(t) => shades::mix(c, Colour::WHITE, t),
            };
            found.push(Found { words: raw(words, at, len + 1), at, len: len + 1, reading: Reading::Colour(colour) });
            at += len + 1;
            continue;
        }
        match longest_at(words, at, lexicon) {
            Some(f) => {
                at += f.len;
                found.push(f);
            }
            None => at += 1,
        }
    }
    found
}

/// The longest phrase starting at word `at`, or a `#rrggbb` colour.
fn longest_at(words: &[Word], at: usize, lexicon: &Lexicon) -> Option<Found> {
    let word = words.get(at)?;
    if let Some(colour) = hex(&word.text) {
        return Some(Found { words: word.raw.clone(), at, len: 1, reading: Reading::Colour(colour) });
    }
    let (phrase, reading) =
        lexicon.phrases().filter(|(phrase, _)| starts_with(words, at, phrase)).max_by_key(|(p, _)| p.len())?;
    Some(Found { words: raw(words, at, phrase.len()), at, len: phrase.len(), reading: reading.clone() })
}

/// Words `at..at + len` as written, joined by spaces.
fn raw(words: &[Word], at: usize, len: usize) -> String {
    words[at..at + len].iter().map(|w| w.raw.as_str()).collect::<Vec<_>>().join(" ")
}

/// Whether `phrase` is the words from `at`; a word also matches its plural (`braid` matches `braids`).
fn starts_with(words: &[Word], at: usize, phrase: &[String]) -> bool {
    phrase.len() <= words.len() - at
        && phrase.iter().zip(&words[at..]).all(|(p, w)| w.text == *p || w.text.strip_suffix('s') == Some(p.as_str()))
}

/// A `#rrggbb` word as a colour.
fn hex(word: &str) -> Option<Colour> {
    if word.starts_with('#') { word.parse().ok() } else { None }
}

/// `named Mika` or `called Mika`: the word after, as written; the two words are used.
fn take_name(words: &[Word], used: &mut [bool], out: &mut Read) {
    let Some(at) = words.iter().position(|w| w.text == "named" || w.text == "called") else { return };
    let Some(name) = words.get(at + 1) else { return };
    out.name = Some(name.raw.clone());
    used[at] = true;
    used[at + 1] = true;
}

/// Binds each run of colours to its target: forward within [`COLOUR_REACH`] words, else back across `is` or `are`.
fn bind_colours(words: &[Word], found: &[Found], chosen: &[(usize, usize, Bound)], lexicon: &Lexicon, out: &mut Read) {
    let mut i = 0;
    while i < found.len() {
        let Reading::Colour(first) = found[i].reading else {
            i += 1;
            continue;
        };
        // A run: colours joined by `and`, `to`, `into` or `fading to`.
        let mut colours = vec![first];
        let mut because = vec![found[i].words.clone()];
        let mut end = found[i].at + found[i].len;
        let mut j = i + 1;
        while let Some(next) = found.get(j) {
            let Reading::Colour(c) = next.reading else { break };
            let gap = &words[end..next.at];
            if !gap.iter().all(|w| matches!(w.text.as_str(), "and" | "to" | "into" | "fading" | "fades" | "with")) {
                break;
            }
            colours.push(c);
            because.push(next.words.clone());
            end = next.at + next.len;
            j += 1;
        }
        // `pink hair fading to blonde`: a run just after a coloured thing, across a fading word, extends its gradient.
        let start = found[i].at;
        let fades = gradient_link(words, start)
            .and_then(|link_start| out.coloured.iter_mut().rev().find(|c| c.ends_at == link_start));
        if let Some(gradient) = fades {
            gradient.colours.extend(colours);
            gradient.because = format!("{} {}", gradient.because, words_between(words, gradient.ends_at, end));
            i = j;
            continue;
        }
        let forward = chosen
            .iter()
            .filter(|(at, _, b)| *at >= end && at - end < COLOUR_REACH && takes(b, lexicon))
            .min_by_key(|(at, _, _)| *at);
        let backward = || {
            let link =
                start.checked_sub(1).and_then(|k| words.get(k)).is_some_and(|w| w.text == "is" || w.text == "are");
            if !link {
                return None;
            }
            chosen
                .iter()
                .filter(|(at, len, b)| at + len < start && start - (at + len) <= BACK_REACH && takes(b, lexicon))
                .max_by_key(|(at, _, _)| *at)
        };
        match forward.or_else(backward) {
            Some((at, len, bound)) => {
                let ends_at = (at + len).max(end);
                let because = words_between(words, start.min(*at), ends_at);
                out.coloured.push(Coloured { target: bound.clone(), colours, because, ends_at });
            }
            None => out.aside.push(Aside {
                words: because.join(" "),
                reason: "nothing after it takes a colour (try \"silver hair\" or \"a red dress\")".into(),
            }),
        }
        i = j;
    }
}

/// Where the fading words before word `at` start (`fading to`, `into`, `turning`), if `at` follows some.
fn gradient_link(words: &[Word], at: usize) -> Option<usize> {
    const LINKS: [&str; 6] = ["fading", "fades", "to", "into", "turning", "ombre"];
    let mut start = at;
    while start > 0 && LINKS.contains(&words[start - 1].text.as_str()) && at - start < 3 {
        start -= 1;
    }
    let fades = words[start..at].iter().any(|w| matches!(w.text.as_str(), "fading" | "fades" | "into" | "turning"));
    (start < at && fades).then_some(start)
}

/// Words `from..to` as written.
fn words_between(words: &[Word], from: usize, to: usize) -> String {
    words[from..to.min(words.len())].iter().map(|w| w.raw.as_str()).collect::<Vec<_>>().join(" ")
}

/// Why option `option` of `facet` can't join the choices `kept`, if it can't: an empty reason when it is already
/// there (named twice). A facet that takes one keeps the first thing named (a hint only when no thing was named); a
/// facet that takes several keeps one option per part (`ram horns` over the hint `demon`, whose horns are the same
/// part).
fn clash(lexicon: &Lexicon, kept: &[(Picked, bool)], facet: &str, option: &str, hint: bool) -> Option<String> {
    if kept.iter().any(|(p, _)| p.facet == facet && p.option == option) {
        return Some(String::new());
    }
    let many = lexicon.facet(facet).is_some_and(|x| x.many);
    if !many {
        let first = kept.iter().find(|(p, _)| p.facet == facet)?;
        return Some(if hint {
            String::new()
        } else {
            format!("{facet} takes one choice and the description already chose \"{}\"", first.0.because)
        });
    }
    let parts = |o: &str| lexicon.option(facet, o).map(|c| c.parts.iter().map(|p| p.id.clone()).collect::<Vec<_>>());
    let mine = parts(option).unwrap_or_default();
    let other = kept
        .iter()
        .find(|(p, _)| p.facet == facet && parts(&p.option).unwrap_or_default().iter().any(|id| mine.contains(id)))?;
    Some(format!("\"{}\" already gives the {}", other.0.because, mine.join(", ")))
}

/// Whether a thing of kind `near` is close to `f`: within [`COLOUR_REACH`] words after it, or just before it across
/// `is` or `are` (`her hair is long`). A thing is a target noun (`hair`) or an option that takes that colour
/// (`twin tails`).
fn is_near(words: &[Word], found: &[Found], f: &Found, near: &ColourTarget, lexicon: &Lexicon) -> bool {
    let is_kind = |g: &Found| match &g.reading {
        Reading::Target(t) => t == near,
        Reading::Choice { facet, option, .. } => {
            lexicon.option(facet, option).is_some_and(|o| o.colour.as_ref() == Some(near))
        }
        Reading::Colour(_) => false,
    };
    let end = f.at + f.len;
    let after = found.iter().any(|g| g.at >= end && g.at - end < COLOUR_REACH && !std::ptr::eq(g, f) && is_kind(g));
    let link = f.at.checked_sub(1).and_then(|k| words.get(k)).is_some_and(|w| w.text == "is" || w.text == "are");
    let before = link && found.iter().any(|g| g.at + g.len < f.at && f.at - (g.at + g.len) <= BACK_REACH && is_kind(g));
    after || before
}

/// The noun for a kind of thing, for a reason.
fn target_noun(target: &ColourTarget) -> &str {
    match target {
        ColourTarget::Hair => "hair",
        ColourTarget::Eyes => "eyes",
        ColourTarget::Outfit => "outfit",
        ColourTarget::Part(p) => p,
    }
}

/// Whether `bound` can take a colour.
fn takes(bound: &Bound, lexicon: &Lexicon) -> bool {
    match bound {
        Bound::Target(_) => true,
        Bound::Option { facet, option } => lexicon.option(facet, option).is_some_and(|o| o.colour.is_some()),
    }
}

/// The words before the first break, less the little ones: what the character is (`sleepy librarian`).
fn what(words: &[Word]) -> Vec<String> {
    const BREAKS: &[&str] = &["with", "in", "wearing", "who", "that", "and", "has", "named", "called"];
    words
        .iter()
        .take_while(|w| !BREAKS.contains(&w.text.as_str()))
        .filter(|w| !LITTLE_WORDS.contains(&w.text.as_str()))
        .map(|w| w.raw.clone())
        .collect()
}
