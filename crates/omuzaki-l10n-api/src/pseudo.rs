//! The pseudo-locale transform (`en-XA`): accented look-alikes and 40% more length, so a hard-coded string stands out
//! as plain ASCII and a layout that cannot take longer text breaks in front of a developer, not a translator.
//!
//! Moved here from `omuzaki-ui`'s `ds::strings`, which re-exports both functions: the catalog and the design
//! system use the same transform.

/// `text` as the pseudo-locale writes it: `[Ápprövé·····]`.
///
/// Letters become accented look-alikes, `{placeholders}` stay as they are (the caller fills them with real values),
/// and a tail of middle dots makes the string 40% longer. The brackets show where the text ends: a closing bracket
/// that is missing on screen means the label was cut.
pub fn pseudo(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2 + 4);
    out.push('[');
    let (mut letters, mut in_placeholder) = (0usize, false);
    for c in text.chars() {
        match c {
            '{' => {
                in_placeholder = true;
                out.push(c);
            }
            '}' => {
                in_placeholder = false;
                out.push(c);
            }
            _ if in_placeholder => out.push(c),
            _ => {
                if c.is_alphabetic() {
                    letters += 1;
                }
                out.push(accent(c));
            }
        }
    }
    out.extend(std::iter::repeat_n('·', (letters * 2).div_ceil(5)));
    out.push(']');
    out
}

/// A look-alike of an ASCII letter that has an accent; anything else is returned as it is.
///
/// Every letter used here is in the three bundled text faces (a test checks), so the pseudo-locale never shows a box.
/// Only some letters have an accented form in them (Instrument Sans stops at Latin-1 and part of Latin Extended-A), so
/// the rest borrow the nearest accented letter that does: the pseudo-locale is for spotting hard-coded text and
/// layouts, not for reading.
pub const fn accent(c: char) -> char {
    match c {
        'a' => 'á',
        'b' => 'þ',
        'c' => 'ç',
        'd' => 'ď',
        'e' => 'é',
        'f' => 'ø',
        'g' => 'ğ',
        'h' => 'ħ',
        'i' => 'í',
        'j' => 'ȷ',
        'k' => 'ķ',
        'l' => 'ļ',
        'm' => 'ŋ',
        'n' => 'ñ',
        'o' => 'ö',
        'p' => 'ð',
        'q' => 'œ',
        'r' => 'ŕ',
        's' => 'š',
        't' => 'ť',
        'u' => 'ü',
        'v' => 'ů',
        'w' => 'ŵ',
        'x' => 'ś',
        'y' => 'ý',
        'z' => 'ž',
        'A' => 'Å',
        'B' => 'Þ',
        'C' => 'Ç',
        'D' => 'Ď',
        'E' => 'É',
        'F' => 'Ø',
        'G' => 'Ğ',
        'H' => 'Ħ',
        'I' => 'Í',
        'J' => 'Ĺ',
        'K' => 'Ķ',
        'L' => 'Ļ',
        'M' => 'Ŋ',
        'N' => 'Ñ',
        'O' => 'Ö',
        'P' => 'Ð',
        'Q' => 'Œ',
        'R' => 'Ŕ',
        'S' => 'Š',
        'T' => 'Ť',
        'U' => 'Ü',
        'V' => 'Ů',
        'W' => 'Ŵ',
        'X' => 'Ś',
        'Y' => 'Ý',
        'Z' => 'Ž',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pseudo_locale_accents_expands_and_keeps_placeholders() {
        let text = pseudo("Reset {label}");
        assert!(text.starts_with('[') && text.ends_with(']'));
        assert!(text.contains("{label}"), "placeholders survive: {text}");
        let outside_placeholders = text.replace("{label}", "");
        assert!(
            !outside_placeholders.chars().filter(|c| c.is_alphabetic()).any(|c| c.is_ascii()),
            "no plain ASCII letters: {text}"
        );
        let plain = "Approve for this session";
        let long = pseudo(plain);
        assert!(long.chars().count() * 5 >= plain.chars().count() * 7, "{long}");
    }

    #[test]
    fn every_ascii_letter_has_a_different_accented_look_alike() {
        for c in ('a'..='z').chain('A'..='Z') {
            let accented = accent(c);
            assert!(!accented.is_ascii() && accented.is_alphabetic(), "{c} maps to {accented}");
        }
    }
}
