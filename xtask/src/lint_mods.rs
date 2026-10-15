//! `cargo xtask lint-mods`: a folder whose module list is generated declares nothing by hand (ADR 0054 §1).
//!
//! Why: `omuzaki_embed::modules` writes the `mod` lines for a folder's children to `OUT_DIR`, and the folder's
//! declaring file `include!`s them, so a new feature is a new file and no list is edited. A hand-written `mod name;`
//! beside that include declares the module twice and brings the merge conflicts back. A crate opts a folder in with
//! `generated-mods` under `[package.metadata.omuzaki]` (paths relative to the crate); crates without the key are skipped.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use xtask::registry::Error as ReadError;

use crate::error::{Error, Result};
use crate::workspace::Workspace;

/// The metadata key a crate uses to opt folders in.
const KEY: &str = "generated-mods";

/// A crate that opted in, and the folders whose module list is generated.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OptIn {
    /// Package name.
    name: String,
    /// The crate's directory, relative to the workspace root.
    dir: std::path::PathBuf,
    /// The folders, relative to the crate.
    folders: Vec<String>,
}

fn failed(what: &str, stderr: String) -> Error {
    Error::Failed { command: what.to_owned(), stderr }
}

/// The `generated-mods` folders of a manifest, or `None` if it has no such key.
///
/// # Errors
/// A key that is not an array of strings: bad input is an error, never a skip.
fn folders_of(manifest: &toml::Table, name: &str) -> Result<Option<Vec<String>>> {
    let Some(value) = manifest.get("package").and_then(|p| p.get("metadata")).and_then(|m| m.get("omuzaki")) else {
        return Ok(None);
    };
    let Some(value) = value.get(KEY) else { return Ok(None) };
    let bad = || {
        failed(
            &format!("lint-mods {name}"),
            format!("`{KEY}` under [package.metadata.omuzaki] must be an array of strings"),
        )
    };
    let array = value.as_array().ok_or_else(bad)?;
    array.iter().map(|item| item.as_str().map(str::to_owned).ok_or_else(bad)).collect::<Result<Vec<_>>>().map(Some)
}

fn opt_ins(workspace: &Workspace) -> Result<Vec<OptIn>> {
    let mut found = Vec::new();
    for package in workspace.packages.iter().filter(|package| package.dir.starts_with("crates")) {
        let path = workspace.root.join(&package.dir).join("Cargo.toml");
        let text = fs::read_to_string(&path).map_err(|source| ReadError::Read { path: path.clone(), source })?;
        let manifest: toml::Table = text
            .parse()
            .map_err(|error: toml::de::Error| failed(&format!("parse {}", path.display()), error.to_string()))?;
        if let Some(folders) = folders_of(&manifest, &package.name)? {
            found.push(OptIn { name: package.name.clone(), dir: package.dir.clone(), folders });
        }
    }
    Ok(found)
}

/// The names of the modules a folder's files and sub-folders would declare, sorted.
fn children(folder: &Path) -> Result<Vec<String>> {
    let read = |source| ReadError::Read { path: folder.to_path_buf(), source };
    let mut names = Vec::new();
    for entry in fs::read_dir(folder).map_err(read)? {
        let entry = entry.map_err(read)?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map_err(read)?.is_dir();
        let name = if is_dir { Some(file_name.as_str()) } else { file_name.strip_suffix(".rs") };
        if let Some(name) = name.filter(|name| *name != "mod" && !name.starts_with('.')) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// Whether a line is the `include!` of a generated module list.
fn is_generated_include(line: &str) -> bool {
    let squeezed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    !squeezed.starts_with("//") && squeezed.contains("include!(concat!(env!(\"OUT_DIR\"),")
}

/// The module a line declares without a body (`mod name;`, with any attributes and visibility), if it is one. Only an
/// unindented line counts: an indented one sits inside another item's body, such as `mod tests { ... }`.
fn bodyless_mod(line: &str) -> Option<&str> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let mut rest = line.trim();
    while let Some(attribute) = rest.strip_prefix("#[") {
        let mut depth = 1;
        let end = attribute.char_indices().find_map(|(index, c)| {
            depth += i32::from(c == '[') - i32::from(c == ']');
            (depth == 0).then_some(index)
        })?;
        rest = attribute[end + 1..].trim_start();
    }
    if let Some(after) = rest.strip_prefix("pub") {
        let after = after.trim_start();
        rest = match after.strip_prefix('(') {
            Some(inner) => inner.split_once(')')?.1,
            None => after,
        };
    }
    let name = rest.trim_start().strip_prefix("mod")?.strip_prefix(char::is_whitespace)?.trim_start();
    let name = name.strip_suffix(';')?.trim_end();
    let name = name.strip_prefix("r#").unwrap_or(name);
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(name)
}

/// Every problem with one opted-in folder of the crate at `crate_root`.
fn folder_violations(name: &str, crate_root: &Path, folder: &str) -> Result<Vec<String>> {
    let dir = crate_root.join(folder);
    let declaring = if folder == "src" { "src/lib.rs".to_owned() } else { format!("{folder}/mod.rs") };
    if !dir.is_dir() {
        return Ok(vec![format!("{name}: `{KEY}` lists {folder}, which is not a folder")]);
    }
    let path = crate_root.join(&declaring);
    if !path.is_file() {
        return Ok(vec![format!("{name}: {declaring} is missing; it declares the modules of {folder}")]);
    }
    let text = fs::read_to_string(&path).map_err(|source| ReadError::Read { path: path.clone(), source })?;
    let mut found = Vec::new();
    if !text.lines().any(is_generated_include) {
        found.push(format!(
            "{name}: {declaring} has no `include!(concat!(env!(\"OUT_DIR\"), ...))`; it opts in but declares nothing generated"
        ));
    }
    let children = children(&dir)?;
    for (number, line) in text.lines().enumerate() {
        if let Some(module) = bodyless_mod(line).filter(|module| children.iter().any(|child| child == module)) {
            found.push(format!(
                "{name}: {declaring}:{}: hand-written `mod {module};`; {folder} generates its module list (`omuzaki_embed::modules`)",
                number + 1
            ));
        }
    }
    Ok(found)
}

fn violations(workspace: &Workspace, opted: &[OptIn]) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for krate in opted {
        let root = workspace.root.join(&krate.dir);
        for folder in &krate.folders {
            found.extend(folder_violations(&krate.name, &root, folder)?);
        }
    }
    Ok(found)
}

