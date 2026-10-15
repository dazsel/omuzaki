//! `cargo xtask docs` and `cargo xtask lint-crate-table`: the crate table, generated from each crate's own manifest
//! (ADR 0053 §3).
//!
//! Why: the hand-written table in `crates/README.md` was edited by nearly every pull request that added or changed a
//! crate, so parallel work collided on it. Each crate now describes itself under `[package.metadata.omuzaki]` (`kind`
//! and `summary`), `docs` prints the table from those, and the lint keeps the README to the rules.

use std::fs;
use std::process::ExitCode;

use xtask::registry::Error as ReadError;

use crate::error::{Error, Result};
use crate::lint_deps::CrateKind;
use crate::workspace::{Package, Workspace};

/// The README that keeps only the rules.
const README: &str = "crates/README.md";

/// One member under `crates/`, as its manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Package name.
    pub name: String,
    /// `kind` under `[package.metadata.omuzaki]`, if declared.
    pub kind: Option<String>,
    /// `summary` under `[package.metadata.omuzaki]`, if declared.
    pub summary: Option<String>,
    /// What `lint-deps` makes of the crate (its name and `layer`), to check `kind` against; `None` if it can't say.
    pub layering: Option<CrateKind>,
}

/// The rows for every member under `crates/`, sorted by name (the workspace is).
///
/// # Errors
/// A manifest that can't be read or parsed.
pub fn rows(workspace: &Workspace) -> Result<Vec<Row>> {
    workspace.packages.iter().filter(|package| package.dir.starts_with("crates")).map(|p| row(workspace, p)).collect()
}

fn row(workspace: &Workspace, package: &Package) -> Result<Row> {
    let path = workspace.root.join(&package.dir).join("Cargo.toml");
    let text = fs::read_to_string(&path).map_err(|source| ReadError::Read { path: path.clone(), source })?;
    let manifest: toml::Table = text.parse().map_err(|error: toml::de::Error| Error::Failed {
        command: format!("parse {}", path.display()),
        stderr: error.to_string(),
    })?;
    let ours = manifest.get("package").and_then(|p| p.get("metadata")).and_then(|m| m.get("omuzaki"));
    let text_of = |key: &str| ours.and_then(|table| table.get(key)?.as_str()).map(str::to_owned);
    Ok(Row {
        name: package.name.clone(),
        kind: text_of("kind"),
        summary: text_of("summary"),
        layering: CrateKind::of_package(package).ok(),
    })
}

/// The `kind` value a manifest writes for each layering class.
fn kind_name(kind: CrateKind) -> &'static str {
    match kind {
        CrateKind::Api => "API",
        CrateKind::Mock => "Mock",
        CrateKind::Lib => "Lib",
        CrateKind::Implementation => "Implementation",
        CrateKind::App => "App",
    }
}

/// The Markdown table, one row per crate.
pub fn table(rows: &[Row]) -> String {
    let mut out = String::from("| Crate | Kind | What it is |\n|---|---|---|\n");
    for row in rows {
        let kind = row.kind.as_deref().unwrap_or("");
        let summary = row.summary.as_deref().unwrap_or("");
        out += &["| `", &row.name, "` | ", kind, " | ", summary, " |\n"].concat();
    }
    out
}

/// Every problem: a crate without its metadata, a `kind` that disagrees with the layering, a table in the README.
pub fn violations(rows: &[Row], readme: &str) -> Vec<String> {
    let mut found = Vec::new();
    for row in rows {
        match (&row.kind, row.layering) {
            (None, _) => found.push(format!("{}: no `kind` under [package.metadata.omuzaki]", row.name)),
            (Some(kind), Some(layering)) if kind != kind_name(layering) => found.push(format!(
                "{}: kind {kind:?}, but lint-deps classifies it as {:?}",
                row.name,
                kind_name(layering)
            )),
            _ => {}
        }
        if row.summary.as_deref().is_none_or(|summary| summary.trim().is_empty()) {
            found.push(format!("{}: no `summary` under [package.metadata.omuzaki]", row.name));
        }
    }
    for (number, line) in readme.lines().enumerate() {
        if is_crate_row(line) {
            found.push(format!(
                "{README}:{}: a crate table row; the table is generated (`cargo xtask docs`)",
                number + 1
            ));
        }
    }
    found
}

