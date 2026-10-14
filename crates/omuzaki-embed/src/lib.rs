//! Embeds a folder of data files into a crate at build time (ADR 0019 D14, ADR 0011).
//!
//! A build script calls [`embed_dir`] and the crate includes the result:
//!
//! ```ignore
//! // build.rs
//! fn main() {
//!     let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
//!     omuzaki_embed::embed_dir("library/seed", out.join("seed.rs")).unwrap();
//! }
//!
//! // src/library_items.rs
//! const SEED: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/seed.rs"));
//! ```
//!
//! The table holds `(path relative to the folder, file contents)`, sorted by path, so the output is the same whatever
//! order the file system lists the files in. The contents are `include_str!` of the file's absolute path, so the
//! data is compiled in (wasm needs no file system) and cargo rebuilds the crate when a file changes.
//!
//! [`modules`] does the same for a folder of Rust modules (ADR 0054): the build script writes one
//! `#[path = "<absolute file>"] <vis> mod <name>;` per child, sorted by name, and the folder's own `mod.rs` includes
//! the list, so adding a feature is adding a file and no shared list is edited:
//!
//! ```ignore
//! // build.rs
//! omuzaki_embed::modules("src/ops", out.join("ops_mods.rs"), "pub").unwrap();
//!
//! // src/ops/mod.rs
//! include!(concat!(env!("OUT_DIR"), "/ops_mods.rs"));
//! ```
//!
//! Why a lib crate and not a copy in each owning crate: four crates embed seed folders, and the other data folders
//! (body shapes, skin tones, poses...) use the same helper instead of listing their files by hand.
#![deny(missing_docs)]

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

/// The embedded table as Rust source, and every path whose change must rebuild the crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// A Rust expression of type `&[(&str, &str)]`.
    pub source: String,
    /// The folder itself, then each folder under it and each embedded file, for `cargo:rerun-if-changed`. The
    /// folder is listed even when absent, so creating it later triggers a rebuild.
    pub watched: Vec<PathBuf>,
}

/// Builds the table for `dir` without writing anything. An absent folder gives an empty table.
///
/// Files whose name starts with `.` are skipped (editor droppings, `.gitkeep`). Every other file must be UTF-8,
/// because it is embedded with `include_str!`.
///
/// # Errors
/// An I/O error reading the folder, or a path that is not valid UTF-8.
pub fn table(dir: impl AsRef<Path>) -> io::Result<Table> {
    let dir = dir.as_ref();
    let mut watched = vec![dir.to_path_buf()];
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if dir.is_dir() {
        collect(dir, "", &mut files, &mut watched)?;
    }
    files.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    watched[1..].sort_unstable();

    let mut source = String::from("&[\n");
    for (relative, absolute) in &files {
        // `{:?}` escapes quotes, backslashes (Windows paths) and control characters into a valid Rust literal.
        let absolute = absolute.to_string_lossy();
        writeln!(source, "    ({relative:?}, include_str!({absolute:?})),").map_err(io::Error::other)?;
    }
    source.push_str("]\n");
    Ok(Table { source, watched })
}

/// Writes the table for `dir` to `out_file` and prints the `cargo:rerun-if-changed` lines for the folder and each
/// file. Call it from `build.rs`, with `out_file` under `OUT_DIR`.
///
/// # Errors
/// As [`table`], or an I/O error writing `out_file`.
pub fn embed_dir(dir: impl AsRef<Path>, out_file: impl AsRef<Path>) -> io::Result<()> {
    let table = table(dir)?;
    for path in &table.watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    std::fs::write(out_file, table.source)
}

/// Rust keywords, which cannot name a module without `r#` (so a file with one of these names is an error).
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
    "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become",
    "box", "do", "final", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield", "gen",
];

fn is_module_name(name: &str) -> bool {
    let mut chars = name.chars();
    let starts_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    starts_ok && name != "_" && chars.all(|c| c.is_ascii_alphanumeric() || c == '_') && !KEYWORDS.contains(&name)
}

