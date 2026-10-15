//! `cargo xtask models`: every shipped model is backed by an allowed entry in `docs/licensing/deps/` (ML-01, ADR 0017).
//!
//! Why: a model we ship must have a licence somebody checked. Its manifest names the register item it rests on
//! (`card.register`, the `item` of a `docs/licensing/deps/*.toml` file, which `register.md` is generated from) and the
//! SPDX licence it declares; this lint refuses a model whose entry is missing, not `yes` or
//! `conditions`, or that doesn't name the same licence. It also refuses a `register_model!` that registers a different id
//! for the file than the manifest says. Mock crates ship test fixtures, so their `models/` folders are exempt.
//!
//! A shipped model's lineage (`card.lineage`: base models, training data, code, conversions) is checked too, since a
//! model is only as clean as its chain. It must document at least one base model or training data link, and every
//! link must name an `https://` source `url` and a licence that is exactly one entry of the allow-list ([`ALLOWED_LICENCES`], the "Allowed"
//! section of the policy). Anything else fails, naming the model and the link: a banned licence, a non-commercial one,
//! a made-up id, words. See "What counts as documented lineage" in `docs/licensing/policy.md`.
//!
//! The licence comparison is exact: the entry's `condition` must name the manifest's whole SPDX expression as
//! whole tokens, and no non-commercial licence ships. Every `register_model!` of a crate must include one of its own
//! `models/*.model.toml`, or the gate fails: it can't check a manifest it doesn't scan.
//!
//! The source scanner fails closed. Anything it can't follow is an error naming the file and line, never a silent skip:
//! a `register_model` token that isn't a direct `register_model!("<id>", include_str!("<path>"))` call (an alias, a
//! re-export, a use inside another macro's definition), an `include!` whose path isn't a string literal or doesn't
//! resolve to a file in the crate's `src/`, and a licence file that doesn't parse. Every `include!` is followed,
//! whatever the extension of the file, and the file is scanned like any other source.
//!
//! A `#[path = "…"]` in a crate's `src/` is followed the same way: its target must resolve inside the crate's `src/`
//! (where every file is scanned) and be a file the gate can read; one that leaves `src/`, or isn't a string literal,
//! fails. A `[lib]` or `[[bin]]` `path` in the crate's `Cargo.toml` outside `src/` fails too, since the gate never
//! reads those files. The one exception is a short reviewed list of shared files ([`REVIEWED_PATHS`]), which are
//! followed and scanned like `src/`. (`tests/` and `benches/` are not shipped code and are not scanned.)
//!
//! `include!(concat!(env!("OUT_DIR"), "/<file>"))` holds the file `build.rs` generates, which the gate can't read, so it
//! is allowed only through [`GENERATED`], a table of `(crate, file, generator)`: the file is bound to the crate whose
//! reviewed `build.rs` generates it, and that `build.rs` must contain the generator's call. Nothing is allowed by file
//! name alone, so a file listed for one crate fails in another. Each generator emits only `mod` / `#[path]` lines into
//! the crate's own `src/` tree (every file there is scanned), `link()` calls, or data, and never a registration macro:
//!
//! - `ops_mods.rs`, `crate_mods.rs`: `omuzaki_embed::modules`, one `#[path = "<abs>"] pub mod <name>;` per file or
//!   folder of the crate's `src/`;
//! - `migrations.rs`: `omuzaki-doc`'s `module_decls`, the same `#[path]` + `mod` lines for `src/migrations/`;
//! - `link_calls.rs`: `omuzaki_link_build::emit`, one `<crate>::link();` per flagged dependency (`omuzaki-link-probe`
//!   may overwrite it with an empty block);
//! - `looks.rs`: `omuzaki-char`'s `build.rs`, a `static` table of `include_str!` of `library/looks/*.toml` (data).
//!
//! Any other pairing fails, so a new generator is added to the table in review, not by a crate on its own.
//!
//! Checked: every `models/*.model.toml` of a workspace crate whose name doesn't end in `-mock`. It runs no build, only
//! parsing and reads. `cargo xtask check` runs it on the whole repo, since it is cheap.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use omuzaki_ai_api::{LineageKind, ModelManifest, Origin};
use xtask::registry::lex::{Tok, Token, tokens};

use crate::lint_licences::register::{self, Named};
use crate::workspace::{Package, Workspace};

/// The statuses a shipped model may rest on (`docs/licensing/policy.md`, Verdicts).
const ALLOWED_STATUS: [&str; 2] = ["yes", "conditions"];

/// The folder of licence files the register is generated from, relative to the workspace root.
const REGISTER: &str = "docs/licensing/deps";

/// One licence file: its `item`, `kind`, `status` and `condition` (the last names the licence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    file: String,
    item: String,
    kind: String,
    status: String,
    condition: String,
}