/// A Markdown table row whose first cell is one code span (a crate name in backticks), the shape of a crate table row.
fn is_crate_row(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('|') else { return false };
    let cell = rest.split('|').next().unwrap_or("").trim();
    cell.len() > 2 && cell.starts_with('`') && cell.ends_with('`') && !cell[1..cell.len() - 1].contains(['`', ' '])
}

/// The `docs` command: prints the crate table.
///
/// # Errors
/// A manifest that can't be read or parsed.
pub fn docs(workspace: &Workspace) -> Result<ExitCode> {
    print!("{}", table(&rows(workspace)?));
    Ok(ExitCode::SUCCESS)
}

/// The `lint-crate-table` command.
///
/// # Errors
/// A manifest or the README that can't be read.
pub fn lint(workspace: &Workspace) -> Result<ExitCode> {
    let rows = rows(workspace)?;
    let path = workspace.root.join(README);
    let readme = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(ReadError::Read { path, source }.into()),
    };
    let found = violations(&rows, &readme);
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        println!("lint-crate-table: ok ({} crates)", rows.len());
        Ok(ExitCode::SUCCESS)
    } else {
        println!("lint-crate-table: {} violation(s); each crate describes itself in its Cargo.toml", found.len());
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, kind: Option<&str>, summary: Option<&str>, layering: CrateKind) -> Row {
        Row {
            name: name.to_owned(),
            kind: kind.map(str::to_owned),
            summary: summary.map(str::to_owned),
            layering: Some(layering),
        }
    }

    #[test]
    fn the_table_has_one_row_per_crate_in_order() {
        let rows = [
            row("omuzaki-core", Some("API"), Some("ids, `glam`"), CrateKind::Api),
            row("omuzaki-gltf", Some("Lib"), Some("glTF 2.0"), CrateKind::Lib),
        ];
        assert_eq!(
            table(&rows),
            "| Crate | Kind | What it is |\n|---|---|---|\n\
             | `omuzaki-core` | API | ids, `glam` |\n| `omuzaki-gltf` | Lib | glTF 2.0 |\n"
        );
    }

    #[test]
    fn missing_metadata_and_a_wrong_kind_are_caught() {
        let rows = [
            row("omuzaki-a", None, Some("x"), CrateKind::Implementation),
            row("omuzaki-b-api", Some("Mock"), Some(" "), CrateKind::Api),
            row("omuzaki-c", Some("Implementation"), Some("fine"), CrateKind::Implementation),
        ];
        assert_eq!(
            violations(&rows, ""),
            [
                "omuzaki-a: no `kind` under [package.metadata.omuzaki]",
                "omuzaki-b-api: kind \"Mock\", but lint-deps classifies it as \"API\"",
                "omuzaki-b-api: no `summary` under [package.metadata.omuzaki]",
            ]
        );
    }

    #[test]
    fn a_crate_row_in_the_readme_is_caught_but_the_rules_table_is_not() {
        let readme = "# Rules\n\n| Kind | Names |\n|---|---|\n| **API** | `omuzaki-core`, `*-api` |\n\n\
                      | Crate | Kind |\n|---|---|\n| `omuzaki-core` | API |\n";
        assert_eq!(
            violations(&[], readme),
            ["crates/README.md:9: a crate table row; the table is generated (`cargo xtask docs`)"]
        );
        assert!(!is_crate_row("| `a b` | x |"));
        assert!(!is_crate_row("text with | `pipe` |"));
    }
}
