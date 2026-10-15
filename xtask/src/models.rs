//! `cargo xtask models`: every shipped model is backed by an allowed row in `docs/licensing/register.md` (ML-01, ADR 0017).
//!
//! Why: a model we ship must have a licence somebody checked. Its manifest names the register row it rests on
//! (`card.register`) and the SPDX licence it declares; this lint refuses a model whose row is missing, not `yes` or
//! `conditions`, or that doesn't name the same licence. It also refuses a `register_model!` that registers a different id
//! for the file than the manifest says. Mock crates ship test fixtures, so their `models/` folders are exempt.
//!
//! The licence comparison is exact: the row's `Key condition` cell must name the manifest's whole SPDX expression as
//! whole tokens, and no non-commercial licence ships. Every `register_model!` of a crate must include one of its own
//! `models/*.model.toml`, or the gate fails: it can't check a manifest it doesn't scan.
//!
//! Checked: every `models/*.model.toml` of a workspace crate whose name doesn't end in `-mock`. It runs no build, only
//! parsing and reads. `cargo xtask check` runs it on the whole repo, since it is cheap.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use omuzaki_ai_api::{ModelManifest, Origin};
use xtask::registry::lex::{Tok, Token, tokens};

use crate::workspace::{Package, Workspace};

/// The statuses a shipped model may rest on (`docs/licensing/policy.md`, Verdicts).
const ALLOWED_STATUS: [&str; 2] = ["yes", "conditions"];

/// The register the rows are read from, relative to the workspace root.
const REGISTER: &str = "docs/licensing/register.md";

/// One row of the register table: its `Item`, `Status` and `Key condition` cells (the last names the licence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    item: String,
    status: String,
    condition: String,
}

/// The table rows of `register.md`, in order. The format rule at its top fixes the columns: `Item` first, `Status`
/// fourth, `Key condition` fifth. Header and separator lines parse too, but no real item is named `Status` or `---`.
pub fn rows(register: &str) -> Vec<Row> {
    register
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| {
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            (cells.len() >= 5).then(|| Row {
                item: cells[0].to_owned(),
                status: cells[3].to_owned(),
                condition: cells[4].to_owned(),
            })
        })
        .collect()
}

/// Every problem with one manifest, each as one line of text (the caller prefixes the file). `text` is its TOML;
/// `register` its rows; `sources` the `(path, text)` of the `.rs` files of its crate.
pub fn problems(file: &str, text: &str, register: &[Row], sources: &[(String, String)]) -> Vec<String> {
    let manifest = match ModelManifest::from_toml(text) {
        Ok(manifest) => manifest,
        Err(error) => return vec![error.to_string()],
    };
    let mut out: Vec<String> = match manifest.validate(Origin::Shipped) {
        Ok(()) => Vec::new(),
        Err(errors) => errors.iter().map(ToString::to_string).collect(),
    };
    if let Some(item) = &manifest.card.register {
        match register.iter().find(|row| &row.item == item) {
            None => out.push(format!("no row for `{item}` in {REGISTER}")),
            Some(row) if !ALLOWED_STATUS.contains(&row.status.as_str()) => out.push(format!(
                "the row for `{item}` in {REGISTER} is `{}`; a shipped model needs `yes` or `conditions`",
                row.status
            )),
            Some(row) => {
                if let Some(spdx) = &manifest.card.licence.spdx {
                    if !names_licence(&row.condition, spdx) {
                        out.push(format!("the row for `{item}` in {REGISTER} doesn't name its licence `{spdx}`"));
                    }
                    if is_non_commercial(spdx) {
                        out.push(format!("the licence `{spdx}` is non-commercial; a shipped model can't use it"));
                    }
                }
            }
        }
    }
    let file_name = file.rsplit('/').next().unwrap_or(file);
    for (path, source) in sources {
        for (id, include) in registrations(source).into_iter().flatten() {
            let include = include.rsplit('/').next().unwrap_or_default();
            if include == file_name && id != manifest.id.as_str() {
                out.push(format!(
                    "`register_model!` in {path} registers `{id}` for this file; its manifest's id is `{}`",
                    manifest.id
                ));
            }
        }
    }
    out
}

