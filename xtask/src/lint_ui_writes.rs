//! `cargo xtask lint-ui-writes`: UI code changes the document only through the `Runner` (QG-11, spec v3 §39).
//!
//! Why: the editor, the CLI and the MCP surface must stay equal, and an op that is logged, undoable and replayable is
//! the only write they share. A panel that applies a patch itself bypasses all three. The lint reads UI sources as
//! tokens (the link gate's lexer, so comments and strings never count) and fails on three things: naming `DocWrite`,
//! calling `apply` (as a method or on a type), and naming the `omuzaki_doc` or `omuzaki_ops` implementation crates, under
//! any name a manifest gives them. The contracts
//! (`omuzaki_doc_api`, `omuzaki_ops_api`) stay allowed: the `Runner` is one of them.
//!
//! Limits, stated so review can judge the lint against them: it is lexical. It catches every spelling of the write
//! path (`.apply`, `Type::apply`, `<T as Tr>::apply`, called or only referenced), a `DocWrite` mention and an
//! implementation crate under any manifest name. It cannot follow a value through generics, macros or a re-exported
//! alias for an `apply`-free path; a module function named `apply` (the theme's) is told apart only by its lower-case
//! owner.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use xtask::registry::Error as ReadError;
use xtask::registry::lex::{Tok, Token, tokens};

use crate::error::{Error, Result};
use crate::workspace::Workspace;

/// Crates whose name starts with this are UI crates (`omuzaki-ui`, `omuzaki-ui-*`).
const UI_PREFIX: &str = "omuzaki-ui";

/// The files allowed to use the document and ops implementations, by workspace-relative path. This is the app's wiring
/// module, the one place that builds the executor and hands the UI its `Runner`; everything else in a UI crate gets
/// the `Runner` passed in.
const WIRING: &[&str] = &["crates/omuzaki-ui/src/wiring.rs"];

/// The implementation packages UI code must not reach the document through.
const IMPLEMENTATION_PACKAGES: &[&str] = &["omuzaki-doc", "omuzaki-ops"];

/// The names a UI crate's source can use for those packages: their own names, plus any other name a manifest gives
/// them with `package = "..."` (a Cargo rename, `ops_impl = { package = "omuzaki-ops" }`, hides the canonical name).
pub fn implementation_names(manifests: &[&toml::Table]) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = IMPLEMENTATION_PACKAGES.iter().map(|p| p.replace('-', "_")).collect();
    for manifest in manifests {
        collect_renames(manifest, &mut names);
    }
    names
}

/// Adds the key of every dependency table entry that names an implementation package, at any depth: the plain
/// sections, `[target.*.dependencies]` and the root's `[workspace.dependencies]`.
fn collect_renames(table: &toml::Table, names: &mut BTreeSet<String>) {
    for (key, value) in table {
        let Some(inner) = value.as_table() else { continue };
        let is_dependencies = matches!(key.as_str(), "dependencies" | "dev-dependencies" | "build-dependencies");
        if is_dependencies {
            for (alias, spec) in inner {
                let package = spec.as_table().and_then(|t| t.get("package")).and_then(toml::Value::as_str);
                if package.is_some_and(|p| IMPLEMENTATION_PACKAGES.contains(&p)) {
                    names.insert(alias.replace('-', "_"));
                }
            }
        } else {
            collect_renames(inner, names);
        }
    }
}

/// One use of the document that does not go through the `Runner`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The file, relative to the workspace root, with `/` separators.
    pub file: String,
    /// The 1-based line.
    pub line: u32,
    /// What was found and what to do instead.
    pub rule: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.rule)
    }
}

/// What is wrong in one file's `source`, which sits at `file` (relative to the workspace root).
pub fn scan(file: &str, source: &str, implementations: &BTreeSet<String>) -> Vec<Violation> {
    let toks = tokens(source);
    let wiring = WIRING.contains(&file);
    let mut found = Vec::new();
    let mut add = |line: u32, rule: String| found.push(Violation { file: file.to_owned(), line, rule });
    for (i, token) in toks.iter().enumerate() {
        let Tok::Ident(name) = &token.tok else { continue };
        if name == "DocWrite" {
            add(
                token.line,
                "`DocWrite` names the document's write side; UI code submits an op through the `Runner` instead (QG-11)"
                    .to_owned(),
            );
        } else if name == "apply" && is_write_call(&toks, i) {
            add(
                token.line,
                "`apply` on a document or patch writes outside the `Runner`; submit an op through it instead (QG-11)"
                    .to_owned(),
            );
        } else if !wiring && implementations.contains(name.as_str()) {
            add(
                token.line,
                format!(
                    "`{name}` is an implementation crate; UI code reaches the document through the `Runner` \
                     (`omuzaki_ops_api`), and only the wiring module may build the executor (QG-11)"
                ),
            );
        }
    }
    found
}

