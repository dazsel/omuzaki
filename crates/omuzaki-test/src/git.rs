//! The little of git that `gc` needs: which commits and worktrees exist, and which golden manifests each one holds.
//!
//! Every function fails on any git error instead of returning less. `gc` deletes whatever the answers don't name, so a
//! partial answer (one unreadable branch, one failed command, a line of output that isn't understood) would delete an
//! image a branch still needs.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::error::{GoldenError, gc_refused, io_err};
use crate::fsio::sorted_entries;
use crate::manifest::suite_of;

fn git_error(what: &str, detail: impl Display) -> GoldenError {
    GoldenError::Git { what: what.to_owned(), detail: detail.to_string() }
}

/// A git checkout, addressed by the root of one of its working trees.
#[derive(Debug)]
pub(crate) struct Repo {
    root: PathBuf,
}

/// One working tree of the repository, as `git worktree list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Worktree {
    /// The folder the working tree is checked out in.
    pub path: PathBuf,
    /// The commit it has checked out; `None` for a bare repository or a branch with no commit yet.
    pub head: Option<String>,
    /// A bare repository has no files to read.
    pub bare: bool,
    /// `git worktree lock`ed: kept on purpose even when its folder is gone, typically because it lives on a drive that
    /// is not plugged in right now.
    pub locked: bool,
}

/// When the repository last fetched, as far as git records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FetchRecord {
    /// No remote is configured, so no remote-tracking branch can be behind anything.
    NoRemotes,
    /// There are remotes but no `FETCH_HEAD` exists anywhere: nothing was fetched since the repository was set up (a
    /// clone does not write it).
    Never {
        /// Where the main checkout's record would be.
        file: PathBuf,
    },
    /// Every `FETCH_HEAD` that exists, with the time it was last written (never empty). A fetch writes the one of the
    /// worktree it ran in.
    At(Vec<(PathBuf, SystemTime)>),
}

/// A manifest file in a commit's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeManifest {
    /// The git object id of the file's content: two commits with the same id have the same manifest.
    pub blob: String,
    /// The file name (`render.toml`).
    pub file: String,
}

impl Repo {
    /// Finds the repository around `dir` (which need not exist yet: its nearest existing parent is used) and returns
    /// it with the path of `dir` inside it, written with `/` as git wants it.
    ///
    /// Every part of that path must be spelled exactly as the file system names it. A case-insensitive file system
    /// (NTFS mounted into WSL) opens `Tests/Goldens` as `tests/goldens`, but git compares names byte for byte, so the
    /// wrong spelling would find the files on disk and none of the committed manifests.
    ///
    /// # Errors
    /// [`GoldenError::Git`] when `dir` is not inside a git working tree, or its path can't be expressed as text,
    /// [`GoldenError::GcRefused`] when a part of it is spelled differently from the file system's own name,
    /// [`GoldenError::Io`] when the folders on the way can't be inspected.
    pub(crate) fn locate(dir: &Path) -> Result<(Self, String), GoldenError> {
        let find = |detail: String| git_error("find the git checkout of the manifest folder", detail);
        let absolute = std::path::absolute(dir).map_err(|e| find(e.to_string()))?;
        let existing = nearest_existing_folder(&absolute)?;
        let root = run(existing, "find the git checkout of the manifest folder", &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(String::from_utf8_lossy(&root).trim_end_matches('\n'));

        // Canonical on both sides, so a symlinked or `..`-laden spelling of the same folder still lands inside the root.
        let canonical = |path: &Path| path.canonicalize().map_err(|e| find(format!("{}: {e}", path.display())));
        let tail = absolute.strip_prefix(existing).map_err(|e| find(e.to_string()))?;
        if tail.components().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(find(format!("{} climbs out of a folder that does not exist", dir.display())));
        }
        let full = canonical(existing)?.join(tail);
        let canonical_root = canonical(&root)?;
        let inside = full
            .strip_prefix(&canonical_root)
            .map_err(|_| find(format!("{} is not inside {}", dir.display(), root.display())))?;
        let parts: Option<Vec<_>> = inside.components().map(|part| part.as_os_str().to_str()).collect();
        let parts = parts.ok_or_else(|| find(format!("{} is not valid text", dir.display())))?;
        check_spelling(&canonical_root, &parts)?;
        Ok((Self { root }, parts.join("/")))
    }

