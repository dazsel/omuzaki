//! Text normalisation before the lexicon: numbers, money, percentages, times and a few abbreviations become the words a
//! person reads them as, so every token the lexicon sees is a word. English (US) readings.

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
const SCALES: [(u64, &str); 4] =
    [(1_000_000_000_000, "trillion"), (1_000_000_000, "billion"), (1_000_000, "million"), (1_000, "thousand")];

/// Abbreviations read as words, matched case-sensitively with their full stop.
const ABBREVIATIONS: [(&str, &str); 12] = [
    ("Mr.", "mister"),
    ("Mrs.", "missus"),
    ("Ms.", "miz"),
    ("Dr.", "doctor"),
    ("St.", "saint"),
    ("Jr.", "junior"),
    ("Sr.", "senior"),
    ("vs.", "versus"),
    ("etc.", "et cetera"),
    ("e.g.", "for example"),
    ("i.e.", "that is"),
    ("approx.", "approximately"),
];

/// `n` in words: `21` → `twenty-one`, `1_005` → `one thousand five`.
#[must_use]
pub fn cardinal(n: u64) -> String {
    if n < 20 {
        return ONES[usize::try_from(n).unwrap_or(0)].to_owned();
    }
    if n < 100 {
        let (tens, ones) = (n / 10, n % 10);
        let tens = TENS[usize::try_from(tens).unwrap_or(0)];
        return if ones == 0 {
            tens.to_owned()
        } else {
            format!("{tens}-{}", ONES[usize::try_from(ones).unwrap_or(0)])
        };
    }
    if n < 1_000 {
        let rest = n % 100;
        let hundreds = format!("{} hundred", ONES[usize::try_from(n / 100).unwrap_or(0)]);
        return if rest == 0 { hundreds } else { format!("{hundreds} {}", cardinal(rest)) };
    }
    for (scale, name) in SCALES {
        if n >= scale {
            let rest = n % scale;
            let head = format!("{} {name}", cardinal(n / scale));
            return if rest == 0 { head } else { format!("{head} {}", cardinal(rest)) };
        }
    }
    unreachable!("every n of 1000 or more meets a scale")
}

/// `n` as an ordinal: `1` → `first`, `22` → `twenty-second`.
#[must_use]
pub fn ordinal(n: u64) -> String {
    let words = cardinal(n);
    let (head, last) = match words.rfind([' ', '-']) {
        Some(i) => words.split_at(i + 1),
        None => ("", words.as_str()),
    };
    let last = match last {
        "one" => "first".to_owned(),
        "two" => "second".to_owned(),
        "three" => "third".to_owned(),
        "five" => "fifth".to_owned(),
        "eight" => "eighth".to_owned(),
        "nine" => "ninth".to_owned(),
        "twelve" => "twelfth".to_owned(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    };
    format!("{head}{last}")
}

/// A four-digit year read the way years are: `1984` → `nineteen eighty-four`, `2005` → `two thousand five`,
/// `2026` → `twenty twenty-six`, `1900` → `nineteen hundred`.
#[must_use]
pub fn year(n: u64) -> String {
    let (high, low) = (n / 100, n % 100);
    if (2000..2010).contains(&n) || n % 1000 == 0 {
        return cardinal(n);
    }
    match low {
        0 => format!("{} hundred", cardinal(high)),
        1..=9 => format!("{} oh {}", cardinal(high), cardinal(low)),
        _ => format!("{} {}", cardinal(high), cardinal(low)),
    }
}

/// Digits one by one: `14` → `one four`.
fn digits(s: &str) -> String {
    s.chars().filter_map(|c| c.to_digit(10)).map(|d| ONES[d as usize]).collect::<Vec<_>>().join(" ")
}

/// Reads a run of digits (commas allowed between groups) as a number; `None` when it is too long for one.
fn parse(run: &str) -> Option<u64> {
    let plain: String = run.chars().filter(char::is_ascii_digit).collect();
    if plain.len() > 15 {
        return None;
    }
    plain.parse().ok()
}

/// Rewrites `text` so that every number, sum of money, percentage and time is in words.
#[must_use]
pub fn normalise(text: &str) -> String {
    let mut text = text.replace(['\u{2018}', '\u{2019}'], "'");
    for (abbreviation, words) in ABBREVIATIONS {
        if text.contains(abbreviation) {
            text = replace_word(&text, abbreviation, words);
        }
    }
    let text = text.replace('&', " and ").replace('+', " plus ").replace('@', " at ");
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts_number = c.is_ascii_digit()
            || ((c == '$' || c == '-') && chars.get(i + 1).is_some_and(char::is_ascii_digit))
            || (c == '.'
                && chars.get(i + 1).is_some_and(char::is_ascii_digit)
                && !chars.get(i.wrapping_sub(1)).is_some_and(char::is_ascii_digit));
        if !starts_number || (c == '-' && chars.get(i.wrapping_sub(1)).is_some_and(|p| p.is_alphanumeric())) {
            out.push(c);
            i += 1;
            continue;
        }
        let (words, used) = number_at(&chars[i..]);
        if !out.is_empty() && !out.ends_with([' ', '(', '"', '-']) {
            out.push(' ');
        }
        out.push_str(&words);
        i += used;
        if chars.get(i).is_some_and(|c| c.is_alphanumeric()) {
            out.push(' ');
        }
    }
    out
}

