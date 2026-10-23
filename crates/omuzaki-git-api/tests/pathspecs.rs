//! The pathspecs the contract hands to git, checked against the real `git` where it is installed.

#![allow(clippy::expect_used)] // helpers outside `#[test]` fns

use std::path::{Path, PathBuf};
use std::process::Command;

use omuzaki_git_api::error::GitError;
use omuzaki_git_api::paths::{
    PRIVATE_EXCLUDES, PROJECT_PATHSPEC, commit_pathspecs, is_private, literal_pathspec, project_pathspecs,
    tree_pathspecs,
};

#[test]
fn the_whole_project_selection_excludes_private_files() {
    let specs = commit_pathspecs(&["."]).expect("the project root is allowed");
    assert_eq!(specs[0], ":(literal).");
    assert_eq!(specs[1..], PRIVATE_EXCLUDES);
    let several = commit_pathspecs(&["a.txt", "sub"]).expect("plain paths");
    assert_eq!(several, [":(literal)a.txt", ":(literal)sub", PRIVATE_EXCLUDES[0], PRIVATE_EXCLUDES[1]]);
}

#[test]
fn bad_paths_are_refused_before_any_pathspec_is_built() {
    assert_eq!(
        commit_pathspecs(&[".omuzaki/state.json"]),
        Err(GitError::BadPath { path: ".omuzaki/state.json".into() })
    );
    assert_eq!(commit_pathspecs::<&str>(&[]), Err(GitError::NothingToCommit));
}

#[test]
fn the_project_selection_is_the_folder_without_private_files() {
    let specs = project_pathspecs();
    assert_eq!(specs[0], PROJECT_PATHSPEC);
    assert_eq!(specs[1..], PRIVATE_EXCLUDES);
}

/// A scratch repository that removes itself.
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str) -> Option<Self> {
        Command::new("git").arg("--version").output().ok()?;
        let dir = std::env::temp_dir().join(format!("omuzaki-git-api-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("scratch folder");
        let repo = Self(dir);
        git(&repo.0, &["init", "-q"]);
        Some(repo)
    }

    fn write(&self, rel: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("folder");
        std::fs::write(path, rel).expect("file");
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

impl Drop for Repo {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn args<'a>(head: &[&'a str], specs: &'a [String]) -> Vec<&'a str> {
    head.iter().copied().chain(["--"]).chain(specs.iter().map(String::as_str)).collect()
}

#[test]
fn a_tracked_private_file_is_not_staged_by_any_selection() {
    let Some(repo) = Repo::new("private") else { return };
    for rel in ["a.txt", ".omuzaki/state.json", "sub/b.txt", "sub/.omuzaki/x", "sub/.OMUZAKI/y"] {
        repo.write(rel);
    }
    // Once force-added, the private files are tracked, which is the case a `.gitignore` cannot stop.
    git(&repo.0, &["add", "-f", ".omuzaki/state.json", "sub/.omuzaki/x", "sub/.OMUZAKI/y"]);
    git(&repo.0, &["commit", "-q", "-m", "seed"]);
    for rel in [".omuzaki/state.json", "sub/.omuzaki/x", "sub/.OMUZAKI/y", "a.txt", "sub/b.txt"] {
        repo.write(&format!("{rel}.new"));
        std::fs::write(repo.0.join(rel), "changed").expect("edit");
    }
    for selection in [vec!["."], vec!["sub"], vec!["a.txt", "sub"]] {
        let specs = commit_pathspecs(&selection).expect("allowed");
        git(&repo.0, &args(&["add", "-A"], &specs));
        let staged = git(&repo.0, &["diff", "--cached", "--name-only"]);
        assert!(!staged.to_ascii_lowercase().contains(".omuzaki"), "{selection:?} staged:\n{staged}");
        assert!(staged.contains("a.txt") || selection == ["sub"], "{selection:?}");
        git(&repo.0, &["reset", "-q"]);
    }
    // And it is the excludes that do it: without them the same `.` stages the private file.
    git(&repo.0, &["add", "-A", "--", PROJECT_PATHSPEC]);
    let staged = git(&repo.0, &["diff", "--cached", "--name-only"]);
    assert!(staged.contains(".omuzaki/state.json"), "the test would pass without the excludes:\n{staged}");
}

#[test]
fn the_log_of_a_subfolder_project_holds_only_its_own_commits() {
    let Some(repo) = Repo::new("log") else { return };
    repo.write("proj/a.txt");
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "mine"]);
    repo.write("other/b.txt");
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "elsewhere"]);
    repo.write("proj/.omuzaki/state.json");
    git(&repo.0, &["add", "-f", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "private only"]);
    let project = repo.0.join("proj");
    let all = git(&project, &["log", "--format=%s"]);
    assert_eq!(all.lines().count(), 3, "without a pathspec every project's commit shows");
    let specs = project_pathspecs();
    let scoped = git(&project, &args(&["log", "--format=%s"], &specs));
    assert_eq!(scoped.lines().collect::<Vec<_>>(), ["mine"]);
}

#[test]
fn a_tree_is_listed_without_magic_and_its_private_entries_are_dropped_after() {
    let Some(repo) = Repo::new("tree") else { return };
    for rel in ["proj/a.txt", "proj/sub/.OMUZAKI/x", "proj/.omuzaki/state.json", "proj/sub/b.txt", "other/c.txt"] {
        repo.write(rel);
    }
    git(&repo.0, &["add", "-f", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "seed"]);
    let project = repo.0.join("proj");
    // The exclude magic the other reads use is refused by ls-tree (#1939): exit 128, so the import would fail.
    let refused = Command::new("git")
        .current_dir(&project)
        .args(args(&["ls-tree", "-r", "--name-only", "HEAD"], &project_pathspecs()))
        .output()
        .expect("git runs");
    assert!(!refused.status.success(), "ls-tree took the exclude magic; tree_pathspecs is no longer needed");
    let listed = git(&project, &args(&["ls-tree", "-r", "--name-only", "--full-name", "HEAD"], &tree_pathspecs()));
    let mut kept: Vec<&str> = listed.lines().filter(|path| !is_private(path)).collect();
    kept.sort_unstable();
    assert_eq!(kept, ["proj/a.txt", "proj/sub/b.txt"], "listed:\n{listed}");
}

#[test]
fn private_paths_are_named_at_any_depth_in_any_case() {
    for path in [".omuzaki", ".omuzaki/state.json", "sub/.OMUZAKI/x", "a/b/.Omuzaki"] {
        assert!(is_private(path), "{path}");
    }
    for path in ["a.txt", "omuzaki/x", "sub/.omuzaki-old/x", "x.omuzaki"] {
        assert!(!is_private(path), "{path}");
    }
}

#[cfg(not(windows))]
#[test]
fn a_backslash_is_part_of_a_file_name_off_windows() {
    assert_eq!(literal_pathspec(Path::new("a\\b.txt")), ":(literal)a\\b.txt");
    let Some(repo) = Repo::new("backslash") else { return };
    repo.write("a\\b.txt");
    repo.write("a/b.txt");
    let specs = commit_pathspecs(&["a\\b.txt"]).expect("allowed");
    git(&repo.0, &args(&["add"], &specs));
    let staged = git(&repo.0, &["diff", "--cached", "--name-only", "-z"]);
    assert_eq!(staged, "a\\b.txt\0", "only the file named with a backslash is staged");
}
