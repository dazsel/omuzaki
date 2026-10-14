//! `modules` against a fixture folder (ADR 0054 step 1).
use std::path::{Path, PathBuf};

fn write(root: &Path, relative: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "").unwrap();
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // Created out of order, so a listing in creation order would differ from the sorted one.
    for f in [
        "zeta.rs",
        "mod.rs",
        "lib.rs",
        "main.rs",
        ".hidden.rs",
        "alpha.rs",
        "notes.txt",
        "group/mod.rs",
        "group/inner.rs",
        "assets/data.toml",
        ".git/mod.rs",
    ] {
        write(dir.path(), f);
    }
    dir
}

fn abs(root: &Path, relative: &str) -> String {
    format!("{:?}", std::path::absolute(root.join(relative)).unwrap().to_string_lossy())
}

#[test]
fn declares_files_and_module_folders_sorted_without_mod_mod() {
    let dir = fixture();
    let list = omuzaki_embed::module_list(dir.path(), "pub").unwrap();
    let r = dir.path();
    let expected = format!(
        "#[path = {}] pub mod alpha;\n#[path = {}] pub mod group;\n#[path = {}] pub mod zeta;\n",
        abs(r, "alpha.rs"),
        abs(r, "group/mod.rs"),
        abs(r, "zeta.rs")
    );
    assert_eq!(list.source, expected);
}

#[test]
fn private_visibility_has_no_stray_space() {
    let dir = fixture();
    let list = omuzaki_embed::module_list(dir.path(), "").unwrap();
    assert!(list.source.lines().all(|l| l.contains("] mod ")), "{}", list.source);
}

#[test]
fn watches_the_folder_subfolders_and_declared_files() {
    let dir = fixture();
    let list = omuzaki_embed::module_list(dir.path(), "pub").unwrap();
    let r = dir.path();
    let expected: Vec<PathBuf> = ["", "assets", "group", "alpha.rs", "zeta.rs"].iter().map(|p| r.join(p)).collect();
    let mut got = list.watched.clone();
    // `PathBuf::join("")` adds a trailing separator; compare by components.
    got.iter_mut().for_each(|p| *p = p.components().collect());
    let expected: Vec<PathBuf> = expected.iter().map(|p| p.components().collect()).collect();
    assert_eq!(got, expected);
}

#[test]
fn modules_writes_the_file_and_two_runs_are_identical() {
    let (dir, out) = (fixture(), tempfile::tempdir().unwrap());
    let (a, b) = (out.path().join("a.rs"), out.path().join("b.rs"));
    omuzaki_embed::modules(dir.path(), &a, "pub(crate)").unwrap();
    omuzaki_embed::modules(dir.path(), &b, "pub(crate)").unwrap();
    let (a, b) = (std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
    assert_eq!(a, b);
    assert_eq!(String::from_utf8(a).unwrap(), omuzaki_embed::module_list(dir.path(), "pub(crate)").unwrap().source);
}

#[test]
fn an_invalid_name_is_a_clear_error() {
    for bad in ["my-file.rs", "1st.rs", "type.rs", "café.rs", "_.rs"] {
        let dir = fixture();
        write(dir.path(), bad);
        let err = omuzaki_embed::module_list(dir.path(), "pub").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{bad}");
        assert!(err.to_string().contains("not a valid Rust module name"), "{bad}: {err}");
    }
}

#[test]
fn a_file_and_a_folder_with_the_same_name_are_an_error() {
    let dir = fixture();
    write(dir.path(), "group.rs");
    let err = omuzaki_embed::module_list(dir.path(), "pub").unwrap_err();
    assert!(err.to_string().contains("declared twice"), "{err}");
}

#[test]
fn an_absent_folder_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(omuzaki_embed::module_list(dir.path().join("nope"), "pub").is_err());
}