/// Whether the identifier `apply` at `i` names something that could write a document: a method (`doc.apply(`,
/// `doc.apply::<T>(`) or an associated function of a type (`Project::apply(`, `<Project as Patch>::apply(`), called or
/// not: `let f = Project::apply;` and `(Project::apply)(d, p)` write just the same. Type names are capitalised, so a
/// function in a module (`ds::apply(ctx, ..)`, the theme) is not one.
fn is_write_call(toks: &[Token], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    if toks[i - 1].is_punct('.') {
        return true;
    }
    let after_path = i >= 3 && toks[i - 1].is_punct(':') && toks[i - 2].is_punct(':');
    // `<Project as Patch>::apply(`: the path ends in `>`, a qualified type.
    after_path
        && (toks[i - 3].is_punct('>')
            || matches!(&toks[i - 3].tok, Tok::Ident(owner) if owner.starts_with(char::is_uppercase)))
}

/// Every violation in the UI crates' sources, in file order.
///
/// # Errors
/// An unreadable source directory or file.
pub fn violations(workspace: &Workspace) -> Result<Vec<Violation>> {
    let mut found = Vec::new();
    let root_manifest = read_manifest(&workspace.root.join("Cargo.toml"))?;
    for package in workspace.packages.iter().filter(|p| p.name.starts_with(UI_PREFIX)) {
        let manifest = read_manifest(&workspace.root.join(&package.dir).join("Cargo.toml"))?;
        let implementations = implementation_names(&[&root_manifest, &manifest]);
        for path in rust_files(&workspace.root.join(&package.dir).join("src"))? {
            let source = fs::read_to_string(&path).map_err(|source| ReadError::Read { path: path.clone(), source })?;
            let relative = path.strip_prefix(&workspace.root).unwrap_or(&path);
            let file: Vec<_> = relative.components().map(|c| c.as_os_str().to_string_lossy()).collect();
            found.extend(scan(&file.join("/"), &source, &implementations));
        }
    }
    Ok(found)
}

/// Reads a `Cargo.toml`; a missing file reads as empty (the in-memory test workspaces have none).
fn read_manifest(path: &Path) -> Result<toml::Table> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(source) => return Err(ReadError::Read { path: path.to_path_buf(), source }.into()),
    };
    text.parse().map_err(|error: toml::de::Error| Error::Failed {
        command: format!("parse {}", path.display()),
        stderr: error.to_string(),
    })
}

