//! What a numeric field accepts when you type into it (`design-system.md` section 7.1): a number, a unit, a percent
//! and simple arithmetic such as `0.5*1.2`, `(1+2)/4` or `30%`.

/// Why typed text could not be read as a number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// Nothing was typed.
    Empty,
    /// The text is not a number or an arithmetic expression.
    NotANumber,
    /// The expression divides by zero.
    DivideByZero,
}

/// `text` with full-width forms (`１２．５`, `＋`, `（`) turned into their ASCII twins, so a number typed through
/// an IME in full-width mode reads as the number it looks like.
fn halfwidth(text: &str) -> std::borrow::Cow<'_, str> {
    let wide = |c: char| ('\u{FF01}'..='\u{FF5E}').contains(&c) || c == '\u{3000}';
    if !text.chars().any(wide) {
        return std::borrow::Cow::Borrowed(text);
    }
    text.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            // The full-width block U+FF01..=U+FF5E is ASCII U+0021..=U+007E shifted by 0xFEE0.
            c if wide(c) => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
            c => c,
        })
        .collect::<String>()
        .into()
}

/// Reads `input` as a value in `min..=max`.
///
/// - Full-width forms from an IME (`１２．５`, `＋`, `％`) are read as their ASCII twins ([`halfwidth`]).
/// - Arithmetic: `+ - * /` and parentheses.
/// - A trailing `unit` (case-insensitive, such as `°` or `px`) is ignored, so a user may type what they see.
/// - A trailing `%` is a position along the range, so `30%` on `0..=1` is `0.3` and on `10..=20` is `13`.
///
/// # Errors
/// Returns why the text is not a number; the caller shows "Enter a number from 0 to 1".
pub fn parse(input: &str, unit: &str, min: f64, max: f64) -> Result<f64, ParseError> {
    let input = halfwidth(input);
    let mut text = input.trim();
    if text.is_empty() {
        return Err(ParseError::Empty);
    }
    if !unit.is_empty()
        && let Some(head) = strip_suffix_ignore_case(text, unit)
    {
        text = head.trim_end();
    }
    let (text, percent) = text.strip_suffix('%').map_or((text, false), |head| (head.trim_end(), true));
    let mut parser = Parser { rest: text.as_bytes() };
    let value = parser.expr()?;
    parser.skip_spaces();
    if !parser.rest.is_empty() || !value.is_finite() {
        return Err(ParseError::NotANumber);
    }
    Ok(if percent { min + value / 100.0 * (max - min) } else { value })
}

fn strip_suffix_ignore_case<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let split = text.len().checked_sub(suffix.len())?;
    let (head, tail) = (text.get(..split)?, text.get(split..)?);
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

/// Recursive descent over the bytes of the input: expr = term {(+|-) term}; term = factor {(*|/) factor}.
struct Parser<'a> {
    rest: &'a [u8],
}