/// Replaces `word` where it stands on its own (not inside a longer word).
fn replace_word(text: &str, word: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(word) {
        let before_ok = rest[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        out.push_str(&rest[..at]);
        out.push_str(if before_ok { with } else { word });
        rest = &rest[at + word.len()..];
    }
    out.push_str(rest);
    out
}

/// The number that starts `chars` in words, and how many chars it used.
fn number_at(chars: &[char]) -> (String, usize) {
    let mut i = 0;
    let money = chars.first() == Some(&'$');
    let negative = chars.first() == Some(&'-');
    if money || negative {
        i += 1;
    }
    let start = i;
    while i < chars.len()
        && (chars[i].is_ascii_digit()
            || (chars[i] == ',' && chars.get(i + 1).is_some_and(char::is_ascii_digit) && i > start))
    {
        i += 1;
    }
    let whole: String = chars[start..i].iter().collect();
    // A time: 10:30, 9:05, 10:00.
    if !money
        && !negative
        && chars.get(i) == Some(&':')
        && chars.get(i + 1).is_some_and(char::is_ascii_digit)
        && chars.get(i + 2).is_some_and(char::is_ascii_digit)
        && !chars.get(i + 3).is_some_and(char::is_ascii_digit)
    {
        let minutes = u64::from(chars[i + 1].to_digit(10).unwrap_or(0) * 10 + chars[i + 2].to_digit(10).unwrap_or(0));
        let hours = parse(&whole).map_or_else(|| digits(&whole), cardinal);
        let words = match minutes {
            0 => format!("{hours} o'clock"),
            1..=9 => format!("{hours} oh {}", cardinal(minutes)),
            m => format!("{hours} {}", cardinal(m)),
        };
        return (words, i + 3);
    }
    let mut fraction = String::new();
    if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        fraction = chars[i + 1..j].iter().collect();
        i = j;
    }
    let value = if whole.is_empty() { Some(0) } else { parse(&whole) };
    let mut words = match value {
        None => digits(&whole),
        Some(n) if money => {
            let dollars = format!("{} {}", cardinal(n), if n == 1 { "dollar" } else { "dollars" });
            let cents: u64 = if fraction.is_empty() { 0 } else { format!("{fraction:0<2}")[..2].parse().unwrap_or(0) };
            fraction.clear();
            match (n, cents) {
                (_, 0) => dollars,
                (0, c) => format!("{} {}", cardinal(c), if c == 1 { "cent" } else { "cents" }),
                (_, c) => format!("{dollars} and {} {}", cardinal(c), if c == 1 { "cent" } else { "cents" }),
            }
        }
        Some(n) => {
            let suffix: String = chars[i..].iter().take(2).collect::<String>().to_ascii_lowercase();
            let ordinal_suffix = matches!(suffix.as_str(), "st" | "nd" | "rd" | "th")
                && !chars.get(i + 2).is_some_and(|c| c.is_alphanumeric());
            if ordinal_suffix && fraction.is_empty() {
                i += 2;
                ordinal(n)
            } else if fraction.is_empty() && whole.len() == 4 && !whole.contains(',') && (1100..2100).contains(&n) {
                year(n)
            } else {
                cardinal(n)
            }
        }
    };
    if !fraction.is_empty() {
        words = format!("{words} point {}", digits(&fraction));
    }
    if negative {
        words = format!("minus {words}");
    }
    if chars.get(i) == Some(&'%') {
        words.push_str(" percent");
        i += 1;
    }
    (words, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_as_a_person_reads_them() {
        assert_eq!(cardinal(0), "zero");
        assert_eq!(cardinal(21), "twenty-one");
        assert_eq!(cardinal(105), "one hundred five");
        assert_eq!(cardinal(1_234_567), "one million two hundred thirty-four thousand five hundred sixty-seven");
        assert_eq!(ordinal(1), "first");
        assert_eq!(ordinal(22), "twenty-second");
        assert_eq!(ordinal(40), "fortieth");
        assert_eq!(ordinal(112), "one hundred twelfth");
        assert_eq!(year(1984), "nineteen eighty-four");
        assert_eq!(year(2005), "two thousand five");
        assert_eq!(year(2026), "twenty twenty-six");
        assert_eq!(year(1900), "nineteen hundred");
        assert_eq!(year(1907), "nineteen oh seven");
    }

    #[test]
    fn text_is_normalised_around_its_numbers() {
        assert_eq!(normalise("I have 3 cats."), "I have three cats.");
        assert_eq!(normalise("It costs $3.50, not $1."), "It costs three dollars and fifty cents, not one dollar.");
        assert_eq!(normalise("Up 12% in 1999!"), "Up twelve percent in nineteen ninety-nine!");
        assert_eq!(normalise("Meet at 10:30 or 9:05."), "Meet at ten thirty or nine oh five.");
        assert_eq!(normalise("The 2nd and 21st."), "The second and twenty-first.");
        assert_eq!(normalise("pi is 3.14"), "pi is three point one four");
        assert_eq!(normalise("It was -5 out, 1,000 people."), "It was minus five out, one thousand people.");
        assert_eq!(normalise("Dr. Who & Mr. Smith"), "doctor Who  and  mister Smith");
        assert_eq!(normalise("COVID-19 and x2"), "COVID-nineteen and x two");
    }
}
