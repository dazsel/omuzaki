//! Source lint for SE-03 ("a CI lint fails any hard-coded text"): no component writes a user-facing word into its
//! drawing code. Words a component adds on its own are messages in `ds::strings`; everything else comes from the caller.
//!
//! The pseudo-locale tests catch what a frame shows; this catches what no test happens to draw (a tooltip nobody
//! hovers, an error nobody provokes). It reads the string literals of `src/ds/widgets/*.rs`, outside test modules,
//! comments and the identifiers egui keys state by (`Id::new("..")`, `.with("..")`).

use std::path::Path;

use omuzaki_ui::ds::audit::plain_english;

/// A string literal, and the line of source it sits on.
struct Literal {
    text: String,
    line: usize,
}

/// The string literals of `source`, skipping comments. Handles escapes and raw strings; a `'` that opens a lifetime is
/// told from a character literal by looking for the closing quote.
fn literals(source: &str) -> Vec<Literal> {
    let chars: Vec<char> = source.chars().collect();
    let (mut found, mut i, mut line) = (Vec::new(), 0, 1);
    while i < chars.len() {
        match chars[i] {
            '\n' => {
                line += 1;
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    line += usize::from(chars[i] == '\n');
                    i += 1;
                }
                i += 2;
            }
            '\'' => {
                // 'x' or '\n' is a character literal; anything else ('a, 'static) is a lifetime.
                let closes = if chars.get(i + 1) == Some(&'\\') { i + 3 } else { i + 2 };
                i = if chars.get(closes) == Some(&'\'') { closes + 1 } else { i + 1 };
            }
            '"' => {
                let (start_line, mut text) = (line, String::new());
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    if let Some(c) = chars.get(i) {
                        text.push(*c);
                        line += usize::from(*c == '\n');
                    }
                    i += 1;
                }
                i += 1;
                found.push(Literal { text, line: start_line });
            }
            _ => i += 1,
        }
    }
    found
}

/// Lines whose literals are not words for a person: state keys, attributes, assertions, key names to look up.
const NOT_FOR_PEOPLE: &[&str] = &[
    "Id::new(",
    ".with(",
    ".id_salt(",
    "make_persistent_id(",
    "debug_assert",
    "reason =",
    "#[",
    "#![",
    "include_",
    "concat!(",
    // The key-label table in `keycap.rs`: names printed on keyboards, by platform and language. Not messages.
    "// key name",
];

#[test]
fn no_component_writes_a_user_facing_word_into_its_drawing_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ds/widgets");
    let mut offenders = Vec::new();
    let entries = std::fs::read_dir(&dir).map(|d| d.flatten().collect::<Vec<_>>()).unwrap_or_default();
    let mut checked = 0;
    for entry in entries {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") || path.file_name().is_some_and(|n| n == "mod.rs") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else { continue };
        // The drawing code comes first in every file. What follows the test module, or the component's contribution to
        // the contrast table (names that only appear in a failing test's message), is not shown to a person.
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        let production = production.split("register_contrast!(").next().unwrap_or(production);
        let lines: Vec<&str> = production.lines().collect();
        checked += 1;
        for literal in literals(production) {
            let context = lines.get(literal.line.saturating_sub(1)).copied().unwrap_or_default();
            let placeholder_name = context.contains(&format!("(\"{}\",", literal.text));
            if placeholder_name || NOT_FOR_PEOPLE.iter().any(|marker| context.contains(marker)) {
                continue;
            }
            if !plain_english([literal.text.as_str()]).is_empty() {
                offenders.push(format!(
                    "{}:{}: {:?}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    literal.line,
                    literal.text
                ));
            }
        }
    }
    assert!(checked >= 15, "the lint read only {checked} component files");
    assert!(
        offenders.is_empty(),
        "words written into a component instead of a message in ds/strings.rs (or taken from the caller):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_lint_reads_literals_and_skips_comments_and_lifetimes() {
    let source =
        "fn f<'a>(x: &'a str) -> char { // \"not this\"\n let _ = \"Reset to default\"; /* \"nor this\" */ 'x' }";
    let found: Vec<String> = literals(source).into_iter().map(|l| l.text).collect();
    assert_eq!(found, ["Reset to default"]);
    assert_eq!(plain_english(["Reset to default"]), ["Reset to default"]);
    assert_eq!(plain_english(["hair.root", "#8A6FD1", "×"]), Vec::<String>::new());
    // Key names are no longer exempt as words: they come from the key-label table, not the message table.
    assert_eq!(plain_english(["Ctrl Enter"]), ["Ctrl Enter"]);
}