/// The SPDX tokens of `text`: runs of identifier characters (a trailing `.` or `:` is sentence punctuation) and the
/// parentheses of an expression. Everything else, backticks and commas included, only separates.
fn spdx_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<String>| {
        let trimmed = word.trim_end_matches(['.', ':']);
        if !trimmed.is_empty() {
            out.push(trimmed.to_owned());
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '+' | ':') {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            if matches!(c, '(' | ')') {
                out.push(c.to_string());
            }
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Whether a register `condition` cell names the whole SPDX expression `spdx`: its tokens appear in a row, as whole
/// tokens (`MIT` is not in `MIT-0`) and not as part of a longer expression (`MIT` is not in `MIT OR GPL-3.0-only`).
fn names_licence(condition: &str, spdx: &str) -> bool {
    let want = spdx_tokens(spdx);
    let have = spdx_tokens(condition);
    let joins = |token: Option<&String>| token.is_some_and(|t| matches!(t.as_str(), "AND" | "OR" | "WITH"));
    !want.is_empty()
        && have.windows(want.len()).enumerate().any(|(at, window)| {
            window == want.as_slice() && !(at > 0 && joins(have.get(at - 1))) && !joins(have.get(at + want.len()))
        })
}

/// Whether an SPDX expression names a non-commercial licence (`CC-BY-NC-4.0`): nothing non-commercial ships.
fn is_non_commercial(spdx: &str) -> bool {
    spdx_tokens(spdx).iter().any(|token| token.split('-').any(|part| part == "NC" || part == "NonCommercial"))
}

/// Every problem with the `register_model!` calls of one crate that no manifest check can see: a call whose literals
/// can't be read, and one that includes anything but `<crate>/models/<name>.model.toml`, the only files the gate scans.
/// `dir` is the crate's workspace-relative folder; `sources` the `(path, text)` of its `.rs` files.
fn unchecked_registrations(dir: &str, sources: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, source) in sources {
        for registration in registrations(source) {
            let include = match registration {
                Ok((_, include)) => include,
                Err(reason) => {
                    out.push(format!("{path}: {reason}"));
                    continue;
                }
            };
            let from = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            let resolved = normalise(&format!("{from}/{include}"));
            let name = resolved.strip_prefix(&format!("{}/models/", normalise(dir)));
            if !name.is_some_and(|name| name.ends_with(".model.toml") && !name.contains('/')) {
                out.push(format!(
                    "{path}: `register_model!` includes `{include}`, which is not a `models/*.model.toml` of its own \
                     crate, so the gate can't check its licence"
                ));
            }
        }
    }
    out
}

/// A `/`-separated path with its `.` and `..` segments resolved (a `..` past the start stays, so it never matches).
fn normalise(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/').filter(|part| !part.is_empty() && *part != ".") {
        if part == ".." && parts.last().is_some_and(|last| *last != "..") {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    parts.join("/")
}

/// The `(id, path)` of every `register_model!("<id>", include_str!("<path>"))` in one Rust source. It reads the
/// registry's lexer, so a commented-out or quoted example never counts, and whitespace or newlines inside the call don't
/// matter. Either macro may use any of Rust's delimiters, `(…)`, `[…]` or `{…}`, as long as each closes with its match,
/// and either may end in a trailing comma. A call of any other shape, or a string literal that can't be decoded, is an
/// `Err` with the reason, never skipped.
fn registrations(source: &str) -> Vec<Result<(String, String), String>> {
    let toks = tokens(source);
    let mut found = Vec::new();
    for at in 0..toks.len() {
        if !toks[at].is_ident("register_model") || !is_punct(&toks, at + 1, '!') {
            continue;
        }
        let line = toks[at].line;
        let unreadable = || {
            format!("line {line}: `register_model!` isn't `(\"<id>\", include_str!(\"<path>\"))` with string literals")
        };
        let Some(close) = toks.get(at + 2).and_then(closer) else {
            found.push(Err(unreadable()));
            continue;
        };
        let call = toks.get(at + 3).and_then(string_value).zip(include_file(&toks, at + 4));
        let Some((id, (file, end))) = call else {
            found.push(Err(unreadable()));
            continue;
        };
        let end = if is_punct(&toks, end, ',') { end + 1 } else { end };
        if !is_punct(&toks, end, close) {
            found.push(Err(unreadable()));
            continue;
        }
        found.push(match (id, file) {
            (Ok(id), Ok(file)) => Ok((id, file)),
            (Err(reason), _) | (_, Err(reason)) => Err(format!("line {line}: {reason}")),
        });
    }
    found
}

/// The closing delimiter for a macro's opening one: `(`, `[` or `{`; `None` for any other token.
fn closer(token: &Token) -> Option<char> {
    [('(', ')'), ('[', ']'), ('{', '}')].into_iter().find(|&(open, _)| token.is_punct(open)).map(|(_, close)| close)
}

/// The decoded path of `, include_str!("<path>")` starting at `at`, and the index of the token after its closing
/// delimiter. The path may be followed by a trailing comma.
fn include_file(toks: &[Token], at: usize) -> Option<(Result<String, String>, usize)> {
    let include = is_punct(toks, at, ',')
        && toks.get(at + 1).is_some_and(|t| t.is_ident("include_str"))
        && is_punct(toks, at + 2, '!');
    if !include {
        return None;
    }
    let close = closer(toks.get(at + 3)?)?;
    let path = string_value(toks.get(at + 4)?)?;
    let end = if is_punct(toks, at + 5, ',') { at + 6 } else { at + 5 };
    if !is_punct(toks, end, close) {
        return None;
    }
    Some((path, end + 1))
}

fn is_punct(toks: &[Token], at: usize, c: char) -> bool {
    toks.get(at).is_some_and(|t| t.is_punct(c))
}

/// The value of a string literal token as Rust reads it; `None` if the token isn't a string literal, `Err` if it is one
/// but can't be decoded.
fn string_value(token: &Token) -> Option<Result<String, String>> {
    let Tok::Literal(text) = &token.tok else { return None };
    if let Some(raw) = text.strip_prefix('r') {
        let hashes = raw.len() - raw.trim_start_matches('#').len();
        let body = raw[hashes..].strip_prefix('"')?;
        let tail = format!("\"{}", "#".repeat(hashes));
        return Some(match body.strip_suffix(tail.as_str()) {
            Some(body) => Ok(body.to_owned()),
            None => Err(format!("malformed raw string {text}")),
        });
    }
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    Some(unescape(body).ok_or_else(|| format!("cannot decode the string literal {text}")))
}

/// Decodes the escapes of a (non-raw) string literal body the way Rust does: `\\ \" \' \n \r \t \0`, `\xNN` (up to
/// `7F`), `\u{…}` and a line continuation, which skips the newline and the whitespace after it. `None` on anything else.
fn unescape(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            'x' => {
                let digits: String = [chars.next()?, chars.next()?].into_iter().collect();
                let value = u8::from_str_radix(&digits, 16).ok().filter(u8::is_ascii)?;
                out.push(char::from(value));
            }
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let mut digits = String::new();
                loop {
                    match chars.next()? {
                        '}' => break,
                        '_' => {}
                        d if d.is_ascii_hexdigit() => digits.push(d),
                        _ => return None,
                    }
                }
                if digits.is_empty() || digits.len() > 6 {
                    return None;
                }
                out.push(char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?);
            }
            '\n' | '\r' => while chars.next_if(|c| matches!(c, ' ' | '\t' | '\n' | '\r')).is_some() {},
            _ => return None,
        }
    }
    Some(out)
}

