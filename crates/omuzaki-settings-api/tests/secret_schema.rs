//! The `SecretRef` schema and constructor accept the same labels. A Settings form or an agent validates against the
//! schema; the service then runs the constructor, so any label only one of them accepts is a rejection the schema
//! promised would not happen (or a label the constructor takes that the schema forbids).
//!
//! The pattern is checked by a small matcher below, because the workspace has no regex crate and adding one is a
//! workspace-manifest change outside this crate. It reads the subset the pattern uses (`^ $`, literals, escapes,
//! classes with ranges, `(?:...|...)`, `* + ? {n} {n,} {n,m}`) and nothing else, and it is itself tested.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::panic)]

use omuzaki_settings_api::secret::SecretRef;
use schemars::JsonSchema;

enum Node {
    Char(char),
    Class(Vec<(char, char)>),
    Group(Vec<Vec<Item>>),
}

struct Item {
    node: Node,
    min: usize,
    max: usize,
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Parser<'_> {
    fn alternatives(&mut self) -> Vec<Vec<Item>> {
        let mut alternatives = vec![Vec::new()];
        while let Some(&c) = self.chars.peek() {
            match c {
                ')' => break,
                '|' => {
                    self.chars.next();
                    alternatives.push(Vec::new());
                }
                _ => {
                    let item = self.item();
                    alternatives.last_mut().unwrap().push(item);
                }
            }
        }
        alternatives
    }

    fn item(&mut self) -> Item {
        let node = match self.chars.next().unwrap() {
            '(' => {
                assert_eq!(self.chars.next(), Some('?'));
                assert_eq!(self.chars.next(), Some(':'));
                let group = self.alternatives();
                assert_eq!(self.chars.next(), Some(')'));
                Node::Group(group)
            }
            '[' => {
                let mut ranges = Vec::new();
                while let Some(c) = self.chars.next() {
                    if c == ']' {
                        break;
                    }
                    let mut look = self.chars.clone();
                    if look.next() == Some('-') && look.peek().is_some_and(|&end| end != ']') {
                        self.chars.next();
                        ranges.push((c, self.chars.next().unwrap()));
                    } else {
                        ranges.push((c, c));
                    }
                }
                Node::Class(ranges)
            }
            '\\' => Node::Char(self.chars.next().unwrap()),
            c => Node::Char(c),
        };
        let (min, max) = match self.chars.peek() {
            Some('*') => (0, usize::MAX),
            Some('+') => (1, usize::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                self.chars.next();
                let body: String = self.chars.by_ref().take_while(|&c| c != '}').collect();
                let (lo, hi) = body.split_once(',').map_or((body.as_str(), Some(body.as_str())), |(lo, hi)| {
                    (lo, if hi.is_empty() { None } else { Some(hi) })
                });
                let lo: usize = lo.parse().unwrap();
                return Item { node, min: lo, max: hi.map_or(usize::MAX, |h| h.parse().unwrap()) };
            }
            _ => return Item { node, min: 1, max: 1 },
        };
        self.chars.next();
        Item { node, min, max }
    }
}

type Cont<'a> = &'a dyn Fn(usize) -> bool;

fn seq(items: &[Item], text: &[char], pos: usize, k: Cont<'_>) -> bool {
    match items.split_first() {
        None => k(pos),
        Some((item, rest)) => rep(item, 0, text, pos, &|p| seq(rest, text, p, k)),
    }
}

fn rep(item: &Item, count: usize, text: &[char], pos: usize, k: Cont<'_>) -> bool {
    // Greedy, and an empty repetition is not repeated again, so the matcher terminates.
    let more = count < item.max
        && node(&item.node, text, pos, &|p| (p != pos || count < item.min) && rep(item, count + 1, text, p, k));
    more || (count >= item.min && k(pos))
}

fn node(n: &Node, text: &[char], pos: usize, k: Cont<'_>) -> bool {
    match n {
        Node::Char(c) => text.get(pos) == Some(c) && k(pos + 1),
        Node::Class(ranges) => {
            text.get(pos).is_some_and(|c| ranges.iter().any(|(lo, hi)| (lo..=hi).contains(&c))) && k(pos + 1)
        }
        Node::Group(alternatives) => alternatives.iter().any(|alt| seq(alt, text, pos, k)),
    }
}

