//! The streaming inline-tag parser (`ai-performer.md` §6.2).
//!
//! The model writes speech with tags in it: `Hi chat! [emote happy 0.8] Fold it and see!`. Deltas arrive in any
//! split, so the parser holds text from `[` until `]` (at most [`TAG_MAX_CHARS`]) and emits [`BrainEvent`]s: speakable
//! text, intents at the character offset of the text spoken before them, `[style …]`, `[pass]`, and the runs it
//! dropped. A run that doesn't parse, an unknown verb, an over-long run and an unclosed `[` at the end are **dropped,
//! never spoken**. Markdown (`*`, `#`, backticks), emoji names (`:smile:`), markup (`<br>`) and a reasoning model's
//! `<think>…</think>` block are stripped from speech, and runs of whitespace become one space, so the text the voice
//! gets is what a person would say.

use omuzaki_anim_api::score::{ClipId, ExpressionId};
use omuzaki_performer_api::brain::{BrainEvent, ParseIssue, ParseIssueKind};
use omuzaki_performer_api::intent::{GazeTarget, Intent, PlaceId, StyleTag};
use omuzaki_performer_api::limits::{EMOTE_HOLD_DEFAULT_S, LOOK_HOLD_DEFAULT_S, TAG_MAX_CHARS};

/// The longest emoji name held back before it is spoken as text (`:thumbsup:` and its kin are shorter).
const EMOJI_MAX_CHARS: usize = 32;
/// The longest `<…>` run treated as markup.
const MARKUP_MAX_CHARS: usize = 16;
/// What closes a reasoning block.
const THINK_END: &str = "</think>";

/// What the parser is in the middle of.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    /// Plain text.
    None,
    /// Inside `[`: the run so far, without the bracket.
    Tag(String),
    /// After a `:` that may open an emoji name: the name so far, without the colon.
    Emoji(String),
    /// After a `<` that may open markup: the run so far, without the angle bracket.
    Angle(String),
    /// Inside `<think>`: the last few characters, to find its end. Nothing in it is spoken.
    Think(String),
}

/// Turns streamed reply text into [`BrainEvent`]s. One per turn.
#[derive(Debug, Clone)]
pub struct TagParser {
    held: Held,
    /// Characters of speakable text emitted so far: the offset the next intent gets.
    spoken: u32,
    /// Whitespace seen since the last spoken character: spoken as one space before the next one, so runs collapse and
    /// trailing whitespace is never spoken.
    space: bool,
    out: String,
}

impl Default for TagParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TagParser {
    /// A parser at the start of a turn.
    pub fn new() -> Self {
        Self { held: Held::None, spoken: 0, space: false, out: String::new() }
    }

    /// Characters of speakable text emitted so far.
    pub fn spoken_chars(&self) -> u32 {
        self.spoken
    }

    /// Feeds a delta; returns what it completed, in order.
    pub fn push(&mut self, delta: &str) -> Vec<BrainEvent> {
        let mut events = Vec::new();
        for c in delta.chars() {
            self.feed(c, &mut events);
        }
        self.flush_text(&mut events);
        events
    }

    /// Ends the turn: an unclosed `[` is dropped and reported; a held `:name` is spoken as it was.
    pub fn finish(&mut self) -> Vec<BrainEvent> {
        let mut events = Vec::new();
        match std::mem::replace(&mut self.held, Held::None) {
            // An unfinished reasoning block is never spoken.
            Held::None | Held::Think(_) => {}
            Held::Tag(run) => {
                self.flush_text(&mut events);
                events.push(BrainEvent::Issue(ParseIssue::new(cut(&run), ParseIssueKind::Unclosed, self.spoken)));
            }
            Held::Emoji(name) => {
                self.speak(':');
                for c in name.chars() {
                    self.speak(c);
                }
            }
            Held::Angle(run) => {
                for c in run.chars() {
                    self.speak(c);
                }
            }
        }
        self.flush_text(&mut events);
        events
    }