/// Every `.rs` file under `dir`, recursively, as `(workspace-relative path, text)`. An unreadable file is reported as its
/// own text so the lint still runs, and the problem shows.
fn rust_sources(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            rust_sources(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            let text = fs::read_to_string(&path).unwrap_or_default();
            out.push((relative, text));
        }
    }
}

/// The manifests of one package, sorted by name: `(workspace-relative path, text)`. A package with no `models/` folder has
/// none.
fn manifests(root: &Path, package: &Package) -> Vec<(String, String)> {
    let dir = root.join(&package.dir).join("models");
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.ends_with(".model.toml"))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let path = format!("{}/models/{name}", package.dir.to_string_lossy());
            let text = fs::read_to_string(dir.join(&name)).unwrap_or_default();
            (path, text)
        })
        .collect()
}

/// Prints every problem and a summary; returns whether there were none.
pub fn report(workspace: &Workspace) -> bool {
    let register = match fs::read_to_string(workspace.root.join(REGISTER)) {
        Ok(text) => rows(&text),
        Err(error) => {
            println!("{REGISTER}: cannot be read: {error}");
            println!("models: 1 problem(s)");
            return false;
        }
    };
    let mut checked = 0;
    let mut skipped = 0;
    let mut found = Vec::new();
    for package in &workspace.packages {
        let manifests = manifests(&workspace.root, package);
        if package.name.ends_with("-mock") {
            skipped += manifests.len();
            continue;
        }
        let mut sources = Vec::new();
        rust_sources(&workspace.root, &workspace.root.join(&package.dir).join("src"), &mut sources);
        found.extend(unchecked_registrations(&package.dir.to_string_lossy(), &sources));
        for (file, text) in &manifests {
            checked += 1;
            found.extend(problems(file, text, &register, &sources).into_iter().map(|p| format!("{file}: {p}")));
        }
    }
    for line in &found {
        println!("{line}");
    }
    if found.is_empty() {
        println!("models: ok ({checked} shipped model(s) checked, {skipped} mock fixture(s) skipped)");
    } else {
        println!("models: {} problem(s); the rules are in docs/licensing/policy.md", found.len());
    }
    found.is_empty()
}