    fn run(&self, what: &str, args: &[&str]) -> Result<Vec<u8>, GoldenError> {
        run(&self.root, what, args)
    }

    /// What every ref points at, each once: local and remote-tracking branches, tags, the stash, and anything else git
    /// keeps a ref for. An annotated tag counts as the commit it names. Refs to a blob are left out (a file is no
    /// folder of manifests).
    ///
    /// # Errors
    /// [`GoldenError::Git`] when git fails or prints something unexpected.
    pub(crate) fn ref_tips(&self) -> Result<Vec<String>, GoldenError> {
        let args = ["for-each-ref", "--format=%(objecttype) %(objectname) %(*objecttype) %(*objectname)"];
        let mut tips = parse_ref_tips(&self.run("list the refs", &args)?)?;
        tips.sort();
        tips.dedup();
        Ok(tips)
    }

    /// Every stash entry, still on the stash stack or not yet dropped from its log, with the index and untracked-file
    /// commits each one is built from. A stash holds uncommitted work, manifests included.
    ///
    /// # Errors
    /// [`GoldenError::Git`] when git fails.
    pub(crate) fn stash_commits(&self) -> Result<Vec<String>, GoldenError> {
        let tip = self.run("look for a stash", &["for-each-ref", "--format=%(objectname) %(parent)", "refs/stash"])?;
        if lines(&tip).is_empty() {
            return Ok(Vec::new());
        }
        let log = self.run("list the stash entries", &["reflog", "show", "--format=%H %P", "refs/stash", "--"])?;
        Ok([lines(&tip), lines(&log)]
            .concat()
            .iter()
            .flat_map(|line| line.split_whitespace())
            .map(str::to_owned)
            .collect())
    }

    /// Checks that every branch of every remote is among the refs, as far as the local clone can tell.
    ///
    /// A shallow clone, or one whose remote fetches only some branches (`git clone --single-branch`, and `--depth`
    /// implies it), never sees the other branches, so their images look unwanted. `gc` cannot ask the remote, so it
    /// refuses instead.
    ///
    /// # Errors
    /// [`GoldenError::GcRefused`] for a shallow clone or a remote that does not fetch all branches,
    /// [`GoldenError::Git`] when git fails.
    pub(crate) fn require_all_branches(&self) -> Result<(), GoldenError> {
        let shallow = self.run("check whether the clone is shallow", &["rev-parse", "--is-shallow-repository"])?;
        match String::from_utf8_lossy(&shallow).trim() {
            "false" => {}
            "true" => {
                return Err(gc_refused(
                    "this is a shallow clone, which does not hold every branch's commits",
                    "run `git fetch --unshallow` and `git fetch --all`, or run gc from a full clone",
                ));
            }
            other => {
                return Err(git_error("check whether the clone is shallow", format!("unexpected answer '{other}'")));
            }
        }
        let config = self.run("read the git configuration", &["config", "-z", "--list"])?;
        match remote_missing_branches(&String::from_utf8_lossy(&config)) {
            None => Ok(()),
            Some((remote, specs)) => Err(gc_refused(
                format!(
                    "remote '{remote}' fetches only {}, so its other branches are invisible here",
                    if specs.is_empty() { "nothing (it has no fetch refspec)".to_owned() } else { specs.join(" and ") }
                ),
                format!("run `git remote set-branches {remote} '*'` and then `git fetch {remote}`"),
            )),
        }
    }

    /// Checks that `commit` does not spell the manifest `folder` in a different letter case than the one `gc` was
    /// given. `ls-tree` compares names exactly, so such a commit would silently list no manifests.
    ///
    /// # Errors
    /// [`GoldenError::GcRefused`] when a folder differs from `folder` only in case,
    /// [`GoldenError::Git`] when git fails.
    pub(crate) fn require_exact_folder(&self, commit: &str, folder: &str) -> Result<(), GoldenError> {
        if folder.is_empty() {
            return Ok(());
        }
        let args = ["ls-tree", "-r", "-d", "--name-only", "-z", commit];
        let folders = self.run(&format!("list the folders of {commit}"), &args)?;
        let wanted = folder.to_lowercase();
        let variant = folders
            .split(|&byte| byte == 0)
            .filter_map(|name| std::str::from_utf8(name).ok())
            .find(|name| *name != folder && name.to_lowercase() == wanted);
        match variant {
            None => Ok(()),
            Some(other) => Err(gc_refused(
                format!("commit {commit:.12} has the manifest folder as '{other}', but gc was given '{folder}'"),
                "pass --manifests with the exact spelling the repository uses (git tells letter cases apart even when \
                 the file system does not)",
            )),
        }
    }

