//! The small slice of regular expressions a form field's `pattern` needs (slugs, hex colours, identifiers).
//!
//! The workspace has no regex engine and a form must not grow one for `^[a-z0-9-]+$`. This matcher understands
//! literals, `.`, classes (`[a-z0-9_-]`, negated, with `\d \w \s`), the quantifiers `* + ? {n} {n,} {n,m}` and the
//! anchors `^` and `$`. Anything else (groups, alternation, look-around) is refused by [`Pattern::parse`], and the form
//! then shows the field read-only instead of guessing. Matching is by backtracking over `&str` slices and allocates
//! nothing, so it can run every frame.

/// One thing a piece can match.
#[derive(Clone, Debug)]
enum Atom {
    Char(char),
    Any,
    Class { negated: bool, items: Vec<Item> },
}

/// One member of a class.
#[derive(Clone, Copy, Debug)]
enum Item {
    Range(char, char),
    Digit,
    Word,
    Space,
}

/// An atom and how many times it repeats.
#[derive(Clone, Debug)]
struct Piece {
    atom: Atom,
    min: usize,
    max: usize,
}

/// A parsed `pattern`.
#[derive(Clone, Debug)]
pub(super) struct Pattern {
    source: String,
    anchored_start: bool,
    anchored_end: bool,
    pieces: Vec<Piece>,
}

impl Item {
    fn matches(self, c: char) -> bool {
        match self {
            Self::Range(lo, hi) => (lo..=hi).contains(&c),
            Self::Digit => c.is_ascii_digit(),
            Self::Word => c.is_ascii_alphanumeric() || c == '_',
            Self::Space => c.is_whitespace(),
        }
    }
}

impl Atom {
    fn matches(&self, c: char) -> bool {
        match self {
            Self::Char(expected) => *expected == c,
            Self::Any => true,
            Self::Class { negated, items } => items.iter().any(|i| i.matches(c)) != *negated,
        }
    }
}

impl Pattern {
    /// The pattern as written, for the message under a field that does not match it.
    pub(super) fn source(&self) -> &str {
        &self.source
    }

    /// Parses `source`, or `None` when it uses syntax this matcher does not implement.
    pub(super) fn parse(source: &str) -> Option<Self> {
        let mut chars = source.chars().peekable();
        let anchored_start = chars.next_if_eq(&'^').is_some();
        let mut pieces: Vec<Piece> = Vec::new();
        let mut anchored_end = false;
        while let Some(c) = chars.next() {
            let atom = match c {
                '$' if chars.peek().is_none() => {
                    anchored_end = true;
                    break;
                }
                '.' => Atom::Any,
                '[' => Self::parse_class(&mut chars)?,
                '\\' => match chars.next()? {
                    'd' => Atom::Class { negated: false, items: vec![Item::Digit] },
                    'w' => Atom::Class { negated: false, items: vec![Item::Word] },
                    's' => Atom::Class { negated: false, items: vec![Item::Space] },
                    escaped if !escaped.is_ascii_alphanumeric() => Atom::Char(escaped),
                    _ => return None,
                },
                '(' | ')' | '|' | '^' | '$' | '*' | '+' | '?' | '{' | '}' | ']' => return None,
                literal => Atom::Char(literal),
            };
            let (min, max) = match chars.peek() {
                Some('*') => (0, usize::MAX),
                Some('+') => (1, usize::MAX),
                Some('?') => (0, 1),
                Some('{') => {
                    chars.next();
                    let bounds = Self::parse_braces(&mut chars)?;
                    pieces.push(Piece { atom, min: bounds.0, max: bounds.1 });
                    continue;
                }
                _ => (1, 1),
            };
            if min != 1 || max != 1 {
                chars.next();
            }
            pieces.push(Piece { atom, min, max });
        }
        Some(Self { source: source.to_owned(), anchored_start, anchored_end, pieces })
    }