impl Parser<'_> {
    fn skip_spaces(&mut self) {
        while let [b' ', tail @ ..] = self.rest {
            self.rest = tail;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.skip_spaces();
        if let [first, tail @ ..] = self.rest
            && *first == byte
        {
            self.rest = tail;
            return true;
        }
        false
    }

    fn expr(&mut self) -> Result<f64, ParseError> {
        let mut value = self.term()?;
        loop {
            if self.eat(b'+') {
                value += self.term()?;
            } else if self.eat(b'-') {
                value -= self.term()?;
            } else {
                return Ok(value);
            }
        }
    }

    fn term(&mut self) -> Result<f64, ParseError> {
        let mut value = self.factor()?;
        loop {
            if self.eat(b'*') {
                value *= self.factor()?;
            } else if self.eat(b'/') {
                let divisor = self.factor()?;
                if divisor == 0.0 {
                    return Err(ParseError::DivideByZero);
                }
                value /= divisor;
            } else {
                return Ok(value);
            }
        }
    }

    fn factor(&mut self) -> Result<f64, ParseError> {
        if self.eat(b'-') {
            return self.factor().map(|v| -v);
        }
        if self.eat(b'+') {
            return self.factor();
        }
        if self.eat(b'(') {
            let value = self.expr()?;
            return if self.eat(b')') { Ok(value) } else { Err(ParseError::NotANumber) };
        }
        self.number()
    }

    fn number(&mut self) -> Result<f64, ParseError> {
        self.skip_spaces();
        let len = self.rest.iter().take_while(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E')).count();
        // `1e-3` has a sign inside the number; accept it only directly after an exponent marker.
        let len = extend_exponent_sign(self.rest, len);
        let (digits, tail) = self.rest.split_at(len);
        let value =
            std::str::from_utf8(digits).ok().and_then(|s| s.parse::<f64>().ok()).ok_or(ParseError::NotANumber)?;
        self.rest = tail;
        Ok(value)
    }
}

fn extend_exponent_sign(bytes: &[u8], len: usize) -> usize {
    let is_exponent = len > 0 && matches!(bytes[len - 1], b'e' | b'E') && len > 1;
    if is_exponent && matches!(bytes.get(len), Some(b'+' | b'-')) {
        let digits = bytes[len + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 {
            return len + 1 + digits;
        }
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<f64, ParseError> {
        parse(s, "", 0.0, 1.0)
    }

    #[test]
    fn full_width_forms_read_as_their_ascii_twins() {
        assert_eq!(p("０．２５"), Ok(0.25));
        assert!((p("（１＋１）／８").unwrap_or(f64::NAN) - 0.25).abs() < 1e-12);
        assert!((p("５０％").unwrap_or(f64::NAN) - 0.5).abs() < 1e-12);
        assert_eq!(p("\u{3000}０．５\u{3000}"), Ok(0.5), "an ideographic space is a space");
        assert_eq!(p("あ"), Err(ParseError::NotANumber));
        assert_eq!(halfwidth("abc").as_ref(), "abc");
    }

    #[test]
    fn plain_numbers_and_arithmetic() {
        assert_eq!(p("0.62"), Ok(0.62));
        assert_eq!(p("  .5 "), Ok(0.5));
        assert_eq!(p("-0.25"), Ok(-0.25));
        assert!((p("0.5*1.2").unwrap_or(f64::NAN) - 0.6).abs() < 1e-12);
        assert!((p("(1+2)/4").unwrap_or(f64::NAN) - 0.75).abs() < 1e-12);
        assert!((p("1 - 0.25 - 0.25").unwrap_or(f64::NAN) - 0.5).abs() < 1e-12);
        assert!((p("1e-3").unwrap_or(f64::NAN) - 0.001).abs() < 1e-15);
        assert_eq!(p("--2"), Ok(2.0));
    }

    #[test]
    fn percent_is_a_position_along_the_range() {
        assert!((p("30%").unwrap_or(f64::NAN) - 0.3).abs() < 1e-12);
        assert!((parse("30%", "", 10.0, 20.0).unwrap_or(f64::NAN) - 13.0).abs() < 1e-12);
        assert!((parse("50 %", "", -1.0, 1.0).unwrap_or(f64::NAN)).abs() < 1e-12);
    }

    #[test]
    fn a_trailing_unit_is_ignored_case_insensitively() {
        assert_eq!(parse("30°", "°", 0.0, 360.0), Ok(30.0));
        assert_eq!(parse("12 PX", "px", 0.0, 100.0), Ok(12.0));
        assert_eq!(parse("2x", "x", 0.0, 5.0), Ok(2.0));
        assert_eq!(parse("2x", "", 0.0, 5.0), Err(ParseError::NotANumber));
    }

    #[test]
    fn rejects_what_is_not_a_number() {
        assert_eq!(p(""), Err(ParseError::Empty));
        assert_eq!(p("   "), Err(ParseError::Empty));
        assert_eq!(p("abc"), Err(ParseError::NotANumber));
        assert_eq!(p("1+"), Err(ParseError::NotANumber));
        assert_eq!(p("(1"), Err(ParseError::NotANumber));
        assert_eq!(p("1 2"), Err(ParseError::NotANumber));
        assert_eq!(p("1/0"), Err(ParseError::DivideByZero));
        assert_eq!(p("inf"), Err(ParseError::NotANumber));
    }
}