/// Whether `text` matches the whole of an anchored pattern `^...$`.
fn matches(pattern: &str, text: &str) -> bool {
    let body = pattern.strip_prefix('^').and_then(|p| p.strip_suffix('$')).ok_or("an anchored pattern").unwrap();
    let mut parser = Parser { chars: body.chars().peekable() };
    let alternatives = parser.alternatives();
    assert!(parser.chars.next().is_none(), "unbalanced `)` in {pattern}");
    let chars: Vec<char> = text.chars().collect();
    let end = chars.len();
    alternatives.iter().any(|alt| seq(alt, &chars, 0, &|p| p == end))
}

#[test]
fn the_matcher_reads_the_constructs_the_pattern_uses() {
    assert!(matches("^a+(?:/b{2,3})*$", "aa/bb/bbb"));
    assert!(!matches("^a+(?:/b{2,3})*$", "aa/b"));
    assert!(matches("^[a-z0-9_-]\\.{0,2}$", "-.."));
    assert!(!matches("^[a-z0-9_-]\\.{0,2}$", "-..."));
    assert!(matches("^(?:x|yy)?$", "") && matches("^(?:x|yy)?$", "yy") && !matches("^(?:x|yy)?$", "y"));
}

fn schema() -> (String, usize) {
    let json = serde_json::to_value(schemars::schema_for!(SecretRef)).unwrap();
    let pattern = json["pattern"].as_str().unwrap().to_owned();
    let max = usize::try_from(json["maxLength"].as_u64().unwrap()).unwrap();
    (pattern, max)
}

fn schema_accepts(label: &str) -> bool {
    let (pattern, max_length) = schema();
    label.chars().count() <= max_length && matches(&pattern, label)
}

#[test]
fn the_schema_and_the_constructor_agree_on_a_table_of_labels() {
    let long = "x".repeat(SecretRef::MAX_LEN);
    let too_long = "x".repeat(SecretRef::MAX_LEN + 1);
    let table: [(&str, bool); 27] = [
        ("connector/openai", true),
        ("a", true),
        ("a.b-c_d/e", true),
        (".hidden", true),
        ("..hidden", true),
        ("a..", true),
        ("...", true),
        ("a/.../b", true),
        ("-", true),
        ("", false),
        (".", false),
        ("..", false),
        ("a/./b", false),
        ("a/../b", false),
        ("../a", false),
        ("a/..", false),
        ("/a", false),
        ("a/", false),
        ("a//b", false),
        ("A", false),
        ("a b", false),
        ("sk-Abc", false),
        ("é", false),
        ("a\\b", false),
        ("a\nb", false),
        (long.as_str(), true),
        (too_long.as_str(), false),
    ];
    for (label, valid) in table {
        let shown: String = label.chars().take(24).collect();
        assert_eq!(SecretRef::new(label).is_ok(), valid, "constructor on {shown:?}");
        assert_eq!(schema_accepts(label), valid, "schema on {shown:?}");
    }
}

#[test]
fn every_short_label_over_the_interesting_alphabet_is_accepted_by_both_or_neither() {
    // Every string of up to five characters over the characters that matter: a letter, a digit, the three punctuation
    // marks, the separator, and two that are never valid. That covers each dot-segment shape.
    let alphabet = ['a', '0', '-', '_', '.', '/', 'A', ' '];
    let mut labels = vec![String::new()];
    let mut frontier = labels.clone();
    for _ in 0..5 {
        frontier = frontier.iter().flat_map(|s| alphabet.iter().map(move |c| format!("{s}{c}"))).collect();
        labels.extend(frontier.iter().cloned());
    }
    let (pattern, max_length) = schema();
    let mut accepted = 0;
    for label in &labels {
        let schema = label.chars().count() <= max_length && matches(&pattern, label);
        assert_eq!(SecretRef::new(label.as_str()).is_ok(), schema, "constructor and schema disagree on {label:?}");
        accepted += usize::from(schema);
    }
    assert!(labels.len() > 37_000 && accepted > 1_000, "the enumeration was not vacuous: {accepted}/{}", labels.len());
}

#[test]
fn the_schema_describes_a_string_of_bounded_length() {
    let json = serde_json::to_value(schemars::schema_for!(SecretRef)).unwrap();
    assert_eq!(json["type"], "string");
    assert_eq!(json["maxLength"], SecretRef::MAX_LEN);
    assert_eq!(json["pattern"], SecretRef::pattern());
    assert_eq!(<SecretRef as JsonSchema>::schema_name(), "SecretRef");
}