    fn parse_class(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<Atom> {
        let negated = chars.next_if_eq(&'^').is_some();
        let mut items = Vec::new();
        loop {
            let c = chars.next()?;
            let lo = match c {
                ']' if !items.is_empty() => return Some(Atom::Class { negated, items }),
                '\\' => match chars.next()? {
                    'd' => {
                        items.push(Item::Digit);
                        continue;
                    }
                    'w' => {
                        items.push(Item::Word);
                        continue;
                    }
                    's' => {
                        items.push(Item::Space);
                        continue;
                    }
                    escaped if !escaped.is_ascii_alphanumeric() => escaped,
                    _ => return None,
                },
                other => other,
            };
            // `a-z` is a range unless the dash is the last thing in the class (`[a-]`).
            let mut ahead = chars.clone();
            if ahead.next() == Some('-') && !matches!(ahead.peek(), Some(']') | None) {
                chars.next();
                let hi = chars.next()?;
                if hi < lo {
                    return None;
                }
                items.push(Item::Range(lo, hi));
            } else {
                items.push(Item::Range(lo, lo));
            }
        }
    }

    fn parse_braces(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<(usize, usize)> {
        let mut text = String::new();
        loop {
            match chars.next()? {
                '}' => break,
                c if c.is_ascii_digit() || c == ',' => text.push(c),
                _ => return None,
            }
        }
        let (min, max) = match text.split_once(',') {
            None => {
                let n = text.parse().ok()?;
                (n, n)
            }
            Some((lo, "")) => (lo.parse().ok()?, usize::MAX),
            Some((lo, hi)) => (lo.parse().ok()?, hi.parse().ok()?),
        };
        (min <= max).then_some((min, max))
    }

    /// Whether `text` matches: the whole string when the pattern is anchored at both ends, a substring otherwise,
    /// as JSON Schema's `pattern` is.
    pub(super) fn is_match(&self, text: &str) -> bool {
        if self.anchored_start {
            return self.match_from(0, text);
        }
        let mut rest = text;
        loop {
            if self.match_from(0, rest) {
                return true;
            }
            let mut chars = rest.chars();
            if chars.next().is_none() {
                return false;
            }
            rest = chars.as_str();
        }
    }

    fn match_from(&self, index: usize, text: &str) -> bool {
        let Some(piece) = self.pieces.get(index) else {
            return !self.anchored_end || text.is_empty();
        };
        // Greedy: take as many as allowed, then give them back one at a time.
        let mut taken = 0;
        let mut ends = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len()));
        let mut at = ends.next().unwrap_or(0);
        for c in text.chars() {
            if taken == piece.max || !piece.atom.matches(c) {
                break;
            }
            taken += 1;
            at = ends.next().unwrap_or(text.len());
        }
        loop {
            if taken >= piece.min && self.match_from(index + 1, &text[at..]) {
                return true;
            }
            if taken == 0 || taken <= piece.min {
                return false;
            }
            taken -= 1;
            at = text.char_indices().nth(taken).map_or(text.len(), |(i, _)| i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Pattern;

    fn matches(pattern: &str, text: &str) -> bool {
        Pattern::parse(pattern).is_some_and(|p| p.is_match(text))
    }

    #[test]
    fn a_slug_pattern_accepts_slugs_and_rejects_the_rest() {
        let slug = "^[a-z0-9]+(-[a-z0-9]+)*$";
        assert!(Pattern::parse(slug).is_none(), "groups are not supported, so the field is shown read-only");
        let flat = "^[a-z0-9-]{1,32}$";
        assert!(matches(flat, "front-hair-2"));
        assert!(!matches(flat, "Front Hair"));
        assert!(!matches(flat, ""));
        assert!(!matches(flat, &"a".repeat(33)));
    }

    #[test]
    fn anchors_decide_between_the_whole_string_and_a_substring() {
        assert!(matches("^ab$", "ab"));
        assert!(!matches("^ab$", "xab"));
        assert!(matches("ab", "xaby"));
        assert!(matches("^ab", "abz"));
        assert!(!matches("ab$", "abz"));
    }

    #[test]
    fn quantifiers_backtrack() {
        assert!(matches("^a*ab$", "aaab"));
        assert!(matches("^a?a$", "a"));
        assert!(matches("^#[0-9a-fA-F]{6}$", "#00ffAA"));
        assert!(!matches("^#[0-9a-fA-F]{6}$", "#00ff"));
        assert!(matches("^\\d{2,}$", "123"));
        assert!(!matches("^\\d{2,}$", "1"));
    }

    #[test]
    fn negated_classes_dots_and_escapes() {
        assert!(matches("^[^/]+$", "a.b"));
        assert!(!matches("^[^/]+$", "a/b"));
        assert!(matches("^a\\.b$", "a.b"));
        assert!(!matches("^a\\.b$", "axb"));
        assert!(matches("^.{3}$", "héé"));
        assert!(matches("^[a-]+$", "a-a"));
    }

    #[test]
    fn syntax_it_cannot_evaluate_is_refused_not_guessed() {
        for unsupported in ["(a|b)", "a|b", "\\bword", "a{2", "[z-a]", "[abc", "a{3,1}", "^*"] {
            assert!(Pattern::parse(unsupported).is_none(), "{unsupported}");
        }
    }
}
