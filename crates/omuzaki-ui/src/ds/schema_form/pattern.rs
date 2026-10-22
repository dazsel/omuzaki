//! A form field's `pattern`, checked with the `regex` crate.
//!
//! JSON Schema patterns are ECMA-262 regular expressions. `regex` reads the same syntax for everything an operation
//! publishes (classes, groups, alternation, counted repeats, anchors) and matches in time linear in the text, so an
//! ambiguous pattern cannot stall a frame. What it does not implement (look-around, back-references) is refused by
//! [`Pattern::parse`], and the form then shows the field read-only instead of guessing.

/// The most compiled program a schema's pattern may build: far above any field's pattern, small enough that a hostile
/// schema cannot make the form allocate without bound.
const SIZE_LIMIT: usize = 1 << 20;

/// A parsed `pattern`.
#[derive(Clone, Debug)]
pub(super) struct Pattern {
    source: String,
    regex: regex::Regex,
}

impl Pattern {
    /// The pattern as written, for the message under a field that does not match it.
    pub(super) fn source(&self) -> &str {
        &self.source
    }

    /// Parses `source`, or `None` when it uses syntax the matcher does not implement.
    pub(super) fn parse(source: &str) -> Option<Self> {
        let regex = regex::RegexBuilder::new(source).size_limit(SIZE_LIMIT).build().ok()?;
        Some(Self { source: source.to_owned(), regex })
    }

    /// Whether `text` matches: the whole string when the pattern is anchored at both ends, a substring otherwise,
    /// as JSON Schema's `pattern` is.
    pub(super) fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text)
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
        assert!(matches(slug, "front-hair-2"));
        assert!(!matches(slug, "front--hair"));
        assert!(!matches(slug, "Front Hair"));
        assert!(!matches(slug, ""));
    }

    #[test]
    fn a_colour_or_a_palette_reference() {
        let colour = "^(#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?|@[a-z0-9][a-z0-9_-]{0,63})$";
        for good in ["#00ffAA", "#00ffaa80", "@skin", "@hair-2"] {
            assert!(matches(colour, good), "{good}");
        }
        for bad in ["#00ff", "skin", "@", "@Skin", "#00ffaa8"] {
            assert!(!matches(colour, bad), "{bad}");
        }
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
    fn an_ambiguous_pattern_is_linear_not_exponential() {
        let pieces = "a*".repeat(60);
        let pattern = Pattern::parse(&format!("^{pieces}b$")).unwrap_or_else(|| panic!("a pattern"));
        let text = format!("{}c", "a".repeat(20_000));
        let start = std::time::Instant::now();
        assert!(!pattern.is_match(&text));
        assert!(pattern.is_match(&format!("{}b", "a".repeat(20_000))));
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "took {:?}", start.elapsed());
    }

    #[test]
    fn syntax_it_cannot_evaluate_is_refused_not_guessed() {
        for unsupported in ["(?=a)b", "(a)\\1", "a{2", "[z-a]", "[abc", "a{3,1}"] {
            assert!(Pattern::parse(unsupported).is_none(), "{unsupported}");
        }
    }
}
