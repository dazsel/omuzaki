//! Text written into the book: a plan item, a rationale, a gate's message, a note. Each one reaches a person's screen
//! and, for notes, an agent's context, so it is cleaned on the way in (ADR 0056 §6): control characters, bidirectional
//! overrides and Unicode tag characters go, whitespace runs fold to one space, and it is capped.

use omuzaki_ops_api::OpError;

/// The longest text the book keeps, in characters (as a review comment, ADR 0056 §3).
pub const MAX_TEXT: usize = 4000;
/// The longest plan item, in characters.
pub const MAX_ITEM: usize = 200;
/// The most items a plan holds.
pub const MAX_ITEMS: usize = 100;
/// The longest plan item id, in characters.
pub const MAX_ID: usize = 64;

/// Whether `c` is dropped: a control character, a bidirectional control, a Unicode tag character or a zero-width
/// character that can hide text.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
        || ('\u{E0000}'..='\u{E007F}').contains(&c)
}

/// `text` cleaned and checked: not empty once cleaned, at most `max` characters.
///
/// # Errors
/// `Invalid` on `field` when the text is empty or too long.
pub fn clean(field: &str, text: &str, max: usize) -> Result<String, OpError> {
    let mut out = String::with_capacity(text.len().min(max * 4));
    for word in text.split(|c: char| c.is_whitespace()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().filter(|&c| !hidden(c)));
    }
    let out = out.trim().to_owned();
    if out.is_empty() {
        return Err(OpError::invalid(field, "say it in words: the text is empty"));
    }
    let count = out.chars().count();
    if count > max {
        return Err(OpError::invalid(field, format!("{count} characters is more than {max}")));
    }
    Ok(out)
}

/// A plan item id: letters, digits, `-`, `_` and `.`, at most [`MAX_ID`].
///
/// # Errors
/// `Invalid` on `field` otherwise.
pub fn id(field: &str, id: &str) -> Result<String, OpError> {
    let ok = !id.is_empty()
        && id.chars().count() <= MAX_ID
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(id.to_owned())
    } else {
        Err(OpError::invalid(field, format!("an id of letters, digits, '-', '_' or '.', at most {MAX_ID} long")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_characters_go_and_whitespace_folds() {
        let cleaned = clean("text", "make\u{202E} this\n\n rounder\u{E0041}\u{200B}", MAX_TEXT);
        assert_eq!(cleaned.ok().as_deref(), Some("make this rounder"));
    }

    #[test]
    fn empty_and_long_text_are_refused_by_field() {
        assert_eq!(clean("text", " \u{200B}\n", MAX_TEXT).err().map(|e| e.message.contains("empty")), Some(true));
        let long = "a".repeat(MAX_ITEM + 1);
        assert!(clean("items", &long, MAX_ITEM).is_err());
        assert!(clean("items", &long, MAX_TEXT).is_ok());
    }

    #[test]
    fn ids_are_plain() {
        assert!(id("id", "face-1.oval").is_ok());
        assert!(id("id", "face 1").is_err());
        assert!(id("id", "").is_err());
    }
}