/// The rows of the licence files `load` read, in file-name order. This is the data `register::render` builds the
/// register page from, so the gate and the page can't disagree.
pub fn rows(entries: &[Named]) -> Vec<Row> {
    entries
        .iter()
        .map(|(file, entry)| Row {
            file: file.clone(),
            item: entry.item.clone(),
            kind: entry.kind.clone(),
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
        let named: Vec<&Row> = register.iter().filter(|row| &row.item == item).collect();
        let models: Vec<&Row> = named.iter().copied().filter(|row| is_model_kind(&row.kind)).collect();
        let files =
            |rows: &[&Row]| rows.iter().map(|row| format!("`{}.toml`", row.file)).collect::<Vec<_>>().join(", ");
        // Exactly one model row must carry the item: with none the licence is unaudited, and with several the gate
        // can't tell which one the model rests on.
        match models.as_slice() {
            [] if named.is_empty() => out.push(format!("no licence file for `{item}` in {REGISTER}/")),
            [] => out.push(format!(
                "the licence file(s) for `{item}` in {REGISTER}/ ({}) are not a model; model `{}` needs a \
                 `kind = \"model\"` entry",
                files(&named),
                manifest.id
            )),
            [_, _, ..] => out.push(format!(
                "{} licence files in {REGISTER}/ name the model `{item}` ({}); exactly one is allowed",
                models.len(),
                files(&models)
            )),
            [row] if !ALLOWED_STATUS.contains(&row.status.as_str()) => out.push(format!(
                "the licence file for `{item}` in {REGISTER}/ is `{}`; a shipped model needs `yes` or `conditions`",
                row.status
            )),
            [row] => {
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
    out.extend(lineage_problems(&manifest));
    let file_name = base_name(file);
    for (path, source) in sources {
        for (id, include) in registrations(source).into_iter().flatten() {
            let include = base_name(&include);
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

/// The licences a lineage link may carry, each a whole accepted entry from the "Allowed" section of
/// `docs/licensing/policy.md` for code, models and data (as `deny.toml` lists whole licences for crates). A link's
/// licence must equal one entry exactly: expressions are not parsed, so `OR`, `AND`, parentheses and bare exception
/// ids all fail, and a dual-licensed source names the one licence we take it under. A new entry is added here with the
/// policy page, in review.
const ALLOWED_LICENCES: [&str; 18] = [
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "BSL-1.0",
    "0BSD",
    "Unlicense",
    "CC0-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "MPL-2.0",
    "OFL-1.1",
    "CC-BY-3.0",
    "CC-BY-4.0",
    "Apache-2.0 WITH LLVM-exception",
];

/// Every problem with a shipped model's lineage: none documented, or a link without an `https://` source or with a
/// missing or disallowed licence. Each names the model and the link.
fn lineage_problems(manifest: &ModelManifest) -> Vec<String> {
    let id = &manifest.id;
    let lineage = &manifest.card.lineage;
    let mut out = Vec::new();
    if !lineage.iter().any(|link| matches!(link.kind, LineageKind::BaseModel | LineageKind::TrainingData)) {
        out.push(format!(
            "model `{id}` has no documented lineage; `card.lineage` needs a base_model or training_data link"
        ));
    }
    for link in lineage {
        let shown: String = link
            .name
            .chars()
            .map(|c| if matches!(c, ' '..='~') { c.to_string() } else { format!("\\u{{{:x}}}", u32::from(c)) })
            .collect();
        let named = format!("model `{id}`: lineage link `{shown}` ({})", kind_name(link.kind));
        if link.name.trim().is_empty() {
            out.push(format!("model `{id}`: a {} lineage link has a blank `name`", kind_name(link.kind)));
        } else if !link.name.chars().all(|c| matches!(c, ' '..='~')) {
            out.push(format!("{named} has a `name` with characters other than printable ASCII"));
        }
        let url = link.url.as_deref().filter(|url| !url.trim().is_empty());
        match url {
            None => out.push(format!("{named} names no source `url`")),
            Some(url) if !is_https_url(url) => {
                out.push(format!("{named} has the source `{url}`, which is not an `https://` URL with a host"));
            }
            Some(_) => {}
        }
        let at = url.map_or(String::new(), |url| format!(" ({url})"));
        match link.licence.as_deref().filter(|licence| !licence.trim().is_empty()) {
            None => out.push(format!("{named} names no licence{at}")),
            Some(licence) if !ALLOWED_LICENCES.contains(&licence) => out.push(format!(
                "{named} has the licence `{licence}`{at}; lineage licence must be one identifier from policy.md's \
                 Allowed list (docs/licensing/policy.md)"
            )),
            Some(_) => {}
        }
    }
    out
}

/// A lineage link's kind as `card.lineage` spells it in TOML.
fn kind_name(kind: LineageKind) -> &'static str {
    match kind {
        LineageKind::TrainingData => "training_data",
        LineageKind::BaseModel => "base_model",
        LineageKind::Code => "code",
        LineageKind::Conversion => "conversion",
    }
}

/// Whether `url` is `https://`, then a host of two or more dot-separated labels of letters, digits and inner `-`, an
/// optional `:port` (1 to 65535), and nothing or a `/`, `?` or `#` and the rest. It may hold only printable ASCII
/// without spaces (0x21 to 0x7E). Userinfo and IPv6 literals fail.
fn is_https_url(url: &str) -> bool {
    // Printable ASCII without spaces (0x21 to 0x7E), checked first: any other character anywhere fails.
    if !url.chars().all(|c| matches!(c, '!'..='~')) {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, port) = match rest[..end].split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (&rest[..end], None),
    };
    let label = |label: &str| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let port_ok = port.is_none_or(|port| {
        (1..=5).contains(&port.len())
            && port.chars().all(|c| c.is_ascii_digit())
            && port.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n))
    });
    host.contains('.') && host.split('.').all(label) && port_ok
}

/// The last segment of a path, with `\` read as `/` (Windows paths come back with `\`).
fn base_name(path: &str) -> String {
    path.replace('\\', "/").rsplit('/').next().unwrap_or_default().to_owned()
}

/// Whether a licence file's `kind` marks a model: `model`, or a `+` combination that includes it (`model+code`), the
/// way `docs/licensing/deps/*.toml` marks one.
fn is_model_kind(kind: &str) -> bool {
    kind.split('+').any(|part| part.trim() == "model")
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

/// The `OUT_DIR` files the gate allows, each bound to the crate whose reviewed `build.rs` generates it:
/// `(crate, file, generator)`. The generator is the call that must appear in that crate's `build.rs`. A file allowed for
/// one crate is not allowed for another, and nothing is allowed by file name alone. Each generator emits only `mod` /
/// `#[path]` lines into the crate's own `src/`, `link()` calls, or data, never a registration macro.
const GENERATED: [(&str, &str, &str); 12] = [
    ("omuzaki-anim", "crate_mods.rs", "omuzaki_embed::modules"),
    ("omuzaki-char", "ops_mods.rs", "omuzaki_embed::modules"),
    ("omuzaki-char", "looks.rs", "looks.rs"),
    ("omuzaki-cli", "link_calls.rs", "omuzaki_link_build::emit"),
    ("omuzaki-doc", "migrations.rs", "migration_files::module_decls"),
    ("omuzaki-face", "ops_mods.rs", "omuzaki_embed::modules"),
    ("omuzaki-io-ops", "ops_mods.rs", "omuzaki_embed::modules"),
    ("omuzaki-link-probe", "link_calls.rs", "omuzaki_link_build::emit"),
    ("omuzaki-stage", "crate_mods.rs", "omuzaki_embed::modules"),
    ("omuzaki-stage", "ops_mods.rs", "omuzaki_embed::modules"),
    ("omuzaki-stage", "link_calls.rs", "omuzaki_link_build::emit"),
    ("omuzaki-test-face", "link_calls.rs", "omuzaki_link_build::emit"),
];

impl Crate<'_> {
    /// Whether `file` in `OUT_DIR` is one this crate's reviewed `build.rs` generates ([`GENERATED`]).
    fn generates(&self, file: &str) -> bool {
        GENERATED.iter().any(|&(name, listed, generator)| {
            name == self.name && listed == file && self.build_script.is_some_and(|text| text.contains(generator))
        })
    }
}

/// Files outside a crate's `src/` that a `#[path]` may name, by workspace-relative path. A listed file is followed and
/// scanned like one in `src/`, not exempted. `omuzaki-doc`'s `build_support/migration_files.rs` is shared by its
/// `build.rs` (the migration module list) and its library; it registers nothing.
const REVIEWED_PATHS: [&str; 1] = ["crates/omuzaki-doc/build_support/migration_files.rs"];

/// A crate as [`unchecked_registrations`] sees it.
struct Crate<'a> {
    /// Its package name.
    name: &'a str,
    /// Its workspace-relative folder.
    dir: &'a str,
    /// The text of the folder's `build.rs`, if it has one.
    build_script: Option<&'a str>,
}

/// Every problem with the `register_model!` calls of one crate that no manifest check can see: a call whose literals
/// can't be read, and one that includes anything but `<crate>/models/<name>.model.toml`, the only files the gate scans.
/// An `include!` or `#[path]` of a file outside `<crate>/src/`, or of one missing from `sources`, is a problem too: the
/// gate never reads it, so it could hide a registration. So is an `include!(concat!(env!("OUT_DIR"), …))` of a file
/// that is not bound to this crate in [`GENERATED`]. `sources` is the `(path, text)` of the
/// crate's `.rs` files and of every file they `include!` or `#[path]` ([`with_includes`]). Either separator
/// (`/` or `\`) may be used, since Windows paths come back with `\`.
fn unchecked_registrations(krate: &Crate<'_>, sources: &[(String, String)]) -> Vec<String> {
    let dir = normalise(krate.dir);
    let in_src = |resolved: &str| resolved.starts_with(&format!("{dir}/src/"));
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
                Ok(Include::Generated(file)) => {
                    if !krate.generates(&file) {
                        out.push(format!(
                            "{path}: crate `{}` includes `{file}` from `OUT_DIR`, which the gate can't read and which \
                             is not a reviewed generator for this crate; add the (crate, file, generator) to \
                             GENERATED in xtask/src/models.rs in review",
                            krate.name
                        ));
                    }
                }
                Ok(Include::File(include)) => {
                    let resolved = normalise(&format!("{from}/{include}"));
                    if !in_src(&resolved) {
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
        for attribute in path_attributes(source) {
            match attribute {
                Err(reason) => out.push(format!("{path}: {reason}")),
                Ok((line, target)) => {
                    let resolved = normalise(&format!("{from}/{target}"));
                    let reviewed = REVIEWED_PATHS.contains(&resolved.as_str());
                    if is_absolute(&target) || !(in_src(&resolved) || reviewed) {
                        out.push(format!(
                            "{path}: line {line}: `#[path = \"{target}\"]` leaves the crate's `src/`, which the gate \
                             doesn't scan for `register_model!`"
                        ));
                    } else if !sources.iter().any(|(other, _)| normalise(other) == resolved) {
                        out.push(format!(
                            "{path}: line {line}: `#[path = \"{target}\"]` is not a file the gate could read, so it can't \
                             scan it for `register_model!`"
                        ));
                    }
                }
            }
        }
    }
    out
}

/// Whether a path is absolute on Unix or Windows (a leading separator, or a drive such as `C:`).
fn is_absolute(path: &str) -> bool {
    path.starts_with(['/', '\\']) || path.as_bytes().get(1) == Some(&b':')
}

/// Every problem with the `[lib]` and `[[bin]]` targets of one `Cargo.toml`: a `path` outside `src/` holds library or
/// binary code the gate never scans, and a manifest that doesn't parse can't be shown to have none.
fn target_problems(text: &str) -> Vec<String> {
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(error) => return vec![format!("Cargo.toml cannot be read: {error}")],
    };
    let mut targets = Vec::new();
    if let Some(lib) = table.get("lib") {
        targets.push(("[lib]", lib));
    }
    if let Some(toml::Value::Array(bins)) = table.get("bin") {
        targets.extend(bins.iter().map(|bin| ("[[bin]]", bin)));
    }
    let mut out = Vec::new();
    for (section, target) in targets {
        match target.get("path") {
            None => {}
            Some(toml::Value::String(path)) => {
                if is_absolute(path) || !normalise(path).starts_with("src/") {
                    out.push(format!(
                        "Cargo.toml: {section} `path = \"{path}\"` is outside `src/`, which the gate scans"
                    ));
                }
            }
            Some(_) => {
                out.push(format!("Cargo.toml: {section} `path` is not a string, so the gate can't tell where it is"));
            }
        }
    }
    out
}

/// What an `include!` reads: a file by path, or a file `build.rs` generates under `OUT_DIR`.
#[derive(Debug, PartialEq, Eq)]
enum Include {
    /// A string-literal path, relative to the including file.
    File(String),
    /// The name of the file in `concat!(env!("OUT_DIR"), "/<name>")`.
    Generated(String),
}

/// What each `include!` in one Rust source reads; `Err` for anything the gate can't tell the target of.
fn source_includes(source: &str) -> Vec<Result<Include, String>> {
    let toks = tokens(source);
    let mut found = Vec::new();
    for at in 0..toks.len() {
        if !toks[at].is_ident("include") || !is_punct(&toks, at + 1, '!') || toks.get(at + 2).and_then(closer).is_none()
        {
            continue;
        }
        let line = toks[at].line;
        if let Some(path) = toks.get(at + 3).and_then(string_value) {
            found.push(path.map(Include::File).map_err(|reason| format!("line {line}: {reason}")));
            continue;
        }
        found.push(generated_include(&toks, at + 3).ok_or_else(|| {
            format!("line {line}: `include!` isn't a string literal or `concat!(env!(\"OUT_DIR\"), \"/<file>\")`")
        }));
    }
    found
}

/// The file name of `concat!(env!("OUT_DIR"), "/<file>")` starting at `at`, with nothing else in the call.
fn generated_include(toks: &[Token], at: usize) -> Option<Include> {
    let concat_close = (toks.get(at)?.is_ident("concat") && is_punct(toks, at + 1, '!'))
        .then(|| toks.get(at + 2).and_then(closer))
        .flatten()?;
    let env_close = (toks.get(at + 3)?.is_ident("env") && is_punct(toks, at + 4, '!'))
        .then(|| toks.get(at + 5).and_then(closer))
        .flatten()?;
    let var = toks.get(at + 6).and_then(string_value)?.ok()?;
    if var != "OUT_DIR" || !is_punct(toks, at + 7, env_close) || !is_punct(toks, at + 8, ',') {
        return None;
    }
    let file = toks.get(at + 9).and_then(string_value)?.ok()?;
    let end = if is_punct(toks, at + 10, ',') { at + 11 } else { at + 10 };
    let name = file.strip_prefix('/').unwrap_or(&file);
    let closed = is_punct(toks, end, concat_close) && toks.get(end + 1).and_then(closer_of).is_some();
    (closed && !name.is_empty() && !name.contains(['/', '\\'])).then(|| Include::Generated(name.to_owned()))
}

/// Whether the token closes any delimiter (`)`, `]` or `}`).
fn closer_of(token: &Token) -> Option<char> {
    [')', ']', '}'].into_iter().find(|&c| token.is_punct(c))
}

/// The `(line, path)` of every `#[path = "…"]` in one Rust source, including one inside `#[cfg_attr(…, path = "…")]`;
/// `Err` for a `path =` that isn't a string literal, and for a relative `..` path inside an inline module, whose
/// directory the gate doesn't track.
fn path_attributes(source: &str) -> Vec<Result<(u32, String), String>> {
    let toks = tokens(source);
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut at = 0;
    while at < toks.len() {
        if toks[at].is_punct('{') {
            depth += 1;
        } else if toks[at].is_punct('}') {
            depth = depth.saturating_sub(1);
        }
        let inner = is_punct(&toks, at + 1, '!');
        if toks[at].is_punct('#') && is_punct(&toks, at + 1 + usize::from(inner), '[') {
            let open = at + 1 + usize::from(inner);
            let mut level = 0usize;
            let mut end = toks.len();
            for (index, token) in toks.iter().enumerate().skip(open) {
                if closer(token).is_some() {
                    level += 1;
                } else if closer_of(token).is_some() {
                    level = level.saturating_sub(1);
                    if level == 0 {
                        end = index;
                        break;
                    }
                }
            }
            for index in open..end {
                if !(toks[index].is_ident("path") && is_punct(&toks, index + 1, '=')) {
                    continue;
                }
                let line = toks[index].line;
                found.push(match toks.get(index + 2).and_then(string_value) {
                    Some(Ok(path)) if depth > 0 && path.contains("..") => Err(format!(
                        "line {line}: `#[path = \"{path}\"]` inside an inline module, where the gate can't resolve a `..`"
                    )),
                    Some(Ok(path)) => Ok((line, path)),
                    Some(Err(reason)) => Err(format!("line {line}: {reason}")),
                    None => Err(format!("line {line}: `#[path = …]` isn't a string literal")),
                });
            }
            at = end;
        }
        at += 1;
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

/// `sources` plus every file they `include!` or name in a `#[path]` (whatever its extension) that lies in the crate's `src/` and can be read,
/// followed until no new file turns up. A file that can't be read is left out, and [`unchecked_registrations`]
/// reports it.
fn with_includes(root: &Path, dir: &str, sources: &mut Vec<(String, String)>) {
    let dir = normalise(dir);
    let mut next = 0;
    while next < sources.len() {
        let path = sources[next].0.replace('\\', "/");
        let from = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let files = source_includes(&sources[next].1)
            .into_iter()
            .filter_map(|include| match include {
                Ok(Include::File(path)) => Some(path),
                _ => None,
            })
            .chain(path_attributes(&sources[next].1).into_iter().filter_map(|attribute| attribute.ok().map(|a| a.1)));
        let wanted: Vec<String> = files
            .map(|include| normalise(&format!("{from}/{include}")))
            .filter(|resolved| {
                resolved.starts_with(&format!("{dir}/src/")) || REVIEWED_PATHS.contains(&resolved.as_str())
            })
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
        let dir = package.dir.to_string_lossy();
        let cargo = workspace.root.join(&package.dir).join("Cargo.toml");
        if let Ok(text) = fs::read_to_string(&cargo) {
            found.extend(target_problems(&text).into_iter().map(|p| format!("{dir}/{p}")));
        }
        let build_script = fs::read_to_string(workspace.root.join(&package.dir).join("build.rs")).ok();
        let krate = Crate { name: &package.name, dir: &dir, build_script: build_script.as_deref() };
        found.extend(unchecked_registrations(&krate, &sources));
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

    /// A `build.rs` holding every reviewed generator's call.
    const BUILD: &str =
        "omuzaki_embed::modules(); omuzaki_link_build::emit(); migration_files::module_decls(); \"looks.rs\"";

    /// A crate in the fixture workspace, with a `build.rs`.
    fn krate(dir: &str) -> Crate<'_> {
        Crate { name: "omuzaki-stage", dir, build_script: Some(BUILD) }
    }

    fn row(item: &str, status: &str, condition: &str) -> Row {
        row_of_kind(item, "model", status, condition)
    }

    fn row_of_kind(item: &str, kind: &str, status: &str, condition: &str) -> Row {
        Row {
            file: item.to_lowercase().replace(' ', "-"),
            item: item.to_owned(),
            kind: kind.to_owned(),
            status: status.to_owned(),
            condition: condition.to_owned(),
        }
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

[[card.lineage]]
kind = "training_data"
name = "Tiny corpus 1"
licence = "CC0-1.0"
url = "https://data.example/tiny-corpus"
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
    fn a_lineage_licence_must_equal_one_allowed_entry() {
        let allowed = |licence: &str| ALLOWED_LICENCES.contains(&licence);
        for ok in ["MIT", "CC-BY-4.0", "Apache-2.0 WITH LLVM-exception"] {
            assert!(allowed(ok), "{ok}");
        }
        let bad = [
            "",
            "unknown",
            "research only",
            "research-only",
            "totally-made-up",
            "LicenseRef-custom",
            "mit",
            "NOASSERTION",
            "BUSL-1.1",
            "SSPL-1.0",
            "FSL-1.1-MIT",
            "CC-BY-NC-4.0",
            "MIT WITH Apache-2.0",
            "LLVM-exception",
            "MIT OR Apache-2.0",
            "MIT AND CC0-1.0",
            "(MIT)",
            "Apache-2.0 WITH Classpath-exception-2.0",
        ];
        for licence in bad {
            assert!(!allowed(licence), "{licence}");
        }
    }

    #[test]
    fn expressions_and_bare_exceptions_fail_a_lineage_link() {
        for licence in ["MIT WITH Apache-2.0", "LLVM-exception", "MIT OR Apache-2.0"] {
            let text = manifest("tiny", HTTPS).replace("CC0-1.0", licence);
            let found = problems(FILE, &text, &register(), &good_source("tiny"));
            assert_eq!(found.len(), 1, "{licence}: {found:?}");
            assert!(found[0].contains("must be one identifier from policy.md's Allowed list"), "{found:?}");
        }
        let text = manifest("tiny", HTTPS).replace("CC0-1.0", "Apache-2.0 WITH LLVM-exception");
        let found = problems(FILE, &text, &register(), &good_source("tiny"));
        assert!(found.is_empty(), "{found:?}");
    }

    /// `value` as the inside of a TOML basic string, with every character other than printable ASCII escaped, so a
    /// control or non-ASCII character reaches the gate as a value instead of breaking the TOML.
    fn toml_escaped(value: &str) -> String {
        value
            .chars()
            .map(|c| match c {
                ' '..='~' if c != '"' && c != '\\' => c.to_string(),
                _ => format!("\\u{:04X}", u32::from(c)),
            })
            .collect()
    }

    #[test]
    fn a_lineage_url_holds_only_printable_ascii_without_spaces() {
        let bad = [
            " https://data.example/x",
            "https://data.example/x ",
            "https://data.example/x\n",
            "https://data.example/x\u{0}",
            "https://data.example/\u{7f}",
            "https://data.example/é",
            "https://dаta.example/x",
        ];
        for url in bad {
            assert!(!is_https_url(url), "{url:?}");
            let text = manifest("tiny", HTTPS).replace("https://data.example/tiny-corpus", &toml_escaped(url));
            let found = problems(FILE, &text, &register(), &good_source("tiny"));
            assert_eq!(found.len(), 1, "{url:?}: {found:?}");
            assert!(found[0].contains("not an `https://` URL"), "{found:?}");
        }
    }

    #[test]
    fn a_lineage_name_is_printable_ascii() {
        for name in ["Tiny\u{0}corpus", "Tiny\u{7f}", "Tiny corpus é", "Tiny\ncorpus"] {
            let text = manifest("tiny", HTTPS).replace("Tiny corpus 1", &toml_escaped(name));
            let found = problems(FILE, &text, &register(), &good_source("tiny"));
            assert_eq!(found.len(), 1, "{name:?}: {found:?}");
            assert!(found[0].contains("printable ASCII") && !found[0].contains(name), "{found:?}");
        }
    }

    #[test]
    fn a_blank_lineage_name_fails() {
        let text = manifest("tiny", HTTPS).replace("Tiny corpus 1", "  ");
        let found = problems(FILE, &text, &register(), &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("blank `name`"), "{found:?}");
    }

    #[test]
    fn a_disallowed_lineage_licence_names_the_link_and_the_policy() {
        for licence in ["research-only", "totally-made-up", "BUSL-1.1", "CC-BY-NC-4.0"] {
            let text = manifest("tiny", HTTPS).replace("CC0-1.0", licence);
            let found = problems(FILE, &text, &register(), &good_source("tiny"));
            assert_eq!(found.len(), 1, "{licence}: {found:?}");
            assert!(
                found[0].contains("lineage link `Tiny corpus 1`")
                    && found[0].contains(licence)
                    && found[0].contains("docs/licensing/policy.md"),
                "{found:?}"
            );
        }
    }

    #[test]
    fn a_lineage_url_must_be_https_with_a_host() {
        for url in ["https://data.example/tiny", "https://data.example:8443?q=1", "https://a-b.example.com#frag"] {
            assert!(is_https_url(url), "{url}");
        }
        let bad = [
            "http://data.example/tiny",
            "unknown",
            "data/tiny",
            "https://",
            "https:///tiny",
            "https://data example/x",
            "https://[not-an-ipv6]",
            "https://[::1]/x",
            "https://example.com:bad-port",
            "https://example.com:99999",
            "https://example.com:0",
            "https://user@example.com",
            "https://localhost",
            "https://-a.example.com",
            "https://a-.example.com",
            "https://a..example.com",
            "https://example.com:",
        ];
        for url in bad {
            let text = manifest("tiny", HTTPS).replace("https://data.example/tiny-corpus", url);
            let found = problems(FILE, &text, &register(), &good_source("tiny"));
            assert_eq!(found.len(), 1, "{url}: {found:?}");
            assert!(found[0].contains("not an `https://` URL"), "{found:?}");
        }
    }

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
            &krate("crates/omuzaki-tiny"),
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
        assert_eq!(unchecked_registrations(&krate("crates/omuzaki-tiny"), &sources), Vec::<String>::new());
        let found = problems(FILE, &manifest("tiny", HTTPS), &register(), &sources);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("registrations.inc registers `other` for this file"), "{found:?}");
        let outside = [
            sources[0].clone(),
            (sources[1].0.clone(), r#"register_model!("x", include_str!("../assets/x.toml"));"#.to_owned()),
        ];
        assert_eq!(unchecked_registrations(&krate("crates/omuzaki-tiny"), &outside).len(), 1);
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
        assert_eq!(unchecked_registrations(&krate("crates\\omuzaki-tiny"), &sources), Vec::<String>::new());
        let back = r#"register_model!("x", include_str!("..\\models\\x.model.toml"));"#;
        let sources = [("crates\\omuzaki-tiny\\src\\lib.rs".to_owned(), back.to_owned())];
        assert_eq!(unchecked_registrations(&krate("crates/omuzaki-tiny"), &sources), Vec::<String>::new());
        let outside = r#"register_model!("x", include_str!("..\\assets\\x.toml"));"#;
        let sources = [("crates\\omuzaki-tiny\\src\\lib.rs".to_owned(), outside.to_owned())];
        assert_eq!(unchecked_registrations(&krate("crates\\omuzaki-tiny"), &sources).len(), 1);
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
            assert_eq!(
                unchecked_registrations(&krate("crates/omuzaki-tiny"), &sources),
                Vec::<String>::new(),
                "{text}"
            );
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

    #[test]
    fn a_row_that_is_not_a_model_fails_naming_the_row_and_the_model() {
        for kind in ["crate", "native", "code"] {
            let rows = vec![row_of_kind("Tiny model", kind, "yes", "MIT OR Apache-2.0")];
            let found = problems(FILE, &manifest("tiny", HTTPS), &rows, &good_source("tiny"));
            assert_eq!(found.len(), 1, "{kind}: {found:?}");
            assert!(found[0].contains("`Tiny model`") && found[0].contains("model `tiny`"), "{found:?}");
            assert!(found[0].contains("are not a model") && found[0].contains(".toml`"), "{found:?}");
        }
        for kind in ["model", "model+code", "model+crate"] {
            let rows = vec![row_of_kind("Tiny model", kind, "yes", "MIT OR Apache-2.0")];
            assert!(problems(FILE, &manifest("tiny", HTTPS), &rows, &good_source("tiny")).is_empty(), "{kind}");
        }
        let both = vec![
            row_of_kind("Tiny model", "crate", "yes", "MIT OR Apache-2.0"),
            row_of_kind("Tiny model", "model", "yes", "MIT OR Apache-2.0"),
        ];
        assert_eq!(problems(FILE, &manifest("tiny", HTTPS), &both, &good_source("tiny")), Vec::<String>::new());
    }

    #[test]
    fn a_path_attribute_leaving_src_fails_naming_the_file_and_the_attribute() {
        for target in ["../build/gen.rs", "../../omuzaki-other/src/lib.rs", "/abs/src/gen.rs", "C:/gen.rs"] {
            let found = unchecked(&format!("#[path = \"{target}\"]\nmod gen;"));
            assert_eq!(found.len(), 1, "{target}: {found:?}");
            assert!(found[0].contains("crates/omuzaki-tiny/src/lib.rs: line 1: `#[path"), "{found:?}");
            assert!(found[0].contains("leaves the crate's `src/`"), "{found:?}");
        }
        let cfg = unchecked("#[cfg_attr(unix, path = \"../gen.rs\")]\nmod gen;");
        assert_eq!(cfg.len(), 1, "{cfg:?}");
        for text in ["#[path = GEN]\nmod gen;", "#[path = concat!(\"a\", \"b\")]\nmod gen;"] {
            assert_eq!(unchecked(text).len(), 1, "{text}");
        }
        let inline = unchecked("mod outer {\n#[path = \"../x.rs\"]\nmod x;\n}");
        assert_eq!(inline.len(), 1, "{inline:?}");
        assert!(inline[0].contains("inline module"), "{inline:?}");
    }

    #[test]
    fn a_path_attribute_inside_src_is_followed_and_must_be_readable() {
        let sources = [
            ("crates/omuzaki-tiny/src/lib.rs".to_owned(), "#[path = \"sub/inner.rs\"]\nmod inner;".to_owned()),
            ("crates/omuzaki-tiny/src/sub/inner.rs".to_owned(), String::new()),
        ];
        assert_eq!(unchecked_registrations(&krate("crates/omuzaki-tiny"), &sources), Vec::<String>::new());
        let found = unchecked_registrations(&krate("crates/omuzaki-tiny"), &sources[..1]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("not a file the gate could read"), "{found:?}");
        let hidden = [
            sources[0].clone(),
            (
                "crates/omuzaki-tiny/src/sub/inner.rs".to_owned(),
                r#"register_model!("x", include_str!("../assets/x.toml"));"#.to_owned(),
            ),
        ];
        assert_eq!(unchecked_registrations(&krate("crates/omuzaki-tiny"), &hidden).len(), 1);
    }

    #[test]
    fn a_path_attribute_target_is_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_deps(root, "yes");
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("models")).unwrap();
        fs::create_dir_all(tiny.join("src/sub")).unwrap();
        fs::write(tiny.join("models/tiny.model.toml"), manifest("tiny", HTTPS)).unwrap();
        fs::write(tiny.join("src/lib.rs"), "#[path = \"sub/x.inc\"]\nmod x;").unwrap();
        fs::write(tiny.join("src/sub/x.inc"), good_source("tiny")[0].1.replace("../models", "../../models")).unwrap();
        let mut ws = workspace(&[("omuzaki-tiny", &[])]);
        ws.root = root.to_path_buf();
        assert!(report(&ws));
        fs::write(tiny.join("src/sub/x.inc"), good_source("other")[0].1.replace("../models", "../../models")).unwrap();
        assert!(!report(&ws), "the id registered in a `#[path]` file that isn't `.rs` is checked");
    }

    #[test]
    fn a_lib_or_bin_path_outside_src_fails() {
        for manifest in [
            "[lib]\npath = \"gen/lib.rs\"\n",
            "[lib]\npath = \"../elsewhere/lib.rs\"\n",
            "[[bin]]\nname = \"a\"\npath = \"src/main.rs\"\n[[bin]]\nname = \"b\"\npath = \"tools/b.rs\"\n",
            "[lib]\npath = 3\n",
            "[lib\n",
        ] {
            assert_eq!(target_problems(manifest).len(), 1, "{manifest}: {:?}", target_problems(manifest));
        }
        for manifest in
            ["[package]\nname = \"a\"\n", "[lib]\npath = \"src/lib.rs\"\n", "[[bin]]\npath = \"./src/bin/a.rs\"\n"]
        {
            assert_eq!(target_problems(manifest), Vec::<String>::new(), "{manifest}");
        }
        let found = target_problems("[lib]\npath = \"gen/lib.rs\"\n");
        assert!(found[0].contains("[lib]") && found[0].contains("gen/lib.rs"), "{found:?}");
    }

    #[test]
    fn a_custom_target_path_fails_the_gate_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_deps(root, "yes");
        let tiny = root.join("crates/omuzaki-tiny");
        fs::create_dir_all(tiny.join("src")).unwrap();
        fs::write(tiny.join("src/lib.rs"), "").unwrap();
        fs::write(tiny.join("Cargo.toml"), "[lib]\npath = \"src/lib.rs\"\n").unwrap();
        let mut ws = workspace(&[("omuzaki-tiny", &[])]);
        ws.root = root.to_path_buf();
        assert!(report(&ws));
        fs::write(tiny.join("Cargo.toml"), "[lib]\npath = \"gen/lib.rs\"\n").unwrap();
        assert!(!report(&ws));
    }

    #[test]
    fn an_out_dir_file_is_allowed_only_for_the_crate_whose_build_script_generates_it() {
        let included = |file: &str| {
            [(
                "crates/omuzaki-tiny/src/lib.rs".to_owned(),
                format!(r#"include!(concat!(env!("OUT_DIR"), "/{file}"));"#),
            )]
        };
        for (name, file, generator) in GENERATED {
            let allowed = Crate { name, dir: "crates/omuzaki-tiny", build_script: Some(generator) };
            assert_eq!(unchecked_registrations(&allowed, &included(file)), Vec::<String>::new(), "{name} {file}");
            let no_build =
                Crate { build_script: None, ..Crate { name, dir: "crates/omuzaki-tiny", build_script: None } };
            assert_eq!(unchecked_registrations(&no_build, &included(file)).len(), 1, "{name} {file}: no build.rs");
            let no_call = Crate { build_script: Some("fn main() {}"), ..Crate { name, dir: "x", build_script: None } };
            assert_eq!(unchecked_registrations(&no_call, &included(file)).len(), 1, "{name} {file}: no generator");
            // The same file under another crate that doesn't list it.
            let other = Crate { name: "omuzaki-other", dir: "crates/omuzaki-tiny", build_script: Some(BUILD) };
            assert_eq!(unchecked_registrations(&other, &included(file)).len(), 1, "{name} {file}: other crate");
        }
        // `looks.rs` is the char crate's only; the stage crate has no such pairing.
        let stage = Crate { name: "omuzaki-stage", dir: "crates/omuzaki-tiny", build_script: Some(BUILD) };
        let found = unchecked_registrations(&stage, &included("looks.rs"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("crate `omuzaki-stage`") && found[0].contains("`looks.rs`"), "{found:?}");
        assert!(found[0].contains("GENERATED"), "{found:?}");
        assert!(GENERATED.iter().all(|(_, file, _)| *file != "seed.rs"));
        for text in [
            r#"include!(concat!(env!("OUT_DIR"), "/registrations.rs"));"#,
            r#"include!(concat!(env!("OUT_DIR"), "/sub/ops_mods.rs"));"#,
            r#"include!(concat!(env!("OUT_DIR"), "/ops_mods.rs", "/extra"));"#,
            r#"include!(concat!(env!("OUT_DIR"), NAME));"#,
            r#"include!(concat!(env!("OUT_DIR")));"#,
        ] {
            assert_eq!(unchecked(text).len(), 1, "{text}");
        }
    }

    #[test]
    fn zero_or_several_model_rows_for_an_item_fail_naming_the_rows() {
        let mut twin = row("Tiny model", "yes", "MIT OR Apache-2.0");
        twin.file = "tiny-twin".to_owned();
        let mut first = row("Tiny model", "yes", "MIT OR Apache-2.0");
        first.file = "tiny-model".to_owned();
        let found = problems(FILE, &manifest("tiny", HTTPS), &[first.clone(), twin], &good_source("tiny"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("`tiny-model.toml`") && found[0].contains("`tiny-twin.toml`"), "{found:?}");
        assert!(found[0].contains("exactly one"), "{found:?}");
        let mut other = row_of_kind("Tiny model", "crate", "yes", "MIT OR Apache-2.0");
        other.file = "tiny-crate".to_owned();
        // A non-model row beside the one model row doesn't count.
        assert_eq!(problems(FILE, &manifest("tiny", HTTPS), &[first, other.clone()], &good_source("tiny")).len(), 0);
        let found = problems(FILE, &manifest("tiny", HTTPS), &[other], &good_source("tiny"));
        assert!(found[0].contains("`tiny-crate.toml`"), "{found:?}");
        assert!(found[0].contains("model `tiny`"), "{found:?}");
    }

    #[test]
    fn windows_separators_do_not_hide_a_registration_for_a_manifest() {
        let call = r#"register_model!("other", include_str!(r"..\models\tiny.model.toml"));"#;
        let sources = vec![("crates\\omuzaki-tiny\\src\\lib.rs".to_owned(), call.to_owned())];
        let windows_file = "crates\\omuzaki-tiny\\models\\tiny.model.toml";
        for file in [FILE, windows_file] {
            let found = problems(file, &manifest("tiny", HTTPS), &register(), &sources);
            assert_eq!(found.len(), 1, "{file}: {found:?}");
            assert!(found[0].contains("registers `other` for this file"), "{found:?}");
        }
        assert_eq!(base_name(r"..\models\tiny.model.toml"), "tiny.model.toml");
    }

    #[test]
    fn a_reviewed_path_outside_src_is_scanned_not_exempted() {
        let krate = Crate { name: "omuzaki-doc", dir: "crates/omuzaki-doc", build_script: None };
        let lib = (
            "crates/omuzaki-doc/src/lib.rs".to_owned(),
            "#[path = \"../build_support/migration_files.rs\"]\nmod m;".to_owned(),
        );
        let helper = |text: &str| (REVIEWED_PATHS[0].to_owned(), text.to_owned());
        let clean = [lib.clone(), helper("fn f() {}")];
        assert_eq!(unchecked_registrations(&krate, &clean), Vec::<String>::new());
        let registering = [lib.clone(), helper(r#"register_model!("x", include_str!("../assets/x.toml"));"#)];
        assert_eq!(unchecked_registrations(&krate, &registering).len(), 1);
        let missing = unchecked_registrations(&krate, &[lib]);
        assert!(missing[0].contains("not a file the gate could read"), "{missing:?}");
        let other = [(
            "crates/omuzaki-doc/src/lib.rs".to_owned(),
            "#[path = \"../build_support/other.rs\"]\nmod m;".to_owned(),
        )];
        assert_eq!(unchecked_registrations(&krate, &other).len(), 1);
    }
}