/// The `models` command.
pub fn run(workspace: &Workspace) -> ExitCode {
    if report(workspace) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::testing::workspace;

    const REGISTER_TEXT: &str = "\
| Item | Kind | Used for | Status | Key condition | Audit |
|---|---|---|---|---|---|
| Tiny model | model | a test | yes | keep notice; MIT OR Apache-2.0 | A1 |
| Blocked model | model | a test | NO | CC BY-NC | A1 |
";

    /// A good shipped manifest, citing the `Tiny model` row, with the given `urls` and `id`.
    fn manifest(id: &str, urls: &str) -> String {
        format!(
            r#"id = "{id}"
version = "1.0.0"
name = "Tiny"
slot = "test_echo@1"
runtime = "fake"
providers = ["cpu"]

[[files]]
name = "tiny.fake.toml"
sha256 = "e5b9c1f03be8153f32f0df28a9196840741698957730b384a262d2b9517bb99c"
size = 226
urls = [{urls}]

[card]
register = "Tiny model"
authors = "Omuzaki"

[card.licence]
spdx = "MIT OR Apache-2.0"
"#
        )
    }

    fn register() -> Vec<Row> {
        rows(REGISTER_TEXT)
    }

    fn good_source(id: &str) -> Vec<(String, String)> {
        let text = format!(r#"register_model!("{id}", include_str!("../models/tiny.model.toml"));"#);
        vec![("crates/omuzaki-tiny/src/lib.rs".to_owned(), text)]
    }

    const HTTPS: &str = r#""https://models.example/tiny/1.0.0/tiny.fake.toml""#;
    const FILE: &str = "crates/omuzaki-tiny/models/tiny.model.toml";

    #[test]
    fn a_good_manifest_and_row_pass() {
        let found = problems(FILE, &manifest("tiny", HTTPS), &register(), &good_source("tiny"));
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_missing_row_gives_one_finding() {
        let text = manifest("tiny", HTTPS).replace("Tiny model", "Other model");
        let found = problems(FILE, &text, &register(), &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("no row for `Other model`"), "{found:?}");
    }

    #[test]
    fn an_unverified_or_banned_row_gives_one_finding() {
        let text = manifest("tiny", HTTPS).replace("Tiny model", "Blocked model");
        let found = problems(FILE, &text, &register(), &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("is `NO`"), "{found:?}");
    }

    #[test]
    fn a_licence_the_row_does_not_name_gives_one_finding() {
        let text = manifest("tiny", HTTPS).replace("MIT OR Apache-2.0\"\n", "GPL-3.0-only\"\n");
        let found = problems(FILE, &text, &register(), &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("doesn't name its licence `GPL-3.0-only`"), "{found:?}");
    }

    #[test]
    fn a_file_with_no_urls_gives_one_finding() {
        let found = problems(FILE, &manifest("tiny", ""), &register(), &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn a_non_https_url_gives_one_finding() {
        let http = r#""http://models.example/tiny/1.0.0/tiny.fake.toml""#;
        let found = problems(FILE, &manifest("tiny", http), &register(), &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn an_id_mismatch_gives_one_finding() {
        let found = problems(FILE, &manifest("tiny", HTTPS), &register(), &good_source("other"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("registers `other` for this file"), "{found:?}");
    }

    #[test]
    fn a_register_model_for_another_file_is_not_this_manifests() {
        let text = r#"register_model!("other", include_str!("../models/another.model.toml"));"#;
        let sources = vec![("crates/omuzaki-tiny/src/lib.rs".to_owned(), text.to_owned())];
        assert_eq!(problems(FILE, &manifest("tiny", HTTPS), &register(), &sources), Vec::<String>::new());
    }

    #[test]
    fn a_parse_error_gives_one_finding() {
        let found = problems(FILE, "id = [", &register(), &[]);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn the_register_rows_read_the_item_and_status_columns() {
        let parsed = register();
        assert_eq!(parsed[0].item, "Item");
        assert_eq!(parsed[2].item, "Tiny model");
        assert_eq!(parsed[2].status, "yes");
        assert_eq!(parsed[3].status, "NO");
    }

    #[test]
    fn the_scan_reads_every_registration_and_its_file() {
        let text = r#"
            register_model!("echo-fake", include_str!("../models/echo-fake.model.toml"));
            register_model!(
                "other",
                include_str!("../models/other.model.toml")
            );
        "#;
        assert_eq!(
            registrations(text),
            [
                Ok(("echo-fake".to_owned(), "../models/echo-fake.model.toml".to_owned())),
                Ok(("other".to_owned(), "../models/other.model.toml".to_owned())),
            ]
        );
    }

    #[test]
    fn whitespace_newlines_and_raw_strings_do_not_hide_a_registration() {
        let spread =
            "register_model!(\n    \"tiny\",\n    include_str!(\n        \"../models/tiny.model.toml\"\n    ),\n);";
        assert_eq!(registrations(spread), [Ok(("tiny".to_owned(), "../models/tiny.model.toml".to_owned()))]);
        let raw = r##"register_model!(r#"tiny"#, include_str!("../models/tiny.model.toml"));"##;
        assert_eq!(registrations(raw), [Ok(("tiny".to_owned(), "../models/tiny.model.toml".to_owned()))]);
    }

    #[test]
    fn each_macro_delimiter_form_is_found() {
        let forms = [
            r#"register_model!("tiny", include_str!("../models/tiny.model.toml"));"#,
            r#"register_model!["tiny", include_str!["../models/tiny.model.toml"]];"#,
            r#"register_model! { "tiny", include_str! { "../models/tiny.model.toml" } }"#,
            r#"register_model! { "tiny", include_str!("../models/tiny.model.toml"), }"#,
            r#"register_model!["tiny", include_str!["../models/tiny.model.toml"],];"#,
            r#"register_model!("tiny", include_str!("../models/tiny.model.toml",));"#,
            r#"register_model!("tiny", include_str!("../models/tiny.model.toml",),);"#,
        ];
        for form in forms {
            assert_eq!(
                registrations(form),
                [Ok(("tiny".to_owned(), "../models/tiny.model.toml".to_owned()))],
                "{form}"
            );
        }
    }

    #[test]
    fn a_mismatched_closer_fails_the_gate_instead_of_being_skipped() {
        let outer = r#"register_model! { "tiny", include_str!("../models/tiny.model.toml") )"#;
        let inner = r#"register_model!("tiny", include_str!["../models/tiny.model.toml"));"#;
        for text in [outer, inner] {
            assert!(matches!(registrations(text).as_slice(), [Err(_)]), "{:?}", registrations(text));
            assert_eq!(unchecked(text).len(), 1, "{text}");
        }
    }

    #[test]
    fn an_id_mismatch_in_braces_gives_one_finding() {
        let text = r#"register_model! { "other", include_str!("../models/tiny.model.toml") }"#;
        let sources = vec![("crates/omuzaki-tiny/src/lib.rs".to_owned(), text.to_owned())];
        let found = problems(FILE, &manifest("tiny", HTTPS), &register(), &sources);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("registers `other` for this file"), "{found:?}");
    }

    #[test]
    fn an_id_mismatch_in_brackets_gives_one_finding() {
        let text = r#"register_model!["other", include_str!("../models/tiny.model.toml")];"#;
        let sources = vec![("crates/omuzaki-tiny/src/lib.rs".to_owned(), text.to_owned())];
        let found = problems(FILE, &manifest("tiny", HTTPS), &register(), &sources);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("registers `other` for this file"), "{found:?}");
    }

    /// The one finding `problems` gives for `text` registered against `FILE`'s manifest (id `tiny`).
    fn findings_for(text: &str) -> Vec<String> {
        let sources = vec![("crates/omuzaki-tiny/src/lib.rs".to_owned(), text.to_owned())];
        problems(FILE, &manifest("tiny", HTTPS), &register(), &sources)
    }

    #[test]
    fn an_inner_trailing_comma_does_not_hide_a_mismatch() {
        let found = findings_for(r#"register_model!("other", include_str!("../models/tiny.model.toml",));"#);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("registers `other` for this file"), "{found:?}");
    }

    #[test]
    fn an_escaped_path_does_not_hide_a_mismatch() {
        let found = findings_for(r#"register_model!("other", include_str!("../models/\x74iny.model.toml"));"#);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("registers `other` for this file"), "{found:?}");
        let unicode = findings_for(r#"register_model!("ot\u{68}er", include_str!("../models/ti\u{6e}y.model.toml"));"#);
        assert!(unicode[0].contains("registers `other` for this file"), "{unicode:?}");
    }

    #[test]
    fn a_raw_string_path_does_not_hide_a_mismatch() {
        let found = findings_for(r##"register_model!("other", include_str!(r#"../models/tiny.model.toml"#));"##);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("registers `other` for this file"), "{found:?}");
    }

    /// What `unchecked_registrations` says about `text`, as if it were `crates/omuzaki-tiny/src/lib.rs`.
    fn unchecked(text: &str) -> Vec<String> {
        unchecked_registrations(
            "crates/omuzaki-tiny",
            &[("crates/omuzaki-tiny/src/lib.rs".to_owned(), text.to_owned())],
        )
    }

    #[test]
    fn an_undecodable_literal_fails_the_gate() {
        let found = unchecked(r#"register_model!("tiny", include_str!("../models/\q.model.toml"));"#);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("cannot decode the string literal"), "{found:?}");
        let out_of_range = unchecked(r#"register_model!("t\xFFny", include_str!("../models/tiny.model.toml"));"#);
        assert!(out_of_range[0].contains("cannot decode"), "{out_of_range:?}");
    }

    #[test]
    fn a_manifest_outside_the_crates_models_folder_fails_the_gate() {
        for path in [
            "../assets/x.toml",
            "../models/sub/x.model.toml",
            "../../omuzaki-other/models/x.model.toml",
            "../models/x.toml",
        ] {
            let found = unchecked(&format!(r#"register_model!("x", include_str!("{path}"));"#));
            assert_eq!(found.len(), 1, "{path}: {found:?}");
            assert!(found[0].contains("can't check its licence"), "{found:?}");
        }
        assert_eq!(
            unchecked(r#"register_model!("x", include_str!("../models/./x.model.toml"));"#),
            Vec::<String>::new()
        );
        assert_eq!(
            unchecked(r#"register_model!("x", include_str!("../src/../models/x.model.toml"));"#),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_call_that_is_not_two_literals_fails_the_gate() {
        for call in [
            r#"register_model!("x", include_str!(concat!("../models/", "x.model.toml")));"#,
            r#"register_model!(ID, include_str!("../models/x.model.toml"));"#,
            r#"register_model!("x", MANIFEST);"#,
        ] {
            let found = unchecked(call);
            assert_eq!(found.len(), 1, "{call}: {found:?}");
            assert!(found[0].contains("isn't"), "{found:?}");
        }
        assert_eq!(unchecked("macro_rules! register_model { () => {} }"), Vec::<String>::new());
    }

    #[test]
    fn a_crate_without_models_is_scanned_too() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs/licensing")).unwrap();
        fs::write(root.join(REGISTER), REGISTER_TEXT).unwrap();
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("src")).unwrap();
        fs::write(tiny.join("src/lib.rs"), r#"register_model!("x", include_str!("../assets/x.toml"));"#).unwrap();
        let mut ws = workspace(&[("omuzaki-tiny", &[])]);
        ws.root = root.to_path_buf();
        assert!(!report(&ws));
    }

    #[test]
    fn the_licence_must_be_named_whole() {
        assert!(names_licence("keep notice; MIT OR Apache-2.0", "MIT OR Apache-2.0"));
        assert!(names_licence("`MIT`, verified. Keep the notice", "MIT"));
        assert!(names_licence("Apache-2.0 per the model cards", "Apache-2.0"));
        assert!(names_licence("licence: GPL-2.0+.", "GPL-2.0+"));
        assert!(!names_licence("MIT-0; keep notice", "MIT"));
        assert!(!names_licence("Apache-2.0-or-later", "Apache-2.0"));
        assert!(!names_licence("MIT OR GPL-3.0-only", "MIT"));
        assert!(!names_licence("GPL-3.0-only AND MIT", "MIT"));
        assert!(!names_licence("keep notice", "MIT"));
        assert!(!names_licence("MIT", ""));
    }

    #[test]
    fn mit_does_not_match_a_row_that_only_names_mit_zero() {
        let rows = rows("| Tiny model | model | a test | yes | MIT-0; keep notice | A1 |\n");
        let text = manifest("tiny", HTTPS).replace("MIT OR Apache-2.0\"\n", "MIT\"\n");
        let found = problems(FILE, &text, &rows, &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("doesn't name its licence `MIT`"), "{found:?}");
    }

    #[test]
    fn a_non_commercial_licence_fails_even_when_the_row_mentions_it() {
        let rows = rows("| Tiny model | model | a test | yes | not CC-BY-NC-4.0; keep notice | A1 |\n");
        let text = manifest("tiny", HTTPS).replace("MIT OR Apache-2.0\"\n", "CC-BY-NC-4.0\"\n");
        let found = problems(FILE, &text, &rows, &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("non-commercial"), "{found:?}");
        assert!(is_non_commercial("MIT OR CC-BY-NC-SA-4.0"));
        assert!(!is_non_commercial("MIT OR Apache-2.0"));
    }

    #[test]
    fn rust_escapes_decode_like_rust() {
        let cases = [
            (r#"a\\b\"c\'d\ne\rf\tg\0h"#, "a\\b\"c'd\ne\rf\tg\0h"),
            (r"\x41\u{1F600}\u{4_1}", "A\u{1F600}A"),
            ("ab\\\n   \t cd", "abcd"),
        ];
        for (body, want) in cases {
            assert_eq!(unescape(body).as_deref(), Some(want), "{body}");
        }
        for bad in ["\\", "\\x4", "\\x80", "\\u41", "\\u{}", "\\u{110000}", "\\u{D800}", "\\u{1234567}"] {
            assert_eq!(unescape(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_commented_out_or_quoted_example_is_not_a_registration() {
        let text = r#"
// register_model!("tiny", include_str!("../models/tiny.model.toml"));
/* register_model!("tiny", include_str!("../models/tiny.model.toml")); */
/// register_model!("tiny", include_str!("../models/tiny.model.toml"));
const DOC: &str = "register_model!(\"tiny\", include_str!(\"../models/tiny.model.toml\"))";
"#;
        assert!(registrations(text).is_empty(), "{:?}", registrations(text));
    }

    #[test]
    fn a_register_only_change_to_no_fails_the_gate_without_touching_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs/licensing")).unwrap();
        fs::write(root.join(REGISTER), REGISTER_TEXT).unwrap();
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("models")).unwrap();
        fs::create_dir_all(tiny.join("src")).unwrap();
        fs::write(tiny.join("models/tiny.model.toml"), manifest("tiny", HTTPS)).unwrap();
        fs::write(tiny.join("src/lib.rs"), good_source("tiny")[0].1.as_str()).unwrap();
        let mut ws = workspace(&[("omuzaki-tiny", &[])]);
        ws.root = root.to_path_buf();
        assert!(report(&ws));

        fs::write(root.join(REGISTER), REGISTER_TEXT.replace("| yes |", "| NO |")).unwrap();
        assert!(!report(&ws));
    }

    #[test]
    fn the_gate_runs_on_a_workspace_and_exempts_mock_crates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs/licensing")).unwrap();
        fs::write(root.join(REGISTER), REGISTER_TEXT).unwrap();
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("models")).unwrap();
        fs::create_dir_all(tiny.join("src")).unwrap();
        fs::write(tiny.join("models/tiny.model.toml"), manifest("tiny", HTTPS)).unwrap();
        fs::write(tiny.join("src/lib.rs"), good_source("tiny")[0].1.as_str()).unwrap();
        let mock = root.join("crates/omuzaki-tiny-mock");
        fs::create_dir_all(mock.join("models")).unwrap();
        fs::write(mock.join("models/fixture.model.toml"), "not a manifest").unwrap();

        let mut ws = workspace(&[("omuzaki-tiny", &[]), ("omuzaki-tiny-mock", &[])]);
        ws.root = root.to_path_buf();
        assert!(report(&ws));

        fs::write(tiny.join("models/tiny.model.toml"), manifest("tiny", "")).unwrap();
        assert!(!report(&ws));
    }
}