    fn feed(&mut self, c: char, events: &mut Vec<BrainEvent>) {
        match &mut self.held {
            Held::Tag(run) => {
                if c == ']' {
                    let run = std::mem::take(run);
                    self.held = Held::None;
                    self.flush_text(events);
                    self.tag(&run, events);
                } else if run.chars().count() >= TAG_MAX_CHARS {
                    // Too long to be a tag: drop it, and everything up to its `]`, if one comes (the run keeps
                    // swallowing until then, so its tail is never spoken either).
                    if run.chars().count() == TAG_MAX_CHARS {
                        let report = cut(run);
                        run.push(c);
                        self.flush_text(events);
                        events.push(BrainEvent::Issue(ParseIssue::new(report, ParseIssueKind::TooLong, self.spoken)));
                    }
                } else {
                    run.push(c);
                }
            }
            Held::Emoji(name) => {
                if c == ':' && !name.is_empty() {
                    // `:name:` is an emoji name: never spoken.
                    self.held = Held::None;
                } else if (c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
                    && name.chars().count() < EMOJI_MAX_CHARS
                    && !(name.is_empty() && c.is_ascii_digit())
                {
                    name.push(c);
                } else {
                    // Not an emoji: speak the colon and the name as they were, then this character.
                    let name = std::mem::take(name);
                    self.held = Held::None;
                    self.speak(':');
                    for n in name.chars() {
                        self.speak(n);
                    }
                    self.feed(c, events);
                }
            }
            Held::Angle(run) => {
                if c == '>' {
                    // `<think>` opens a reasoning block; any other short markup is dropped.
                    let think = run == "think";
                    self.held = if think { Held::Think(String::new()) } else { Held::None };
                } else if c.is_whitespace() || c == '<' || run.chars().count() >= MARKUP_MAX_CHARS {
                    // Not markup: the words were speech (the `<` itself never is).
                    let run = std::mem::take(run);
                    self.held = Held::None;
                    for r in run.chars() {
                        self.speak(r);
                    }
                    self.feed(c, events);
                } else {
                    run.push(c);
                }
            }
            Held::Think(tail) => {
                tail.push(c);
                if tail.ends_with(THINK_END) {
                    self.held = Held::None;
                } else if tail.len() > 4 * THINK_END.len() {
                    let keep = tail.char_indices().rev().nth(THINK_END.len()).map_or(0, |(i, _)| i);
                    tail.drain(..keep);
                }
            }
            Held::None => match c {
                '[' => {
                    self.held = Held::Tag(String::new());
                }
                ':' => self.held = Held::Emoji(String::new()),
                '<' => self.held = Held::Angle(String::new()),
                '*' | '#' | '`' | ']' => {}
                _ => self.speak(c),
            },
        }
    }

    fn speak(&mut self, c: char) {
        if c.is_whitespace() {
            self.space = self.spoken > 0;
            return;
        }
        if std::mem::take(&mut self.space) {
            self.out.push(' ');
            self.spoken += 1;
        }
        self.out.push(c);
        self.spoken += 1;
    }

    fn flush_text(&mut self, events: &mut Vec<BrainEvent>) {
        if !self.out.is_empty() {
            events.push(BrainEvent::Text(std::mem::take(&mut self.out)));
        }
    }

    fn tag(&mut self, run: &str, events: &mut Vec<BrainEvent>) {
        if run.chars().count() > TAG_MAX_CHARS {
            // Already reported when it grew too long.
            return;
        }
        match parse_tag(run) {
            Ok(Tag::Intent(intent)) => events.push(BrainEvent::intent(intent, self.spoken)),
            Ok(Tag::Style(tags)) => events.push(BrainEvent::Style(tags)),
            Ok(Tag::Pass) => events.push(BrainEvent::Pass),
            Err(kind) => events.push(BrainEvent::Issue(ParseIssue::new(run, kind, self.spoken))),
        }
    }
}

fn cut(run: &str) -> String {
    run.chars().take(TAG_MAX_CHARS).collect()
}

/// A parsed tag.
#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    /// An intent.
    Intent(Intent),
    /// `[style …]`.
    Style(Vec<StyleTag>),
    /// `[pass]`.
    Pass,
}

/// Parses one tag's inside (without the brackets): `emote happy 0.8 hold 2s`, `gesture wave`, `look chat`.
///
/// # Errors
/// [`ParseIssueKind::UnknownVerb`] or [`ParseIssueKind::BadArguments`].
pub fn parse_tag(run: &str) -> Result<Tag, ParseIssueKind> {
    let words: Vec<String> = run.split_whitespace().map(str::to_lowercase).collect();
    let Some((verb, args)) = words.split_first() else { return Err(ParseIssueKind::UnknownVerb) };
    let bad = ParseIssueKind::BadArguments;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let tag = match verb.as_str() {
        "pass" if args.is_empty() => Tag::Pass,
        "pass" => return Err(bad),
        "emote" => {
            let (first, rest) = args.split_first().ok_or(bad)?;
            let expression = ExpressionId::new(*first).map_err(|_| bad)?;
            let (numbers, hold) = split_hold(rest)?;
            let intensity = match numbers {
                [] => 1.0,
                [v] => number(v)?,
                _ => return Err(bad),
            };
            Tag::Intent(Intent::emote(expression, intensity, hold.unwrap_or(EMOTE_HOLD_DEFAULT_S)))
        }
        "gesture" => {
            let clip = ClipId::new(*args.first().ok_or(bad)?).map_err(|_| bad)?;
            let weight = match &args[1..] {
                [] => 1.0,
                [v] => number(v)?,
                _ => return Err(bad),
            };
            Tag::Intent(Intent::gesture(clip, weight))
        }
        "clip" => match args.as_slice() {
            [c] => Tag::Intent(Intent::play_clip(ClipId::new(*c).map_err(|_| bad)?)),
            _ => return Err(bad),
        },
        "look" => {
            let (first, rest) = args.split_first().ok_or(bad)?;
            let target = first.parse::<GazeTarget>().map_err(|_| bad)?;
            let (numbers, hold) = split_hold(rest)?;
            if !numbers.is_empty() {
                return Err(bad);
            }
            Tag::Intent(Intent::look_at(target, hold.unwrap_or(LOOK_HOLD_DEFAULT_S)))
        }
        "move" => match args.as_slice() {
            [p] => Tag::Intent(Intent::move_to(p.parse::<PlaceId>().map_err(|_| bad)?)),
            _ => return Err(bad),
        },
        "style" => {
            if args.is_empty() {
                return Err(bad);
            }
            let tags: Option<Vec<StyleTag>> = args.iter().map(|a| StyleTag::from_name(a)).collect();
            Tag::Style(tags.ok_or(bad)?)
        }
        _ => return Err(ParseIssueKind::UnknownVerb),
    };
    Ok(tag)
}

