//! `cargo xtask models`: every shipped model is backed by an allowed entry in `docs/licensing/deps/` (ML-01, ADR 0017).
//!
//! Why: a model we ship must have a licence somebody checked. Its manifest names the register item it rests on
//! (`card.register`, the `item` of a `docs/licensing/deps/*.toml` file, which `register.md` is generated from) and the
//! SPDX licence it declares; this lint refuses a model whose entry is missing, not `yes` or
//! `conditions`, or that doesn't name the same licence. It also refuses a `register_model!` that registers a different id
//! for the file than the manifest says. Mock crates ship test fixtures, so their `models/` folders are exempt.
//!
//! The licence comparison is exact: the entry's `condition` must name the manifest's whole SPDX expression as
//! whole tokens, and no non-commercial licence ships. Every `register_model!` of a crate must include one of its own
//! `models/*.model.toml`, or the gate fails: it can't check a manifest it doesn't scan.
//!
//! The source scanner fails closed. Anything it can't follow is an error naming the file and line, never a silent skip:
//! a `register_model` token that isn't a direct `register_model!("<id>", include_str!("<path>"))` call (an alias, a
//! re-export, a use inside another macro's definition), an `include!` whose path isn't a string literal or doesn't
//! resolve to a file in the crate's `src/`, and a licence file that doesn't parse. Every `include!` is followed,
//! whatever the extension of the file, and the file is scanned like any other source. The one exception is
//! `include!(concat!(env!("OUT_DIR"), …))`, the file `build.rs` generates, which holds no source to read.
//!
//! Checked: every `models/*.model.toml` of a workspace crate whose name doesn't end in `-mock`. It runs no build, only
//! parsing and reads. `cargo xtask check` runs it on the whole repo, since it is cheap.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use omuzaki_ai_api::{ModelManifest, Origin};
use xtask::registry::lex::{Tok, Token, tokens};

use crate::lint_licences::register::{self, Named};
use crate::workspace::{Package, Workspace};

/// The statuses a shipped model may rest on (`docs/licensing/policy.md`, Verdicts).
const ALLOWED_STATUS: [&str; 2] = ["yes", "conditions"];

/// The folder of licence files the register is generated from, relative to the workspace root.
const REGISTER: &str = "docs/licensing/deps";

/// One licence file: its `item`, `status` and `condition` (the last names the licence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    item: String,
    status: String,
    condition: String,
}