/// Builds the module declarations for the children of `dir` without writing anything; see [`modules`].
///
/// The result's `source` holds one `#[path = "<absolute path>"] <visibility> mod <name>;` line per child, sorted by
/// name. `watched` is the folder, then each sub-folder (a `mod.rs` appearing in one changes it) and each declared
/// file, so `cargo:rerun-if-changed` fires when a module is added or removed.
///
/// # Errors
/// An I/O error reading the folder (an absent folder is an error: the module list would silently be empty), a name
/// that is not valid UTF-8, a name that is not a plain Rust identifier (for example `my-file.rs`, or a keyword), or
/// a file `foo.rs` next to a folder `foo/` (use `foo/mod.rs`; see [`modules`]).
pub fn module_list(dir: impl AsRef<Path>, visibility: &str) -> io::Result<Table> {
    let dir = dir.as_ref();
    let bad = |message: String| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut children: Vec<(String, PathBuf)> = Vec::new();
    let mut watched = vec![dir.to_path_buf()];
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_name =
            entry.file_name().into_string().map_err(|n| bad(format!("{} is not valid UTF-8", n.to_string_lossy())))?;
        if file_name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let (name, file) = if entry.file_type()?.is_dir() {
            watched.push(path.clone());
            let file = path.join("mod.rs");
            if !file.is_file() {
                continue; // not a module folder (assets, fixtures...)
            }
            (file_name, file)
        } else {
            if let Some(stem) = file_name.strip_suffix(".rs") {
                let folder = dir.join(stem);
                if folder.is_dir() {
                    // `#[path]` makes `foo.rs` resolve `mod inner;` as a sibling, not as `foo/inner.rs`.
                    return Err(bad(format!(
                        "{} and {} both exist; a module with children must be the folder layout, so move {} to {}",
                        path.display(),
                        folder.display(),
                        path.display(),
                        folder.join("mod.rs").display()
                    )));
                }
            }
            match file_name.strip_suffix(".rs") {
                // The folder's own `mod.rs` includes this list; `lib.rs`/`main.rs` are crate roots.
                Some("mod" | "lib" | "main") | None => continue,
                Some(stem) => (stem.to_owned(), path),
            }
        };
        if !is_module_name(&name) {
            return Err(bad(format!(
                "{}: `{name}` is not a valid Rust module name; rename it (letters, digits and `_`, not a keyword)",
                file.display()
            )));
        }
        children.push((name, file));
    }
    children.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    watched[1..].sort_unstable();
    watched.extend(children.iter().filter(|(_, f)| f.parent() == Some(dir)).map(|(_, f)| f.clone()));
    let visibility = if visibility.is_empty() { String::new() } else { format!("{visibility} ") };
    let mut source = String::new();
    for (name, file) in &children {
        // `{:?}` escapes backslashes (Windows paths) and quotes into a valid Rust string literal.
        let absolute = std::path::absolute(file)?;
        writeln!(source, "#[path = {:?}] {visibility}mod {name};", absolute.to_string_lossy())
            .map_err(io::Error::other)?;
    }
    Ok(Table { source, watched })
}

/// Writes the module declarations for the children of `dir` to `out_file` and prints the `cargo:rerun-if-changed`
/// lines. Call it from `build.rs`, with `out_file` under `OUT_DIR`, and `include!` the file from the folder's
/// `mod.rs`.
///
/// Declared: every `.rs` file and every sub-folder holding a `mod.rs`, sorted by name. Skipped: the folder's own
/// `mod.rs`, `lib.rs`, `main.rs`, names starting with `.`, other file types and sub-folders without a `mod.rs`.
/// A module with children must use the folder layout, `foo/mod.rs` with its children inside `foo/`: the generated
/// `#[path]` makes a `foo.rs` look for `mod inner;` next to itself instead of in `foo/`, so a `foo.rs` beside a
/// folder `foo/` is an error rather than a silent wrong lookup.
///
/// `visibility` is written before `mod` (`"pub"`, `"pub(crate)"`, or `""` for private).
///
/// # Errors
/// As [`module_list`], or an I/O error writing `out_file`.
pub fn modules(dir: impl AsRef<Path>, out_file: impl AsRef<Path>, visibility: &str) -> io::Result<()> {
    let list = module_list(dir, visibility)?;
    for path in &list.watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    std::fs::write(out_file, list.source)
}