/// Splits `… hold 2s` off the end of `args`: the words before it, and the hold in seconds.
fn split_hold<'a>(args: &'a [&'a str]) -> Result<(&'a [&'a str], Option<f32>), ParseIssueKind> {
    match args.iter().position(|a| *a == "hold") {
        None => Ok((args, None)),
        Some(i) => match &args[i + 1..] {
            [v] => Ok((&args[..i], Some(number(v.strip_suffix('s').unwrap_or(v))?))),
            _ => Err(ParseIssueKind::BadArguments),
        },
    }
}

fn number(word: &str) -> Result<f32, ParseIssueKind> {
    word.parse::<f32>().ok().filter(|v| v.is_finite()).ok_or(ParseIssueKind::BadArguments)
}

/// Parses a whole reply at once: `(spoken text, events)`, for tests and for replies that arrive in one piece.
pub fn parse_reply(reply: &str) -> (String, Vec<BrainEvent>) {
    let mut parser = TagParser::new();
    let mut events = parser.push(reply);
    events.extend(parser.finish());
    let text = events
        .iter()
        .filter_map(|e| match e {
            BrainEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    (text, events)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn intents(events: &[BrainEvent]) -> Vec<(String, u32)> {
        events
            .iter()
            .filter_map(|e| match e {
                BrainEvent::Intent { intent, at_char, .. } => Some((intent.verb().unwrap_or("?").to_owned(), *at_char)),
                _ => None,
            })
            .collect()
    }

    fn issues(events: &[BrainEvent]) -> Vec<(String, ParseIssueKind)> {
        events
            .iter()
            .filter_map(|e| match e {
                BrainEvent::Issue(i) => Some((i.run.clone(), i.kind)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_worked_example_parses() {
        let (text, events) =
            parse_reply("Hi chat! [emote happy 0.8] [gesture wave] Fold it and see! [dance] [unclosed");
        assert_eq!(text, "Hi chat! Fold it and see!");
        assert_eq!(intents(&events), [("emote".to_owned(), 8), ("gesture".to_owned(), 8)]);
        assert_eq!(
            issues(&events),
            [("dance".to_owned(), ParseIssueKind::UnknownVerb), ("unclosed".to_owned(), ParseIssueKind::Unclosed)]
        );
        let emote = events.iter().find_map(|e| match e {
            BrainEvent::Intent { intent: Intent::Emote { intensity, hold_s, .. }, .. } => Some((*intensity, *hold_s)),
            _ => None,
        });
        assert_eq!(emote, Some((0.8, EMOTE_HOLD_DEFAULT_S)));
    }

    #[test]
    fn every_verb_and_its_arguments() {
        let ok = [
            "emote smug",
            "emote happy 0.5 hold 2s",
            "emote happy hold 3",
            "gesture wave 0.4",
            "clip backflip",
            "look chat hold 2s",
            "look member:bob",
            "move home",
            "move prop:desk",
            "style calm whisper",
            "pass",
            "EMOTE Happy",
        ];
        for run in ok {
            assert!(parse_tag(run).is_ok(), "{run}");
        }
        let bad = [
            ("emote", ParseIssueKind::BadArguments),
            ("emote happy loud", ParseIssueKind::BadArguments),
            ("emote happy 0.5 0.6", ParseIssueKind::BadArguments),
            ("emote happy hold", ParseIssueKind::BadArguments),
            ("gesture", ParseIssueKind::BadArguments),
            ("clip a b", ParseIssueKind::BadArguments),
            ("look ceiling", ParseIssueKind::BadArguments),
            ("look chat 3", ParseIssueKind::BadArguments),
            ("move", ParseIssueKind::BadArguments),
            ("style shouty", ParseIssueKind::BadArguments),
            ("style", ParseIssueKind::BadArguments),
            ("pass now", ParseIssueKind::BadArguments),
            ("dance", ParseIssueKind::UnknownVerb),
            ("", ParseIssueKind::UnknownVerb),
            ("emote happy NaN", ParseIssueKind::BadArguments),
        ];
        for (run, kind) in bad {
            assert_eq!(parse_tag(run), Err(kind), "{run}");
        }
        assert_eq!(parse_tag("style calm whisper"), Ok(Tag::Style(vec![StyleTag::Calm, StyleTag::Whisper])));
        let Ok(Tag::Intent(Intent::Emote { hold_s, intensity, .. })) = parse_tag("emote happy 0.5 hold 2s") else {
            panic!("an emote")
        };
        assert_eq!((intensity, hold_s), (0.5, 2.0));
    }

    #[test]
    fn markdown_emoji_and_spaces_are_not_spoken() {
        let (text, _) = parse_reply("**Wow!**  `really` # yes :smile: it's 3:30, note: fine :) ok");
        assert_eq!(text, "Wow! really yes it's 3:30, note: fine :) ok");
    }

    #[test]
    fn reasoning_blocks_and_markup_are_not_spoken() {
        let reply =
            "<think>\nThe user greets me. [emote sad] I should wave.\n</think>\n\nHi chat! [emote happy] a<br>b, 3 < 4";
        let (text, events) = parse_reply(reply);
        assert_eq!(text, "Hi chat! ab, 3 4");
        assert_eq!(intents(&events), [("emote".to_owned(), 8)]);
        // Split anywhere, the same.
        for at in 0..reply.len() {
            if !reply.is_char_boundary(at) {
                continue;
            }
            let mut p = TagParser::new();
            let mut events = p.push(&reply[..at]);
            events.extend(p.push(&reply[at..]));
            events.extend(p.finish());
            let text: String = events
                .iter()
                .filter_map(|e| if let BrainEvent::Text(t) = e { Some(t.as_str()) } else { None })
                .collect();
            assert_eq!(text, "Hi chat! ab, 3 4", "split at {at}");
        }
        // An unfinished block says nothing.
        assert_eq!(parse_reply("<think>still thinking").0, "");
    }

    #[test]
    fn a_run_over_the_limit_is_dropped_up_to_its_bracket() {
        let long = "x".repeat(TAG_MAX_CHARS + 10);
        let (text, events) = parse_reply(&format!("Before [{long}] after."));
        assert_eq!(text, "Before after.");
        assert_eq!(issues(&events).len(), 1);
        assert_eq!(issues(&events)[0].1, ParseIssueKind::TooLong);
        assert_eq!(issues(&events)[0].0.chars().count(), TAG_MAX_CHARS);
    }

    #[test]
    fn pass_and_style_come_through() {
        let (text, events) = parse_reply("[pass]");
        assert_eq!(text, "");
        assert_eq!(events, [BrainEvent::Pass]);
        let (_, events) = parse_reply("[style excited] Go!");
        assert_eq!(events[0], BrainEvent::Style(vec![StyleTag::Excited]));
    }

    fn split_points(s: &str, cuts: &[usize]) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        let mut points: Vec<usize> = cuts.iter().map(|c| c % (chars.len() + 1)).collect();
        points.sort_unstable();
        let mut out = Vec::new();
        let mut last = 0;
        for p in points.into_iter().chain([chars.len()]) {
            out.push(chars[last..p].iter().collect());
            last = p;
        }
        out
    }

    proptest! {
        /// However the reply is split into deltas, the parser emits the same text, intents and issues, and never
        /// speaks a bracket.
        #[test]
        fn any_split_gives_the_same_result(
            reply in "[a-z !?.:*\\[\\]0-9]{0,120}",
            cuts in proptest::collection::vec(0usize..200, 0..8),
        ) {
            let (whole_text, whole) = parse_reply(&reply);
            let mut parser = TagParser::new();
            let mut events = Vec::new();
            for delta in split_points(&reply, &cuts) {
                events.extend(parser.push(&delta));
            }
            events.extend(parser.finish());
            let text: String = events.iter().filter_map(|e| match e { BrainEvent::Text(t) => Some(t.as_str()), _ => None }).collect();
            prop_assert_eq!(&text, &whole_text);
            prop_assert_eq!(intents(&events), intents(&whole));
            prop_assert_eq!(issues(&events), issues(&whole));
            prop_assert!(!text.contains('[') && !text.contains(']'));
        }
    }
}
