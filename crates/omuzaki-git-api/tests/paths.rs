//! The commit path rules: what is refused, what is kept literal.

use std::path::{Path, PathBuf};

use omuzaki_git_api::error::GitError;
use omuzaki_git_api::paths::{literal_pathspec, validate_commit_paths};

fn bad(path: &str) -> Result<(), GitError> {
    Err(GitError::BadPath { path: path.to_owned() })
}

#[test]
fn refuses_paths_outside_the_folder_or_in_private_places() {
    for path in [
        "../x",
        "a/../../x",
        "/etc/passwd",
        ".omuzaki/state.json",
        "sub/.omuzaki/x",
        ".OMUZAKI/x",
        ".git/config",
        "a/.git/hooks/pre-commit",
        "",
    ] {
        assert_eq!(validate_commit_paths(&[PathBuf::from(path)]), bad(path), "{path}");
    }
}

#[test]
fn names_the_first_bad_path_among_good_ones() {
    assert_eq!(validate_commit_paths(&["a.txt", "../x", ".git/y"]), bad("../x"));
}

#[test]
fn accepts_names_that_look_like_pathspec_magic() {
    let paths = [":(top)*", "dir/a b.txt", "ünï.txt", "[x].txt", "./a.txt", ".omuzaki-notes.txt", ".", "./"];
    assert_eq!(validate_commit_paths(&paths), Ok(()));
}

#[test]
fn the_project_root_selects_the_whole_project() {
    assert_eq!(validate_commit_paths(&["."]), Ok(()));
    assert_eq!(literal_pathspec(Path::new(".")), ":(literal).");
}

#[test]
fn an_empty_list_is_nothing_to_commit() {
    assert_eq!(validate_commit_paths::<&str>(&[]), Err(GitError::NothingToCommit));
}

#[test]
fn magic_stays_literal() {
    assert_eq!(literal_pathspec(Path::new(":(top)*")), ":(literal):(top)*");
    assert_eq!(literal_pathspec(Path::new("dir/a.txt")), ":(literal)dir/a.txt");
}
