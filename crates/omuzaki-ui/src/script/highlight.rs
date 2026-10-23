//! Rhai highlighting for the console's editor (#1097): comments, strings, numbers, keywords and op calls each in their
//! colour, and the problems `script.check` found underlined at their line and column. The tokens are read by a small
//! scanner here (the editor needs colours every frame, not a parse), so a half-typed script still colours.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};

use crate::ds::theme::Ds;

/// Rhai's keywords and the names it gives meaning to.
const KEYWORDS: &[&str] = &[
    "let", "const", "if", "else", "switch", "do", "while", "until", "loop", "for", "in", "break", "continue", "return",
    "throw", "try", "catch", "fn", "private", "true", "false", "this", "global", "is", "as",
];

/// What a run of text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    /// Plain code: names, operators.
    Plain,
    /// A comment.
    Comment,
    /// A string or character.
    Text,
    /// A number.
    Number,
    /// A keyword.
    Keyword,
    /// The `ops::area::verb` of a call.
    Op,
}

/// The tokens of `source` as byte ranges, in order and covering it whole.
pub fn tokens(source: &str) -> Vec<(std::ops::Range<usize>, Token)> {
    let bytes = source.as_bytes();
    let mut out: Vec<(std::ops::Range<usize>, Token)> = Vec::new();
    let mut push = |range: std::ops::Range<usize>, token: Token| match out.last_mut() {
        Some((last, kind)) if *kind == token && last.end == range.start => last.end = range.end,
        _ => out.push((range, token)),
    };
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            push(start..i, Token::Comment);
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            push(start..i, Token::Comment);
        } else if b == b'"' || b == b'`' || b == b'\'' {
            i += 1;
            while i < bytes.len() && bytes[i] != b && !(b == b'"' && bytes[i] == b'\n') {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(bytes.len());
            push(start..i, Token::Text);
        } else if b.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_') {
                i += 1;
            }
            push(start..i, Token::Number);
        } else if b.is_ascii_alphabetic() || b == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &source[start..i];
            if word == "ops" && source[i..].starts_with("::") {
                // `ops::area::verb`, as far as the path goes.
                while source[i..].starts_with("::") {
                    i += 2;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                        i += 1;
                    }
                }
                push(start..i, Token::Op);
            } else if KEYWORDS.contains(&word) {
                push(start..i, Token::Keyword);
            } else {
                push(start..i, Token::Plain);
            }
        } else {
            // One character, whole: a multi-byte one is never split.
            i += source[i..].chars().next().map_or(1, char::len_utf8);
            push(start..i, Token::Plain);
        }
    }
    out
}

/// The byte range of the problem at 1-based `line` and `column` (in characters): the name or token starting there, or
/// one character; `None` past the end.
pub fn problem_range(source: &str, line: u32, column: u32) -> Option<std::ops::Range<usize>> {
    let line_start =
        if line <= 1 { 0 } else { source.match_indices('\n').nth(line as usize - 2).map(|(at, _)| at + 1)? };
    let text = &source[line_start..];
    let line_text = text.split('\n').next().unwrap_or_default();
    let skip = column.saturating_sub(1) as usize;
    let start = line_start + line_text.char_indices().nth(skip).map_or(line_text.len(), |(at, _)| at);
    let rest = &source[start..];
    let word =
        rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':').map(char::len_utf8).sum();
    let len = if word > 0 { word } else { rest.chars().next().map_or(0, char::len_utf8) };
    // A problem at the very end (a missing closing brace) marks the last character.
    if len == 0 {
        let back = source[..start].chars().next_back().map_or(0, char::len_utf8);
        return (back > 0).then(|| start - back..start);
    }
    Some(start..start + len)
}

/// The colour of `token` in `ds`'s theme.
fn colour(ds: &Ds, token: Token) -> Color32 {
    let t = &ds.theme;
    match token {
        Token::Plain => t.text,
        Token::Comment => t.text2,
        Token::Text => t.ok,
        Token::Number => t.warn,
        Token::Keyword => t.accent,
        Token::Op => t.accent_text,
    }
}

/// The editor's text laid out: coloured, with `problems` (byte ranges) underlined, wrapped at `wrap`.
pub fn layout(ds: &Ds, source: &str, font: &FontId, problems: &[std::ops::Range<usize>], wrap: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    let underline = Stroke::new(1.5, ds.theme.danger);
    for (range, token) in tokens(source) {
        // Split the token where a problem starts or ends, so only the problem is underlined.
        let mut cuts = vec![range.start, range.end];
        for p in problems {
            for at in [p.start, p.end] {
                if range.start < at && at < range.end && source.is_char_boundary(at) {
                    cuts.push(at);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let marked = problems.iter().any(|p| p.start <= a && b <= p.end && p.start < p.end);
            let format = TextFormat {
                font_id: font.clone(),
                color: colour(ds, token),
                underline: if marked { underline } else { Stroke::NONE },
                ..TextFormat::default()
            };
            job.append(&source[a..b], 0.0, format);
        }
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(&str, Token)> {
        tokens(source).into_iter().map(|(r, t)| (&source[r], t)).collect()
    }

    #[test]
    fn calls_keywords_strings_numbers_and_comments_are_told_apart() {
        let got = kinds("let x = ops::face::set_blush(#{ amount: 0.5, note: \"a // b\" }); // done");
        assert!(got.contains(&("let", Token::Keyword)));
        assert!(got.contains(&("ops::face::set_blush", Token::Op)));
        assert!(got.contains(&("0.5", Token::Number)));
        assert!(got.contains(&("\"a // b\"", Token::Text)), "a comment marker inside a string is text");
        assert!(got.contains(&("// done", Token::Comment)));
        let whole: String = got.iter().map(|(s, _)| *s).collect();
        assert_eq!(whole.len(), "let x = ops::face::set_blush(#{ amount: 0.5, note: \"a // b\" }); // done".len());
    }

    #[test]
    fn an_unclosed_string_or_comment_stops_at_the_end() {
        assert_eq!(kinds("\"abc"), [("\"abc", Token::Text)]);
        assert_eq!(kinds("/* abc"), [("/* abc", Token::Comment)]);
        assert_eq!(kinds("é + 1").len(), 2, "a multi-byte character stays whole");
    }

    #[test]
    fn a_problem_marks_the_name_at_its_line_and_column() {
        let source = "let a = 1;\nops::face::nope(#{});\n";
        let range = problem_range(source, 2, 1).unwrap_or_else(|| panic!("in range"));
        assert_eq!(&source[range], "ops::face::nope");
        let range = problem_range(source, 1, 9).unwrap_or_else(|| panic!("in range"));
        assert_eq!(&source[range], "1");
        assert_eq!(problem_range(source, 9, 1), None);
        let end = "let a = #{";
        let range = problem_range(end, 1, 11).unwrap_or_else(|| panic!("at the end"));
        assert_eq!(&end[range], "{", "a problem past the last character marks the last one");
    }
}