    /// The commit `rev` (a branch, tag, or commit id) names.
    ///
    /// # Errors
    /// [`GoldenError::Git`] when `rev` names nothing.
    pub(crate) fn resolve(&self, rev: &str) -> Result<String, GoldenError> {
        let target = format!("{rev}^{{commit}}");
        let args = ["rev-parse", "--verify", "--end-of-options", &target];
        let out = self.run(&format!("resolve '{rev}' to a commit"), &args)?;
        lines(&out).pop().ok_or_else(|| git_error(&format!("resolve '{rev}' to a commit"), "no such commit"))
    }

    /// Every working tree of the repository, this one included.
    ///
    /// # Errors
    /// [`GoldenError::Git`] when git fails.
    pub(crate) fn worktrees(&self) -> Result<Vec<Worktree>, GoldenError> {
        let out = self.run("list the worktrees", &["worktree", "list", "--porcelain"])?;
        Ok(parse_worktrees(&String::from_utf8_lossy(&out)))
    }

    /// When `git fetch` (or `git pull`) last ran in this repository, from the modification times of the `FETCH_HEAD`
    /// files. Git writes that file **per worktree** (`<common git dir>/FETCH_HEAD` for the main checkout,
    /// `<common git dir>/worktrees/<name>/FETCH_HEAD` for a linked one), but the remote-tracking branches a fetch
    /// updates are shared by all of them, so a fetch in any worktree counts and every record is returned. The record
    /// is rewritten by every fetch, also one that finds nothing new, but also by a fetch of a single branch, so it is a
    /// reminder to fetch and not proof that every branch is here.
    ///
    /// # Errors
    /// [`GoldenError::Git`] when git fails, [`GoldenError::Io`] when a `FETCH_HEAD` exists but can't be inspected or the
    /// folder of the linked worktrees can't be listed.
    pub(crate) fn fetch_record(&self) -> Result<FetchRecord, GoldenError> {
        if lines(&self.run("list the remotes", &["remote"])?).is_empty() {
            return Ok(FetchRecord::NoRemotes);
        }
        let what = "find the common git directory";
        let common = self.run(what, &["rev-parse", "--git-common-dir"])?;
        let common = lines(&common).pop().ok_or_else(|| git_error(what, "git printed nothing"))?;
        // Relative to the folder git ran in; an absolute answer replaces the root in `join`.
        let common = self.root.join(common);
        let main = common.join("FETCH_HEAD");
        let linked = sorted_entries(&common.join("worktrees"))?.into_iter().map(|tree| tree.join("FETCH_HEAD"));

        let mut found = Vec::new();
        for file in std::iter::once(main.clone()).chain(linked) {
            match fs::metadata(&file).and_then(|meta| meta.modified()) {
                Ok(time) => found.push((file, time)),
                // A stray file among the worktree folders has no `FETCH_HEAD` to find, as a folder without one.
                Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {}
                Err(e) => return Err(io_err(&file)(e)),
            }
        }
        Ok(if found.is_empty() { FetchRecord::Never { file: main } } else { FetchRecord::At(found) })
    }

    /// The manifest files directly inside `folder` (as returned by [`Repo::locate`]) in the tree of `commit`. A commit
    /// without that folder has none.
    ///
    /// # Errors
    /// [`GoldenError::Git`] when git fails.
    pub(crate) fn manifests_at(&self, commit: &str, folder: &str) -> Result<Vec<TreeManifest>, GoldenError> {
        let listed = format!("{folder}/");
        let mut args = vec!["ls-tree", "-z", commit];
        if !folder.is_empty() {
            args.push(&listed);
        }
        parse_tree_listing(&self.run(&format!("list the manifests of {commit}"), &args)?)
    }