/// The rows of the licence files `load` read, in file-name order. This is the data `register::render` builds the
/// register page from, so the gate and the page can't disagree.
pub fn rows(entries: &[Named]) -> Vec<Row> {
    entries
        .iter()
        .map(|(_, entry)| Row {
            item: entry.item.clone(),
            status: entry.status.clone(),
            condition: entry.condition.clone(),
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
            None => out.push(format!("no licence file for `{item}` in {REGISTER}/")),
            Some(row) if !ALLOWED_STATUS.contains(&row.status.as_str()) => out.push(format!(
                "the licence file for `{item}` in {REGISTER}/ is `{}`; a shipped model needs `yes` or `conditions`",
                row.status
            )),
            Some(row) => {
                if let Some(spdx) = &manifest.card.licence.spdx {
                    if !names_licence(&row.condition, spdx) {
                        out.push(format!(
                            "the licence file for `{item}` in {REGISTER}/ doesn't name its licence `{spdx}`"
                        ));
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

/// Whether an SPDX expression names a non-commercial licence (`CC-BY-NC-4.0`, `PolyForm-Noncommercial-1.0.0`, in any
/// case): nothing non-commercial ships.
fn is_non_commercial(spdx: &str) -> bool {
    spdx_tokens(spdx).iter().map(|token| token.to_ascii_lowercase()).any(|token| {
        token.contains("noncommercial") || token.contains("non-commercial") || token.split('-').any(|part| part == "nc")
    })
}

/// Every problem with the `register_model!` calls of one crate that no manifest check can see: a call whose literals
/// can't be read, and one that includes anything but `<crate>/models/<name>.model.toml`, the only files the gate scans.
/// An `include!` of a file outside `<crate>/src/`, or of one missing from `sources`, is a problem too: the gate never
/// reads it, so it could hide a registration. `include!(concat!(env!("OUT_DIR"), …))` (a file `build.rs` generates) is
/// the one include it allows. `dir` is the crate's workspace-relative folder; `sources` the `(path, text)` of its `.rs`
/// files and of every file they `include!` ([`with_includes`]). Either separator
/// (`/` or `\`) may be used, since Windows paths come back with `\`.
fn unchecked_registrations(dir: &str, sources: &[(String, String)]) -> Vec<String> {
    let dir = normalise(dir);
    let mut out = Vec::new();
    for (path, source) in sources {
        let path = path.replace('\\', "/");
        let from = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        for registration in registrations(source) {
            let include = match registration {
                Ok((_, include)) => include,
                Err(reason) => {
                    out.push(format!("{path}: {reason}"));
                    continue;
                }
            };
            let resolved = normalise(&format!("{from}/{include}"));
            let name = resolved.strip_prefix(&format!("{dir}/models/"));
            if !name.is_some_and(|name| name.ends_with(".model.toml") && !name.contains('/')) {
                out.push(format!(
                    "{path}: `register_model!` includes `{include}`, which is not a `models/*.model.toml` of its own \
                     crate, so the gate can't check its licence"
                ));
            }
        }
        for include in source_includes(source) {
            match include {
                Err(reason) => out.push(format!("{path}: {reason}")),
                Ok(None) => {}
                Ok(Some(include)) => {
                    let resolved = normalise(&format!("{from}/{include}"));
                    if !resolved.starts_with(&format!("{dir}/src/")) {
                        out.push(format!(
                            "{path}: `include!` reads `{include}`, outside the crate's `src/`, which the gate doesn't scan \
                             for `register_model!`"
                        ));
                    } else if !sources.iter().any(|(other, _)| normalise(other) == resolved) {
                        out.push(format!(
                            "{path}: `include!` reads `{include}`, which is not a file the gate could read, so it can't \
                             scan it for `register_model!`"
                        ));
                    }
                }
            }
        }
    }
    out
}

/// What each `include!` in one Rust source reads: `Ok(Some(path))` for a string literal, `Ok(None)` for the generated
/// `concat!(env!("OUT_DIR"), …)` form, `Err` for anything else (the gate can't tell what it includes).
fn source_includes(source: &str) -> Vec<Result<Option<String>, String>> {
    let toks = tokens(source);
    let mut found = Vec::new();
    for at in 0..toks.len() {
        if !toks[at].is_ident("include") || !is_punct(&toks, at + 1, '!') || toks.get(at + 2).and_then(closer).is_none()
        {
            continue;
        }
        let line = toks[at].line;
        if let Some(path) = toks.get(at + 3).and_then(string_value) {
            found.push(path.map(Some).map_err(|reason| format!("line {line}: {reason}")));
            continue;
        }
        let generated = toks.get(at + 3).is_some_and(|t| t.is_ident("concat"))
            && is_punct(&toks, at + 4, '!')
            && toks.get(at + 5).and_then(closer).is_some()
            && toks.get(at + 6).is_some_and(|t| t.is_ident("env"))
            && is_punct(&toks, at + 7, '!')
            && toks.get(at + 8).and_then(closer).is_some()
            && toks.get(at + 9).and_then(string_value).is_some_and(|name| name.as_deref() == Ok("OUT_DIR"));
        found.push(if generated {
            Ok(None)
        } else {
            Err(format!("line {line}: `include!` isn't a string literal or `concat!(env!(\"OUT_DIR\"), …)`"))
        });
    }
    found
}

/// A path with `\` read as `/` and its `.` and `..` segments resolved (a `..` past the start stays, so it never matches).
fn normalise(path: &str) -> String {
    let path = path.replace('\\', "/");
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
    let bodies = macro_bodies(&toks);
    for at in 0..toks.len() {
        if !toks[at].is_ident("register_model") {
            continue;
        }
        let line = toks[at].line;
        // The definition `macro_rules! register_model { … }` is the macro itself, not a use of it.
        if at >= 2 && toks[at - 1].is_punct('!') && toks[at - 2].is_ident("macro_rules") {
            continue;
        }
        if bodies.iter().any(|body| body.contains(&at)) {
            found.push(Err(format!(
                "line {line}: `register_model` inside another macro's definition, which the gate can't read"
            )));
            continue;
        }
        if !is_punct(&toks, at + 1, '!') {
            if !is_plain_import(&toks, at) {
                found.push(Err(format!(
                    "line {line}: `register_model` isn't a direct `register_model!(\"<id>\", include_str!(\"<path>\"))` \
                     call or a plain `use`; an alias or any other path hides what it registers"
                )));
            }
            continue;
        }
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

/// The token ranges of every `macro_rules! name { … }` body in `toks`, delimiters included.
fn macro_bodies(toks: &[Token]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    for at in 0..toks.len() {
        if !(toks[at].is_ident("macro_rules") && is_punct(toks, at + 1, '!')) {
            continue;
        }
        let open = at + 3;
        if toks.get(open).and_then(closer).is_none() {
            continue;
        }
        let mut depth = 0usize;
        let mut end = toks.len();
        for (index, token) in toks.iter().enumerate().skip(open) {
            if closer(token).is_some() {
                depth += 1;
            } else if [')', ']', '}'].iter().any(|&c| token.is_punct(c)) {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = index + 1;
                    break;
                }
            }
        }
        out.push(open..end);
    }
    out
}

/// Whether the `register_model` at `at` (not followed by `!`) is only imported by a `use` item: no `as` rename in
/// either direction, and the name ends the path. The nearest earlier `;` or `use` tells whether `at` is in a `use`,
/// since every `use` item ends in a `;`.
fn is_plain_import(toks: &[Token], at: usize) -> bool {
    let in_use =
        toks[..at].iter().rev().find(|t| t.is_punct(';') || t.is_ident("use")).is_some_and(|t| t.is_ident("use"));
    let renamed_to = at > 0 && toks[at - 1].is_ident("as");
    let ends = toks.get(at + 1).is_some_and(|t| t.is_punct(',') || t.is_punct(';') || t.is_punct('}'));
    in_use && !renamed_to && ends
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

/// `sources` plus every file they `include!` (whatever its extension) that lies in the crate's `src/` and can be read,
/// followed until no new file turns up. An include that can't be read is left out, and [`unchecked_registrations`]
/// reports it.
fn with_includes(root: &Path, dir: &str, sources: &mut Vec<(String, String)>) {
    let dir = normalise(dir);
    let mut next = 0;
    while next < sources.len() {
        let path = sources[next].0.replace('\\', "/");
        let from = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let wanted: Vec<String> = source_includes(&sources[next].1)
            .into_iter()
            .filter_map(|include| include.ok().flatten())
            .map(|include| normalise(&format!("{from}/{include}")))
            .filter(|resolved| resolved.starts_with(&format!("{dir}/src/")))
            .collect();
        for resolved in wanted {
            if sources.iter().any(|(other, _)| normalise(other) == resolved) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(root.join(&resolved)) {
                sources.push((resolved, text));
            }
        }
        next += 1;
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
    let (entries, broken) = register::load(&workspace.root.join(REGISTER));
    let register = rows(&entries);
    let mut found: Vec<String> =
        broken.iter().map(|(name, message)| format!("{REGISTER}/{name}.toml: cannot be read: {message}")).collect();
    let mut checked = 0;
    let mut skipped = 0;
    for package in &workspace.packages {
        let manifests = manifests(&workspace.root, package);
        if package.name.ends_with("-mock") {
            skipped += manifests.len();
            continue;
        }
        let mut sources = Vec::new();
        rust_sources(&workspace.root, &workspace.root.join(&package.dir).join("src"), &mut sources);
        with_includes(&workspace.root, &package.dir.to_string_lossy(), &mut sources);
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

    /// The licence files of the fixture register: `(file stem, item, status, condition)`.
    const DEPS: [(&str, &str, &str, &str); 2] = [
        ("tiny-model", "Tiny model", "yes", "keep notice; MIT OR Apache-2.0"),
        ("blocked-model", "Blocked model", "NO", "CC BY-NC"),
    ];

    /// Writes the fixture register under `root`, one TOML file per item as `docs/licensing/deps/` holds them.
    fn write_deps(root: &Path, tiny_status: &str) {
        let deps = root.join(REGISTER);
        fs::create_dir_all(&deps).unwrap();
        for (stem, item, status, condition) in DEPS {
            let status = if stem == "tiny-model" { tiny_status } else { status };
            let text = format!(
                "section = \"models\"\nitem = \"{item}\"\nkind = \"model\"\nused_for = \"a test\"\nstatus = \"{status}\"\n\
                 condition = \"{condition}\"\naudit = \"A1\"\n"
            );
            fs::write(deps.join(format!("{stem}.toml")), text).unwrap();
        }
    }

    fn row(item: &str, status: &str, condition: &str) -> Row {
        Row { item: item.to_owned(), status: status.to_owned(), condition: condition.to_owned() }
    }

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
        DEPS.iter().map(|(_, item, status, condition)| row(item, status, condition)).collect()
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
        assert!(found[0].contains("no licence file for `Other model`"), "{found:?}");
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
    fn the_rows_come_from_the_licence_files_the_register_page_is_rendered_from() {
        let dir = tempfile::tempdir().unwrap();
        write_deps(dir.path(), "yes");
        let (entries, broken) = register::load(&dir.path().join(REGISTER));
        assert!(broken.is_empty(), "{broken:?}");
        assert_eq!(rows(&entries), register().into_iter().rev().collect::<Vec<_>>());
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
    fn an_aliased_or_hidden_register_model_fails_the_gate() {
        for text in [
            r#"use omuzaki_ai_api::register_model as reg; reg!("x", include_str!("../models/x.model.toml"));"#,
            "use omuzaki_ai_api::{register_model as reg};",
            "use omuzaki_ai_api::other as register_model;",
            "let f = register_model;",
            "pub use omuzaki_ai_api::register_model::inner;",
            r#"macro_rules! wrap { () => { register_model!("x", include_str!("../models/x.model.toml")); } }"#,
            r#"macro_rules! wrap { ($m:expr) => { $crate::register_model!("x", $m); } }"#,
        ] {
            let found = unchecked(text);
            assert!(!found.is_empty(), "{text}");
            assert!(
                found
                    .iter()
                    .all(|f| f.contains("crates/omuzaki-tiny/src/lib.rs: line 1: `register_model`")
                        || f.contains("isn't")),
                "{found:?}"
            );
        }
        let aliased = unchecked("use omuzaki_ai_api::register_model as reg;");
        assert!(aliased[0].contains("an alias or any other path hides what it registers"), "{aliased:?}");
        for text in [
            "use omuzaki_ai_api::register_model;",
            "use omuzaki_ai_api::{SlotId, register_model, SlotInfo};",
            "pub use omuzaki_ai_api::register_model;",
            r#"omuzaki_ai_api::register_model!("x", include_str!("../models/x.model.toml"));"#,
        ] {
            assert_eq!(unchecked(text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn an_included_file_of_any_extension_is_scanned() {
        let sources = [
            ("crates/omuzaki-tiny/src/lib.rs".to_owned(), r#"include!("registrations.inc");"#.to_owned()),
            (
                "crates/omuzaki-tiny/src/registrations.inc".to_owned(),
                r#"register_model!("other", include_str!("../models/tiny.model.toml"));"#.to_owned(),
            ),
        ];
        assert_eq!(unchecked_registrations("crates/omuzaki-tiny", &sources), Vec::<String>::new());
        let found = problems(FILE, &manifest("tiny", HTTPS), &register(), &sources);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("registrations.inc registers `other` for this file"), "{found:?}");
        let outside = [
            sources[0].clone(),
            (sources[1].0.clone(), r#"register_model!("x", include_str!("../assets/x.toml"));"#.to_owned()),
        ];
        assert_eq!(unchecked_registrations("crates/omuzaki-tiny", &outside).len(), 1);
    }

    #[test]
    fn an_include_the_gate_cannot_resolve_fails_the_gate() {
        let found = unchecked(r#"include!("registrations.inc");"#);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("src/lib.rs: `include!` reads `registrations.inc`"), "{found:?}");
    }

    #[test]
    fn includes_are_followed_from_disk_through_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_deps(root, "yes");
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("models")).unwrap();
        fs::create_dir_all(tiny.join("src/sub")).unwrap();
        fs::write(tiny.join("models/tiny.model.toml"), manifest("tiny", HTTPS)).unwrap();
        fs::write(tiny.join("src/lib.rs"), r#"include!("sub/a.inc");"#).unwrap();
        fs::write(tiny.join("src/sub/a.inc"), r#"include!("b.txt");"#).unwrap();
        let nested = |id: &str| good_source(id)[0].1.replace("../models", "../../models");
        fs::write(tiny.join("src/sub/b.txt"), nested("tiny")).unwrap();
        let mut ws = workspace(&[("omuzaki-tiny", &[])]);
        ws.root = root.to_path_buf();
        assert!(report(&ws));

        fs::write(tiny.join("src/sub/b.txt"), nested("other")).unwrap();
        assert!(!report(&ws), "the id of the registration in the nested include is checked");
        fs::remove_file(tiny.join("src/sub/b.txt")).unwrap();
        assert!(!report(&ws), "a missing include fails");
    }

    #[test]
    fn a_model_naming_a_licence_file_passes_and_an_unknown_name_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_deps(root, "yes");
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("models")).unwrap();
        fs::create_dir_all(tiny.join("src")).unwrap();
        fs::write(tiny.join("src/lib.rs"), good_source("tiny")[0].1.as_str()).unwrap();
        fs::write(tiny.join("models/tiny.model.toml"), manifest("tiny", HTTPS)).unwrap();
        let mut ws = workspace(&[("omuzaki-tiny", &[])]);
        ws.root = root.to_path_buf();
        assert!(report(&ws));
        fs::write(tiny.join("models/tiny.model.toml"), manifest("tiny", HTTPS).replace("Tiny model", "Unknown"))
            .unwrap();
        assert!(!report(&ws));
        fs::remove_dir_all(root.join(REGISTER)).unwrap();
        assert!(!report(&ws), "no licence folder fails closed");
    }

    #[test]
    fn a_crate_without_models_is_scanned_too() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_deps(root, "yes");
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("src")).unwrap();
        fs::write(tiny.join("src/lib.rs"), r#"register_model!("x", include_str!("../assets/x.toml"));"#).unwrap();
        let mut ws = workspace(&[("omuzaki-tiny", &[])]);
        ws.root = root.to_path_buf();
        assert!(!report(&ws));
    }

    #[test]
    fn every_non_commercial_spelling_is_refused() {
        for spdx in [
            "CC-BY-NC-4.0",
            "cc-by-nc-sa-4.0",
            "PolyForm-Noncommercial-1.0.0",
            "polyform-noncommercial-1.0.0",
            "LicenseRef-Non-Commercial",
            "MIT OR PolyForm-NonCommercial-1.0.0",
            "CC-BY-NC-ND-3.0",
        ] {
            assert!(is_non_commercial(spdx), "{spdx}");
        }
        for spdx in ["MIT", "Apache-2.0 WITH LLVM-exception", "CC-BY-4.0", "BSD-3-Clause", "ONC-1.0"] {
            assert!(!is_non_commercial(spdx), "{spdx}");
        }
    }

    #[test]
    fn windows_paths_resolve_like_unix_ones() {
        let call = r#"register_model!("x", include_str!("../models/x.model.toml"));"#;
        let sources = [("crates\\omuzaki-tiny\\src\\lib.rs".to_owned(), call.to_owned())];
        assert_eq!(unchecked_registrations("crates\\omuzaki-tiny", &sources), Vec::<String>::new());
        let back = r#"register_model!("x", include_str!("..\\models\\x.model.toml"));"#;
        let sources = [("crates\\omuzaki-tiny\\src\\lib.rs".to_owned(), back.to_owned())];
        assert_eq!(unchecked_registrations("crates/omuzaki-tiny", &sources), Vec::<String>::new());
        let outside = r#"register_model!("x", include_str!("..\\assets\\x.toml"));"#;
        let sources = [("crates\\omuzaki-tiny\\src\\lib.rs".to_owned(), outside.to_owned())];
        assert_eq!(unchecked_registrations("crates\\omuzaki-tiny", &sources).len(), 1);
    }

    #[test]
    fn an_include_outside_src_fails_the_gate() {
        for text in [
            r#"include!("../registration.rs");"#,
            r#"include!("../../omuzaki-other/src/lib.rs");"#,
            r#"include! { "..\\registration.rs" }"#,
            "include!(REGISTRATION);",
            r#"include!(concat!(env!("CARGO_MANIFEST_DIR"), "/registration.rs"));"#,
        ] {
            assert_eq!(unchecked(text).len(), 1, "{text}: {:?}", unchecked(text));
        }
    }

    #[test]
    fn an_include_inside_src_or_generated_by_build_rs_is_allowed() {
        let present = |name: &str| (format!("crates/omuzaki-tiny/src/{name}"), String::new());
        for text in [r#"include!("registrations.rs");"#, r#"include!("./sub/registrations.rs");"#] {
            let sources = [
                ("crates/omuzaki-tiny/src/lib.rs".to_owned(), text.to_owned()),
                present("registrations.rs"),
                present("sub/registrations.rs"),
            ];
            assert_eq!(unchecked_registrations("crates/omuzaki-tiny", &sources), Vec::<String>::new(), "{text}");
        }
        for text in [
            r#"include!(concat!(env!("OUT_DIR"), "/ops_mods.rs"));"#,
            r#"// include!("../registration.rs");"#,
            r#"include_str!("../README.md");"#,
        ] {
            assert_eq!(unchecked(text), Vec::<String>::new(), "{text}");
        }
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
        let rows = vec![row("Tiny model", "yes", "MIT-0; keep notice")];
        let text = manifest("tiny", HTTPS).replace("MIT OR Apache-2.0\"\n", "MIT\"\n");
        let found = problems(FILE, &text, &rows, &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("doesn't name its licence `MIT`"), "{found:?}");
    }

    #[test]
    fn a_non_commercial_licence_fails_even_when_the_row_mentions_it() {
        let rows = vec![row("Tiny model", "yes", "not CC-BY-NC-4.0; keep notice")];
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
        write_deps(root, "yes");
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("models")).unwrap();
        fs::create_dir_all(tiny.join("src")).unwrap();
        fs::write(tiny.join("models/tiny.model.toml"), manifest("tiny", HTTPS)).unwrap();
        fs::write(tiny.join("src/lib.rs"), good_source("tiny")[0].1.as_str()).unwrap();
        let mut ws = workspace(&[("omuzaki-tiny", &[])]);
        ws.root = root.to_path_buf();
        assert!(report(&ws));

        write_deps(root, "NO");
        assert!(!report(&ws));
    }

    #[test]
    fn the_gate_runs_on_a_workspace_and_exempts_mock_crates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_deps(root, "yes");
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
