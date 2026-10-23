//! Source lint for SE-03 ("a CI lint fails any hard-coded text"): no component writes a user-facing word into its
//! drawing code. Words a component adds on its own are messages in `ds::strings`; everything else comes from the caller.
//!
//! The pseudo-locale tests catch what a frame shows; this catches what no test happens to draw (a tooltip nobody
//! hovers, an error nobody provokes). It reads the string literals of every UI module under `src/` (the components,
//! the shell, the panels, the live workspace and the output window; #227), outside test modules, comments and the
//! identifiers egui keys state by (`Id::new("..")`, `.with("..")`).

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
    // Registrations: a panel's, workspace's or story's `title` is the English key the shell translates when it draws.
    "title: \"",
    // A setting's summary, likewise: the English key the Settings window translates when it draws.
    "const META: SettingMeta",
    // A schema option's own title: a language's name in that language, which reads the same in every language.
    "{ \"const\":",
    // Key legends, as printed on keys.
    "shortcut_hint(",
    // A GPU object's debug label, and a type's debug name.
    "label: Some(",
    "debug_struct(",
    // An error's detail. Where the studio shows a failure, a translated sentence leads and the detail fills its
    // `{reason}`, as an operating system's error text does; op errors are what an agent reads over MCP.
    "OpError::",
    "Error::",
    "write!(f",
    "f.write_str(",
    "Err(",
    "ok_or_else(",
    "map_err(",
    // Logs and panics are for the developer reading them, not the person using the studio.
    "tracing::",
    "request_discard(",
    "warn!(",
    "error!(",
    "info!(",
    "debug!(",
    "panic!(",
    "unreachable!(",
    ".expect(",
];

/// Files outside the components whose literals are not prose: the audits' and the contrast table's failure reports,
/// which only a failing test prints; the font table's file names; and the schema form's reading of JSON Schema, whose
/// type names are the schema's own vocabulary, shown as a read-only row's detail; and the lexicon's translator notes,
/// which go into the Fluent files for translators and are never drawn; and My scripts' folder store, whose refusals are
/// op errors (`script.save_mine` returns them), which are English like every op's.
const NOT_PROSE: &[&str] =
    &["audit.rs", "contrast.rs", "fonts.rs", "schema_form/model.rs", "ds/lexicon.rs", "script/store.rs"];

/// Names given to things in the document or the window, not UI text: the product, the kit's sample character, and the
/// name of the project folder the studio makes for itself.
const NAMES: &[&str] = &["Omuzaki", "Kit", "My character"];

/// Whether `text` reads as words for a person outside the components: plain English (see `plain_english`) that is
/// either more than one word or a capitalised word ("Delete"). The component lint counts any word, since a component
/// has no business with single lowercase words either; the rest of the UI also handles schema keywords, file names
/// and command-line arguments, which are single lowercase tokens.
fn reads_as_words(text: &str, strict: bool) -> bool {
    // A placeholder (`{who}: {label}`) is filled from somewhere else; what is left is what this literal says.
    let mut said = String::new();
    let mut depth = 0_u32;
    for c in text.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => said.push(c),
            _ => {}
        }
    }
    if NAMES.contains(&text) || plain_english([said.as_str()]).is_empty() {
        return false;
    }
    let text = said.as_str();
    if !strict {
        return true;
    }
    let words: Vec<&str> = text.split_whitespace().filter(|w| w.chars().any(|c| c.is_ascii_alphabetic())).collect();
    let capitalised = |w: &&str| {
        let mut chars = w.chars();
        chars.next().is_some_and(|c| c.is_ascii_uppercase()) && chars.next().is_some_and(|c| c.is_ascii_lowercase())
    };
    words.len() >= 2 || words.iter().any(capitalised)
}

/// The source files of `dir` and below that draw for a person: everything under `src/` except the message tables
/// (`strings.rs` and each area's `text.rs`, which are where the words belong), test-only modules, the developer
/// gallery (its stories are specimen content and it translates through its own `tr!`), and the binaries' command-line
/// help.
fn drawing_sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(dir).map(|d| d.flatten().collect::<Vec<_>>()).unwrap_or_default();
    for entry in entries {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "tests" | "gallery" | "bin" | "testing") {
                drawing_sources(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs")
            && !matches!(name.as_str(), "strings.rs" | "text.rs" | "tests.rs" | "testing.rs")
        {
            out.push(path);
        }
    }
}

/// The plain-English literals of the drawing code in `source`, as `line: "text"`; `component` for the `ds` widgets,
/// which are held to any word at all.
fn offenders_in(source: &str, component: bool) -> Vec<(usize, String)> {
    // The drawing code comes first in every file. What follows the test module, or a component's contribution to
    // the contrast table (names that only appear in a failing test's message), is not shown to a person.
    let production = source.split("#[cfg(test)]").next().unwrap_or(source);
    let production = production.split("register_contrast!(").next().unwrap_or(production);
    let lines: Vec<&str> = production.lines().collect();
    let mut out = Vec::new();
    for literal in literals(production) {
        let context = lines.get(literal.line.saturating_sub(1)).copied().unwrap_or_default();
        let placeholder_name = context.contains(&format!("(\"{}\",", literal.text));
        // An enum's own words (`Self::TooFewPoints => "…"`) outside a message table are its `Display`: a detail.
        let display_arm = context.trim_start().starts_with("Self::") && context.contains("=>");
        if placeholder_name || display_arm || NOT_FOR_PEOPLE.iter().any(|marker| context.contains(marker)) {
            continue;
        }
        if reads_as_words(&literal.text, !component) {
            out.push((literal.line, literal.text));
        }
    }
    out
}

#[test]
fn no_ui_module_writes_a_user_facing_word_into_its_drawing_code() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    drawing_sources(&src, &mut files);
    files.sort();
    let mut offenders = Vec::new();
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else { continue };
        let shown = path.strip_prefix(&src).unwrap_or(path).display().to_string();
        if NOT_PROSE.iter().any(|name| path.ends_with(name)) {
            continue;
        }
        let component = path.starts_with(src.join("ds/widgets"));
        offenders.extend(
            offenders_in(&source, component).into_iter().map(|(line, text)| format!("{shown}:{line}: {text:?}")),
        );
    }
    for area in ["ds/widgets", "shell", "panels", "live", "output_window"] {
        assert!(files.iter().any(|f| f.starts_with(src.join(area))), "the lint reads no file under src/{area}");
    }
    assert!(files.len() >= 80, "the lint read only {} files", files.len());
    assert!(
        offenders.is_empty(),
        "words written into UI code instead of a message table (ds/strings.rs or the area's text.rs), or taken \
         from the caller:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn a_planted_english_literal_in_a_panel_is_named_with_its_line() {
    let panel = "fn ui(ui: &mut Ui) {\n    let id = Id::new(\"outliner\");\n    ui.label(\"Rename the part\");\n}\n";
    assert_eq!(offenders_in(panel, false), [(3, "Rename the part".to_owned())]);
    // A schema keyword or a file name is not words for a person outside the components; a capitalised word is.
    let keys = "fn f() {\n    v.get(\"properties\");\n    let t = \"Delete\";\n}\n";
    assert_eq!(offenders_in(keys, false), [(3, "Delete".to_owned())]);
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