/// Every `.rs` file under `dir`, sorted; nothing if `dir` doesn't exist.
fn rust_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        if !next.is_dir() {
            continue;
        }
        let entries = fs::read_dir(&next).map_err(|source| ReadError::Read { path: next.clone(), source })?;
        for path in entries.filter_map(std::result::Result::ok).map(|e| e.path()) {
            if path.is_dir() {
                todo.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Prints every violation and a summary; returns whether there were none.
///
/// # Errors
/// An unreadable source file.
pub fn report(workspace: &Workspace) -> Result<bool> {
    let found = violations(workspace)?;
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        println!("lint-ui-writes: ok");
    } else {
        println!("lint-ui-writes: {} violation(s); UI code writes only through the Runner (QG-11)", found.len());
    }
    Ok(found.is_empty())
}

/// The `lint-ui-writes` command.
///
/// # Errors
/// An unreadable source file.
pub fn run(workspace: &Workspace) -> Result<ExitCode> {
    Ok(if report(workspace)? { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: &str = "crates/omuzaki-ui/src/panels/inspector.rs";
    const WIRING_FILE: &str = "crates/omuzaki-ui/src/wiring.rs";

    fn lines(file: &str, source: &str) -> Vec<String> {
        scan(file, source, &implementation_names(&[])).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_panel_calling_docwrite_apply_fails_with_file_and_line() {
        let source = "use omuzaki_doc_api::DocWrite;\nfn nudge(doc: &mut impl DocWrite, patch: Patch) {\n    doc.apply(patch);\n}\n";
        let found = lines(PANEL, source);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("crates/omuzaki-ui/src/panels/inspector.rs:1: `DocWrite`"));
        assert!(found[1].starts_with("crates/omuzaki-ui/src/panels/inspector.rs:2: `DocWrite`"));
        assert!(found[2].starts_with("crates/omuzaki-ui/src/panels/inspector.rs:3: `apply`"));
    }

    #[test]
    fn a_panel_using_the_runner_passes() {
        let source = "use omuzaki_ops_api::{Runner, OpRequest};\nfn nudge(runner: &dyn Runner) { runner.run(OpRequest::new(\"rig.nudge\")); }\n";
        assert_eq!(lines(PANEL, source), Vec::<String>::new());
    }

    #[test]
    fn the_wiring_module_may_build_the_executor() {
        let source = "use omuzaki_ops::Executor;\nfn build() { let _ = Executor::new(); omuzaki_doc::open(); }\n";
        assert_eq!(lines(WIRING_FILE, source), Vec::<String>::new());
        assert_eq!(lines(PANEL, source).len(), 2);
    }

    #[test]
    fn the_wiring_module_may_not_write_through_docwrite() {
        assert_eq!(lines(WIRING_FILE, "fn f(p: Patch) { doc.apply(p); }").len(), 1);
    }

    #[test]
    fn a_write_in_a_comment_or_a_string_is_not_flagged() {
        let source = "// doc.apply(patch) via DocWrite\n/* omuzaki_ops::Executor */\n/// DocWrite::apply\nfn f() -> &'static str {\n    \"doc.apply(patch); omuzaki_doc; DocWrite\"\n}\n";
        assert_eq!(lines(PANEL, source), Vec::<String>::new());
    }

    #[test]
    fn a_write_through_a_use_alias_is_flagged() {
        let found = lines(
            PANEL,
            "use omuzaki_doc_api::DocWrite as Writer;\nuse omuzaki_ops::Executor as Exec;\nfn f(w: &mut impl Writer) { w.apply(p); }\n",
        );
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].contains(":1:") && found[1].contains(":2:") && found[2].contains(":3:"));
    }

    #[test]
    fn a_function_named_apply_is_not_a_method_call() {
        assert_eq!(
            lines(PANEL, "fn f(ctx: &Context) { ds::apply(ctx); apply_settings(ctx); apply(ctx); }"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_associated_apply_on_a_reexported_type_is_flagged() {
        let found = lines(PANEL, "use crate::wiring::*;\nfn f(d: &mut Doc, p: Patch) { Project::apply(d, p); }\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains(":2:") && found[0].contains("`apply`"));
    }

    #[test]
    fn a_qualified_apply_is_flagged_and_a_theme_function_is_not() {
        let found = lines(PANEL, "fn f(d: &mut Doc, p: Patch) { <Project as Patch>::apply(d, p); ds::apply(ctx); }\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains(":1:"));
    }

    #[test]
    fn an_apply_reference_that_is_not_called_in_place_is_flagged() {
        let found = lines(
            PANEL,
            "fn f(d: &mut Doc, p: Patch) {\n    let g = Project::apply;\n    g(d, p);\n    (Project::apply)(d, p);\n    d.apply::<X>(p);\n    let h = d.apply;\n}\n",
        );
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(
            found[0].contains(":2:")
                && found[1].contains(":4:")
                && found[2].contains(":5:")
                && found[3].contains(":6:")
        );
    }

    #[test]
    fn a_cargo_rename_of_an_implementation_crate_is_flagged() {
        let manifest: toml::Table = "[dependencies]\nops_impl = { package = \"omuzaki-ops\", path = \"../omuzaki-ops\" }\n\
             [target.'cfg(unix)'.dev-dependencies]\ndoc-impl = { package = \"omuzaki-doc\" }\n[dependencies.other]\nversion = \"1\"\n"
            .parse()
            .expect("toml");
        let names = implementation_names(&[&manifest]);
        let found: Vec<String> =
            scan(PANEL, "use ops_impl::Executor;\nuse doc_impl::Project;\nuse other::Thing;\n", &names)
                .iter()
                .map(ToString::to_string)
                .collect();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains(":1:") && found[1].contains(":2:"));
    }

    #[test]
    fn only_ui_crates_are_scanned() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (krate, file) in [("omuzaki-ui-panels", "a.rs"), ("omuzaki-ops", "b.rs"), ("omuzaki-cli", "c.rs")] {
            let src = dir.path().join("crates").join(krate).join("src");
            fs::create_dir_all(&src).expect("mkdir");
            fs::write(src.join(file), "use omuzaki_ops::Executor;\n").expect("write");
        }
        let workspace = crate::workspace::testing::workspace(&[
            ("omuzaki-ui-panels", &[]),
            ("omuzaki-ops", &[]),
            ("omuzaki-cli", &[]),
        ]);
        let workspace = Workspace { root: dir.path().to_path_buf(), ..workspace };
        let found: Vec<String> = violations(&workspace).expect("scan").iter().map(ToString::to_string).collect();
        assert_eq!(found.len(), 1, "{found:?}"); // only omuzaki-ui-panels is a UI crate
        assert!(found[0].starts_with("crates/omuzaki-ui-panels/src/a.rs:1:"));
    }
}