fn collect(dir: &Path, prefix: &str, files: &mut Vec<(String, PathBuf)>, watched: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|name| {
            io::Error::new(io::ErrorKind::InvalidData, format!("{} is not valid UTF-8", name.to_string_lossy()))
        })?;
        if name.starts_with('.') {
            continue;
        }
        let relative = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            watched.push(path.clone());
            collect(&path, &relative, files, watched)?;
        } else {
            watched.push(path.clone());
            files.push((relative, std::path::absolute(&path)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn the_table_is_sorted_by_relative_path_and_embeds_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        // Written in reverse order, so a listing in creation order would differ from the sorted one.
        write(dir.path(), "hair/twintails.toml", "b");
        write(dir.path(), "body-shapes/curvy.toml", "a");
        write(dir.path(), "body-shapes/athletic.toml", "a");
        let t = table(dir.path()).unwrap();
        let names: Vec<&str> =
            t.source.lines().filter_map(|l| l.trim().strip_prefix("(\"")?.split('"').next()).collect();
        assert_eq!(names, ["body-shapes/athletic.toml", "body-shapes/curvy.toml", "hair/twintails.toml"]);
        let absolute = std::path::absolute(dir.path().join("hair/twintails.toml")).unwrap();
        assert!(t.source.contains(&format!("include_str!({absolute:?})")), "{}", t.source);
    }

    #[test]
    fn the_output_does_not_depend_on_the_order_files_were_created() {
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for name in ["a.toml", "b.toml", "c/d.toml"] {
            write(one.path(), name, "x");
        }
        for name in ["c/d.toml", "b.toml", "a.toml"] {
            write(two.path(), name, "x");
        }
        let relative = |t: &Table, root: &Path| {
            t.source.replace(&format!("{:?}", std::path::absolute(root).unwrap()).trim_matches('"').to_owned(), "ROOT")
        };
        let (a, b) = (table(one.path()).unwrap(), table(two.path()).unwrap());
        assert_eq!(relative(&a, one.path()), relative(&b, two.path()));
    }

    #[test]
    fn an_absent_folder_is_an_empty_table_that_is_still_watched() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("library/seed");
        let t = table(&missing).unwrap();
        assert_eq!(t.source, "&[\n]\n");
        assert_eq!(t.watched, [missing]);
    }

    #[test]
    fn the_folder_each_subfolder_and_each_file_are_watched() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a/x.toml", "x");
        write(dir.path(), "b.toml", "y");
        let t = table(dir.path()).unwrap();
        let expected =
            [dir.path().to_path_buf(), dir.path().join("a"), dir.path().join("a/x.toml"), dir.path().join("b.toml")];
        assert_eq!(t.watched, expected);
    }

    #[test]
    fn dotfiles_are_skipped_and_odd_characters_are_escaped() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitkeep", "");
        write(dir.path(), "q\"uote.toml", "x");
        let t = table(dir.path()).unwrap();
        assert!(!t.source.contains("gitkeep"));
        assert!(t.source.contains(r#"("q\"uote.toml", include_str!("#), "{}", t.source);
    }

    #[test]
    fn embed_dir_writes_the_table_to_the_out_file() {
        let (dir, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(dir.path(), "a.toml", "x");
        let out_file = out.path().join("seed.rs");
        embed_dir(dir.path(), &out_file).unwrap();
        assert_eq!(std::fs::read_to_string(out_file).unwrap(), table(dir.path()).unwrap().source);
    }
}