    /// The text of the file with git object id `blob`.
    ///
    /// # Errors
    /// [`GoldenError::Git`] when git fails or the file isn't UTF-8.
    pub(crate) fn text_of(&self, blob: &str) -> Result<String, GoldenError> {
        let what = format!("read the manifest {blob}");
        String::from_utf8(self.run(&what, &["cat-file", "blob", blob])?).map_err(|e| git_error(&what, e))
    }
}

/// Runs `git -C dir args…` and returns its output, or an error carrying git's message.
fn run(dir: &Path, what: &str, args: &[&str]) -> Result<Vec<u8>, GoldenError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0") // a read-only question must not take locks other git commands wait on
        .env_remove("GIT_DIR") // `-C dir` must decide the repository, even when run from inside a git hook
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|e| git_error(what, format!("could not run git: {e}")))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let message = String::from_utf8_lossy(&output.stderr);
    let message = message.trim();
    Err(git_error(what, if message.is_empty() { output.status.to_string() } else { message.to_owned() }))
}

/// The nearest folder at or above `path`, or `path` itself. Only a clean "no such folder" lets the search move up: a
/// folder that can't be inspected (no permission) might hold the manifests, so that is an error.
fn nearest_existing_folder(path: &Path) -> Result<&Path, GoldenError> {
    for candidate in path.ancestors() {
        match fs::metadata(candidate) {
            Ok(meta) if meta.is_dir() => return Ok(candidate),
            Ok(_) => {
                let not_a_folder = io::Error::new(io::ErrorKind::NotADirectory, "not a folder");
                return Err(io_err(candidate)(not_a_folder));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(candidate)(e)),
        }
    }
    Ok(path)
}

/// Checks that each of `parts` (the folder, below `root`) is named exactly so in its parent's listing. The first part
/// that no folder is called, in any letter case, is a folder that does not exist; the search ends there.
fn check_spelling(root: &Path, parts: &[&str]) -> Result<(), GoldenError> {
    let mut folder = root.to_owned();
    for part in parts {
        let names = match fs::read_dir(&folder) {
            Ok(entries) => entries
                .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(io_err(&folder))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_err(&folder)(e)),
        };
        if names.iter().any(|name| name == part) {
            folder.push(part);
            continue;
        }
        return match names.iter().find(|name| name.to_lowercase() == part.to_lowercase()) {
            None => Ok(()),
            Some(real) => Err(gc_refused(
                format!(
                    "the manifest folder is spelled '{part}' but the file system calls it '{real}' (a case-insensitive \
                     file system such as an NTFS mount opens both, git only knows the real one, so the committed manifests \
                     would not be found)"
                ),
                format!("pass --manifests with the exact spelling, '{real}' in place of '{part}'"),
            )),
        };
    }
    Ok(())
}

/// The non-empty lines of git's output.
fn lines(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output).lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect()
}

/// Reads `git worktree list --porcelain`: blocks of `worktree <path>`, `HEAD <commit>`, `bare` / `branch …` /
/// `detached` / `locked …` lines, separated by blank lines.
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut trees: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            trees.push(Worktree { path: PathBuf::from(path), head: None, bare: false, locked: false });
        } else if let Some(tree) = trees.last_mut() {
            if let Some(commit) = line.strip_prefix("HEAD ") {
                // An all-zero id is git's way of saying "no commit yet".
                tree.head = Some(commit.to_owned()).filter(|id| id.bytes().any(|b| b != b'0'));
            } else if line == "bare" {
                tree.bare = true;
            } else if line == "locked" || line.starts_with("locked ") {
                tree.locked = true;
            }
        }
    }
    trees
}

/// Reads `git ls-tree -z` output (`<mode> <type> <id>\t<path>` records ending in NUL), keeping the files whose name is
/// a manifest's. A record that does not have that shape is an error, never skipped: it might have been a manifest.
fn parse_tree_listing(output: &[u8]) -> Result<Vec<TreeManifest>, GoldenError> {
    let mut found = Vec::new();
    for record in output.split(|&byte| byte == 0).filter(|record| !record.is_empty()) {
        let Ok(record) = std::str::from_utf8(record) else { continue }; // a name that isn't text is no manifest's
        let unexpected = || git_error("list the manifests", format!("unexpected record '{record}'"));
        let (meta, path) = record.split_once('\t').ok_or_else(unexpected)?;
        let mut fields = meta.split(' ');
        let (Some(_mode), Some(kind), Some(blob)) = (fields.next(), fields.next(), fields.next()) else {
            return Err(unexpected());
        };
        let file = path.rsplit('/').next().unwrap_or(path);
        if kind == "blob" && suite_of(file).is_some() {
            found.push(TreeManifest { blob: blob.to_owned(), file: file.to_owned() });
        }
    }
    Ok(found)
}