/// The `lint-mods` command.
///
/// # Errors
/// A manifest that can't be read or parsed, a `generated-mods` that isn't an array of strings, or a file that can't be read.
pub fn lint(workspace: &Workspace) -> Result<ExitCode> {
    let opted = opt_ins(workspace)?;
    let found = violations(workspace, &opted)?;
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        println!("lint-mods: ok ({} crates opted in)", opted.len());
        Ok(ExitCode::SUCCESS)
    } else {
        println!(
            "lint-mods: {} violation(s); a folder in `{KEY}` declares its modules only through the generated list",
            found.len()
        );
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INCLUDE: &str = "include!(concat!(env!(\"OUT_DIR\"), \"/ops_mods.rs\"));\n";

    /// A crate root with `src/ops/` holding `files`, and `mod_rs` as `src/ops/mod.rs`.
    fn krate(mod_rs: &str, files: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("a temporary directory");
        let ops = root.path().join("src/ops");
        fs::create_dir_all(ops.join("nested")).expect("the folder");
        fs::write(ops.join("mod.rs"), mod_rs).expect("mod.rs");
        for file in files {
            fs::write(ops.join(file), "").expect("a child");
        }
        root
    }

    fn check(root: &tempfile::TempDir) -> Vec<String> {
        folder_violations("omuzaki-x", root.path(), "src/ops").expect("the folder reads")
    }

    #[test]
    fn a_clean_folder_passes_and_a_body_or_unrelated_mod_is_allowed() {
        let source =
            format!("//! docs\n// mod a;\n{INCLUDE}#[cfg(test)]\nmod tests {{\n    mod a;\n}}\nmod elsewhere;\n");
        assert_eq!(check(&krate(&source, &["a.rs"])), Vec::<String>::new());
    }

    #[test]
    fn a_hand_written_mod_for_a_child_is_caught_with_any_visibility_or_attribute() {
        let source = format!(
            "{INCLUDE}mod a;\npub mod b;\npub(crate) mod c ;\n#[cfg(test)] mod d;\n#[allow(dead_code)]\npub mod r#e;\n"
        );
        let found = check(&krate(&source, &["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]));
        let modules: Vec<&str> = found.iter().map(|line| line.split('`').nth(1).expect("a module")).collect();
        assert_eq!(modules, ["mod a;", "mod b;", "mod c;", "mod d;", "mod e;"]);
        assert!(found[0].starts_with("omuzaki-x: src/ops/mod.rs:2: "), "{found:?}");
    }

    #[test]
    fn a_child_folder_counts_as_a_child() {
        let found = check(&krate(&format!("{INCLUDE}mod nested;\n"), &[]));
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn a_missing_include_is_caught() {
        let found = check(&krate("// include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n", &["a.rs"]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("no `include!"), "{found:?}");
    }

    #[test]
    fn a_missing_folder_or_declaring_file_is_caught_and_src_declares_in_lib_rs() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let missing = |folder| folder_violations("omuzaki-x", root.path(), folder).expect("reads");
        assert!(missing("src/ops")[0].contains("not a folder"));
        fs::create_dir_all(root.path().join("src")).expect("src");
        assert!(missing("src")[0].contains("src/lib.rs is missing"));
        fs::write(root.path().join("src/lib.rs"), format!("{INCLUDE}mod ops;\n")).expect("lib.rs");
        fs::write(root.path().join("src/ops.rs"), "").expect("a child");
        let found = missing("src");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("src/lib.rs:2"), "{found:?}");
    }

    #[test]
    fn a_manifest_without_the_key_is_skipped_and_a_malformed_key_is_an_error() {
        let parse = |text: &str| folders_of(&text.parse::<toml::Table>().expect("toml"), "omuzaki-x");
        assert_eq!(parse("[package]\nname = \"x\"\n").expect("no metadata"), None);
        assert_eq!(parse("[package.metadata.omuzaki]\nkind = \"App\"\n").expect("no key"), None);
        let ok = parse("[package.metadata.omuzaki]\ngenerated-mods = [\"src\", \"src/ops\"]\n").expect("array");
        assert_eq!(ok, Some(vec!["src".to_owned(), "src/ops".to_owned()]));
        for bad in ["\"src/ops\"", "[1]", "[\"src\", 2]", "{ a = 1 }"] {
            let text = format!("[package.metadata.omuzaki]\ngenerated-mods = {bad}\n");
            assert!(parse(&text).is_err(), "{bad} must be an error");
        }
    }
}