/// Reads `git for-each-ref --format='%(objecttype) %(objectname) %(*objecttype) %(*objectname)'`: what to list the
/// tree of for each ref. A tag of a commit gives the commit, so tags and branches on the same commit are read once.
/// Anything git can look through itself (a tag of a tag, a ref to a tree) is passed on as it is: if it can't, git fails
/// and so does `gc`.
fn parse_ref_tips(output: &[u8]) -> Result<Vec<String>, GoldenError> {
    let mut tips = Vec::new();
    for line in lines(output) {
        let mut fields = line.split_whitespace();
        let (Some(kind), Some(id)) = (fields.next(), fields.next()) else {
            return Err(git_error("list the refs", format!("unexpected line '{line}'")));
        };
        match (kind, fields.next(), fields.next()) {
            ("blob", ..) | ("tag", Some("blob"), _) => {}
            ("tag", Some("commit"), Some(commit)) => tips.push(commit.to_owned()),
            _ => tips.push(id.to_owned()),
        }
    }
    Ok(tips)
}

/// Finds a remote in `git config -z --list` output whose fetch refspecs do not cover every branch, with those
/// refspecs. A remote needs `refs/heads/*` (or `refs/*`) as the source of one refspec and no negative (`^`) refspec.
fn remote_missing_branches(config: &str) -> Option<(String, Vec<String>)> {
    let mut remotes: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for entry in config.split('\0') {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        let Some((name, variable)) = key.strip_prefix("remote.").and_then(|rest| rest.rsplit_once('.')) else {
            continue;
        };
        match variable {
            "url" => {
                remotes.entry(name).or_default();
            }
            "fetch" => remotes.entry(name).or_default().push(value),
            _ => {}
        }
    }
    let covers_all = |specs: &[&str]| {
        let source = |spec: &&str| spec.trim_start_matches('+').split(':').next().unwrap_or("").to_owned();
        let all = specs.iter().map(source).any(|source| source == "refs/heads/*" || source == "refs/*");
        all && !specs.iter().any(|spec| spec.starts_with('^'))
    };
    remotes
        .into_iter()
        .find(|(_, specs)| !covers_all(specs))
        .map(|(name, specs)| (name.to_owned(), specs.into_iter().map(str::to_owned).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktrees_are_read_with_their_commit_bare_and_locked_flags() {
        let text = "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\n\
                    worktree /wt/detached\nHEAD 2222222222222222222222222222222222222222\ndetached\n\n\
                    worktree /wt/unborn\nHEAD 0000000000000000000000000000000000000000\nbranch refs/heads/new\n\n\
                    worktree /mnt/usb/wt\nHEAD 3333333333333333333333333333333333333333\ndetached\nlocked on a usb stick\n\n\
                    worktree /wt/plain-lock\nHEAD 4444444444444444444444444444444444444444\ndetached\nlocked\n\n\
                    worktree /bare.git\nbare\n";
        let trees = parse_worktrees(text);
        let seen: Vec<_> =
            trees.iter().map(|t| (t.path.to_str().unwrap(), t.head.is_some(), t.bare, t.locked)).collect();
        assert_eq!(
            seen,
            [
                ("/repo", true, false, false),
                ("/wt/detached", true, false, false),
                ("/wt/unborn", false, false, false),
                ("/mnt/usb/wt", true, false, true),
                ("/wt/plain-lock", true, false, true),
                ("/bare.git", false, true, false)
            ]
        );
        assert_eq!(trees[1].head.as_deref(), Some("2222222222222222222222222222222222222222"));
    }

    #[test]
    fn a_tree_listing_keeps_only_manifest_files() {
        let listing = b"100644 blob aaaa\ttests/goldens/render.toml\0\
                        100644 blob bbbb\ttests/goldens/render.toml.lock\0\
                        040000 tree cccc\ttests/goldens/nested.toml\0\
                        100644 blob dddd\ttests/goldens/Notes.toml\0\
                        100644 blob eeee\ttests/goldens/face-2.toml\0\
                        100644 blob ffff\ttests/goldens/\xff\xfe.toml\0";
        let found = parse_tree_listing(listing).unwrap();
        let names: Vec<_> = found.iter().map(|m| (m.file.as_str(), m.blob.as_str())).collect();
        assert_eq!(names, [("render.toml", "aaaa"), ("face-2.toml", "eeee")]);
        assert_eq!(parse_tree_listing(b"").unwrap(), Vec::new());
    }

    #[test]
    fn a_tree_listing_record_that_is_not_understood_is_an_error_not_a_skipped_manifest() {
        for bad in [&b"100644 blob aaaa render.toml\0"[..], b"100644 blob\trender.toml\0", b"garbage\0"] {
            let err = parse_tree_listing(bad).unwrap_err();
            assert!(matches!(err, GoldenError::Git { .. }), "{err}");
        }
    }

    #[test]
    fn refs_give_the_commit_to_list_and_blobs_give_nothing() {
        let c = "1".repeat(40);
        let (tag, peeled, tree, blob_tag, nested) =
            ("2".repeat(40), "3".repeat(40), "4".repeat(40), "5".repeat(40), "6".repeat(40));
        let out = format!(
            "commit {c}  \n\
             tag {tag} commit {peeled}\n\
             tree {tree}  \n\
             tag {blob_tag} blob {}\n\
             blob {}  \n\
             tag {nested} tag {}\n",
            "7".repeat(40),
            "8".repeat(40),
            "9".repeat(40)
        );
        assert_eq!(parse_ref_tips(out.as_bytes()).unwrap(), [c, peeled, tree, nested]);
        assert!(matches!(parse_ref_tips(b"commit\n"), Err(GoldenError::Git { .. })));
    }

    #[test]
    fn a_remote_must_fetch_every_branch_to_be_trusted() {
        let config = |entries: &[&str]| entries.iter().flat_map(|e| [*e, "\0"]).collect::<String>();
        let full = "remote.origin.fetch\n+refs/heads/*:refs/remotes/origin/*";
        let url = "remote.origin.url\nhttps://example.com/r.git";
        assert_eq!(remote_missing_branches(&config(&["core.bare\nfalse", url, full])), None);
        assert_eq!(remote_missing_branches(&config(&["core.bare\nfalse"])), None, "no remote, nothing to miss");
        assert_eq!(remote_missing_branches(&config(&[url, "remote.origin.fetch\n+refs/*:refs/*"])), None);

        let single = "remote.origin.fetch\n+refs/heads/main:refs/remotes/origin/main";
        let found = remote_missing_branches(&config(&[url, single])).unwrap();
        assert_eq!(found, ("origin".to_owned(), vec!["+refs/heads/main:refs/remotes/origin/main".to_owned()]));

        let none = remote_missing_branches(&config(&[url])).unwrap();
        assert_eq!(none, ("origin".to_owned(), Vec::new()), "a remote without a fetch refspec fetches nothing");

        let negative = "remote.origin.fetch\n^refs/heads/hidden";
        assert!(
            remote_missing_branches(&config(&[url, full, negative])).is_some(),
            "a negative refspec hides branches"
        );

        let dotted =
            config(&["remote.my.fork.url\nu", "remote.my.fork.fetch\n+refs/heads/dev:refs/remotes/my.fork/dev"]);
        assert_eq!(remote_missing_branches(&dotted).unwrap().0, "my.fork", "remote names may contain dots");
        let other = config(&[url, full, "remote.pushdefault\norigin", "remote.fork.url\nu"]);
        assert_eq!(remote_missing_branches(&other).unwrap().0, "fork", "the second remote counts too");
    }

    #[test]
    fn a_folder_part_in_another_letter_case_is_refused_with_the_real_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("tests/goldens")).unwrap();
        assert!(check_spelling(tmp.path(), &["tests", "goldens"]).is_ok());
        assert!(
            check_spelling(tmp.path(), &["tests", "goldens", "deeper", "still"]).is_ok(),
            "missing folders are a typo for other checks"
        );
        assert!(check_spelling(tmp.path(), &["nothing", "like-it"]).is_ok());
        assert!(check_spelling(tmp.path(), &[]).is_ok());

        let err = check_spelling(tmp.path(), &["Tests", "goldens"]).unwrap_err().to_string();
        assert!(err.contains("'Tests'") && err.contains("'tests'") && err.contains("exact spelling"), "{err}");
        let err = check_spelling(tmp.path(), &["tests", "GOLDENS"]).unwrap_err().to_string();
        assert!(err.contains("'GOLDENS'") && err.contains("'goldens'"), "{err}");
    }

    #[test]
    fn only_a_clean_no_such_folder_lets_the_search_for_the_folder_move_up() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a-file");
        fs::write(&file, b"x").unwrap();
        assert_eq!(nearest_existing_folder(&tmp.path().join("missing/deeper")).unwrap(), tmp.path());
        assert_eq!(nearest_existing_folder(tmp.path()).unwrap(), tmp.path());
        assert!(matches!(nearest_existing_folder(&file), Err(GoldenError::Io { .. })), "a file is not a folder");
        assert!(matches!(nearest_existing_folder(&file.join("below")), Err(GoldenError::Io { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_inspected_is_an_error_and_not_a_missing_folder() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let enforced = fs::read_dir(&locked).is_err(); // root is not kept out
        let result = nearest_existing_folder(&locked.join("tests/goldens")).map(Path::to_path_buf);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if enforced {
            assert!(matches!(result, Err(GoldenError::Io { .. })), "{result:?}");
        }
    }

    /// A repository in `dir` with one commit, isolated from the developer's git configuration.
    fn repository(dir: &Path) {
        for args in [&["init", "-q", "-b", "main"][..], &["commit", "-q", "--allow-empty", "-m", "one"]] {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
    }

    #[test]
    fn a_git_command_that_fails_is_an_error_carrying_gits_message() {
        let tmp = tempfile::tempdir().unwrap();
        repository(tmp.path());
        assert!(run(tmp.path(), "read the log", &["log", "-1", "--format=%s"]).is_ok());

        let err = run(tmp.path(), "resolve nothing", &["rev-parse", "--verify", "--end-of-options", "no-such-ref"])
            .unwrap_err();
        assert!(
            matches!(&err, GoldenError::Git { what, detail } if what == "resolve nothing" && !detail.is_empty()),
            "{err}"
        );
        assert!(matches!(run(&tmp.path().join("missing"), "x", &["status"]), Err(GoldenError::Git { .. })));

        let (repo, _) = Repo::locate(tmp.path()).unwrap();
        assert!(matches!(repo.resolve("no-such-branch"), Err(GoldenError::Git { .. })));
        assert!(matches!(repo.text_of(&"0".repeat(40)), Err(GoldenError::Git { .. })));
        assert!(matches!(repo.manifests_at(&"0".repeat(40), "tests/goldens"), Err(GoldenError::Git { .. })));
    }

    #[test]
    fn a_folder_outside_any_repository_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(Repo::locate(&tmp.path().join("tests/goldens")), Err(GoldenError::Git { .. })));
    }

    #[test]
    fn the_folder_is_found_relative_to_the_repository_even_when_it_does_not_exist_yet() {
        let tmp = tempfile::tempdir().unwrap();
        repository(tmp.path());
        let (_, folder) = Repo::locate(&tmp.path().join("tests/goldens")).unwrap();
        assert_eq!(folder, "tests/goldens");
        let (_, root) = Repo::locate(tmp.path()).unwrap();
        assert_eq!(root, "");
        // Unix keeps the `..` and cannot climb out of a folder that is not there. Windows has already folded
        // `missing/..` away by name (`std::path::absolute`), so the path is simply `<repo>/tests`.
        let through_a_missing_folder = Repo::locate(&tmp.path().join("missing/../tests"));
        if cfg!(windows) {
            assert_eq!(through_a_missing_folder.unwrap().1, "tests");
        } else {
            let err = through_a_missing_folder.unwrap_err();
            assert!(matches!(err, GoldenError::Git { .. }), "{err}");
        }
    }
}
