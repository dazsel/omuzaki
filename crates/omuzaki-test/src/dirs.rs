//! Where the three golden folders live, and how they are found.
//!
//! | Folder | Holds | In git? | Chosen by |
//! |---|---|---|---|
//! | store | `objects/<hash>.png`, the reference images | no, in the assets folder | `OMUZAKI_GOLDENS` / `--goldens` |
//! | manifests | `<suite>.toml`, case -> hash + tolerance | yes, `tests/goldens` | `OMUZAKI_GOLDEN_MANIFESTS` / `--manifests`, else the workspace's `tests/goldens` |
//! | out | evidence for the PR comment | no | `OMUZAKI_GOLDEN_OUT` / `--out`, else `<store>-out/<checkout>` |
//!
//! Explicit arguments beat the environment, which beats the default. An empty value counts as unset, so a stray
//! `OMUZAKI_GOLDENS=` can't make the store (and its `objects/`) appear in the current directory. The environment is
//! passed in as a function so tests never touch the real one.
//!
//! **The default evidence folder is per checkout.** Evidence is "what the latest run found", and a passing check
//! deletes its own stale evidence. With one folder shared by every branch, branch B passing a case would erase branch
//! A's failure of it, and the report would say nothing changed. So the default is `<store>-out/<checkout>`, where
//! `<checkout>` is the name of the workspace folder (the git worktree, which this project names after its branch). CI
//! that wants one folder per PR run sets `OMUZAKI_GOLDEN_OUT` instead.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::GoldenError;
use crate::objects::ObjectStore;

/// Environment variable holding the golden store's root directory (the images, outside git).
pub const GOLDENS_ENV: &str = "OMUZAKI_GOLDENS";
/// Environment variable holding the manifest directory. Defaults to `tests/goldens` in the workspace.
pub const MANIFESTS_ENV: &str = "OMUZAKI_GOLDEN_MANIFESTS";
/// Environment variable holding the evidence directory. Defaults to `<store>-out/<checkout>`, next to the store.
pub const OUT_ENV: &str = "OMUZAKI_GOLDEN_OUT";

/// Folders given explicitly (command-line flags); whatever is `None` falls back to the environment, then a default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dirs {
    /// The store root: images live in `<store>/objects`.
    pub store: Option<PathBuf>,
    /// The manifest folder.
    pub manifests: Option<PathBuf>,
    /// The evidence folder.
    pub out: Option<PathBuf>,
}

/// An empty path is no path: it would silently mean the current directory.
fn non_empty(path: Option<PathBuf>) -> Option<PathBuf> {
    path.filter(|p| !p.as_os_str().is_empty())
}

/// The path in environment variable `key`, if it is set to something.
fn env_path(env: &impl Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    non_empty(env(key).map(PathBuf::from))
}

/// All three folders, decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The store root.
    pub store: PathBuf,
    /// The manifest folder.
    pub manifests: PathBuf,
    /// The evidence folder.
    pub out: PathBuf,
}

/// The folders the PR report needs: it reads evidence and never the store or the manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportDirs {
    /// The evidence folder.
    pub evidence: PathBuf,
    /// Where the golden images are (`<store>/objects`), when the store is known.
    pub objects: Option<PathBuf>,
}

impl Dirs {
    /// Decides all three folders from the real environment and the current directory.
    ///
    /// # Errors
    /// [`GoldenError::NoGoldensDir`] or [`GoldenError::NoManifestsDir`] when a folder can't be determined.
    pub fn locate(self) -> Result<Located, GoldenError> {
        // An unreadable current directory only matters when the manifests are not given explicitly.
        let start = std::env::current_dir().unwrap_or_default();
        self.locate_with(|key| std::env::var_os(key), &start)
    }

    /// [`Dirs::locate`] with the environment and the starting directory passed in.
    ///
    /// # Errors
    /// [`GoldenError::NoGoldensDir`] or [`GoldenError::NoManifestsDir`] when a folder can't be determined.
    pub fn locate_with(self, env: impl Fn(&str) -> Option<OsString>, start: &Path) -> Result<Located, GoldenError> {
        let store = non_empty(self.store).or_else(|| env_path(&env, GOLDENS_ENV)).ok_or(GoldenError::NoGoldensDir)?;
        let workspace = workspace_root(start);
        let manifests = non_empty(self.manifests)
            .or_else(|| env_path(&env, MANIFESTS_ENV))
            .or_else(|| workspace.as_deref().map(|root| root.join("tests").join("goldens")))
            .ok_or(GoldenError::NoManifestsDir)?;
        let out = non_empty(self.out)
            .or_else(|| env_path(&env, OUT_ENV))
            .unwrap_or_else(|| default_out(&store, workspace.as_deref()));
        Ok(Located { store, manifests, out })
    }

    /// Decides only what the PR report needs, from the real environment and the current directory.
    ///
    /// # Errors
    /// [`GoldenError::NoGoldensDir`] when no evidence folder can be determined.
    pub fn locate_report(self) -> Result<ReportDirs, GoldenError> {
        let start = std::env::current_dir().unwrap_or_default();
        self.locate_report_with(|key| std::env::var_os(key), &start)
    }

    /// [`Dirs::locate_report`] with the environment and the starting directory passed in. The evidence folder is the
    /// explicit one, else `OMUZAKI_GOLDEN_OUT`, else the per-checkout default derived from the store, exactly as for
    /// [`Dirs::locate_with`], so the report reads where the checks wrote.
    ///
    /// # Errors
    /// [`GoldenError::NoGoldensDir`] when none of those is available.
    pub fn locate_report_with(
        self,
        env: impl Fn(&str) -> Option<OsString>,
        start: &Path,
    ) -> Result<ReportDirs, GoldenError> {
        let store = non_empty(self.store).or_else(|| env_path(&env, GOLDENS_ENV));
        let evidence = non_empty(self.out)
            .or_else(|| env_path(&env, OUT_ENV))
            .or_else(|| store.as_deref().map(|store| default_out(store, workspace_root(start).as_deref())))
            .ok_or(GoldenError::NoGoldensDir)?;
        Ok(ReportDirs { evidence, objects: store.map(|root| ObjectStore::new(&root).dir().to_path_buf()) })
    }
}

/// `<store>-out/<checkout>`: store `…/goldens` and a workspace folder `192-fix` give `…/goldens-out/192-fix`. Without a
/// workspace (the tool run from some unrelated folder) there is no checkout to name, so it is `…/goldens-out`.
fn default_out(store: &Path, workspace: Option<&Path>) -> PathBuf {
    let mut name = store.file_name().map_or_else(|| OsString::from("goldens"), OsString::from);
    name.push("-out");
    let shared = store.with_file_name(name);
    match workspace.and_then(Path::file_name) {
        Some(checkout) => shared.join(checkout),
        None => shared,
    }
}

/// The nearest folder at or above `start` whose `Cargo.toml` declares a `[workspace]`.
///
/// Render tests run with the crate's folder as the working directory, so this is how they find the repo's
/// `tests/goldens` without every crate repeating a relative path. In a git worktree it finds that worktree's own
/// root, which is what makes manifests per-branch.
pub fn workspace_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|dir| declares_workspace(&dir.join("Cargo.toml"))).map(Path::to_path_buf)
}

fn declares_workspace(manifest: &Path) -> bool {
    fs::read_to_string(manifest)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|table| table.contains_key("workspace"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(store: Option<&str>, manifests: Option<&str>, out: Option<&str>) -> Dirs {
        Dirs { store: store.map(Into::into), manifests: manifests.map(Into::into), out: out.map(Into::into) }
    }

    #[test]
    fn folders_come_from_arguments_then_environment_then_defaults() {
        let env = |key: &str| match key {
            GOLDENS_ENV => Some(OsString::from("/data/goldens")),
            MANIFESTS_ENV => Some(OsString::from("/repo/tests/goldens")),
            OUT_ENV => Some(OsString::from("/data/elsewhere")),
            _ => None,
        };
        let nowhere = Path::new("/nowhere");
        let from_env = Dirs::default().locate_with(env, nowhere).unwrap();
        assert_eq!(
            from_env,
            Located {
                store: "/data/goldens".into(),
                manifests: "/repo/tests/goldens".into(),
                out: "/data/elsewhere".into()
            }
        );

        let explicit = dirs(Some("/a/g"), Some("/a/m"), Some("/a/o")).locate_with(env, nowhere).unwrap();
        assert_eq!((explicit.store, explicit.manifests, explicit.out), ("/a/g".into(), "/a/m".into(), "/a/o".into()));

        let derived_out = dirs(Some("/a/g"), Some("/a/m"), None).locate_with(|_| None, nowhere).unwrap();
        assert_eq!(derived_out.out, Path::new("/a/g-out"), "outside any workspace there is no checkout to name");
    }

    #[test]
    fn empty_values_count_as_unset() {
        let empty = |_: &str| Some(OsString::new());
        let nowhere = Path::new("/nowhere");
        assert!(matches!(Dirs::default().locate_with(empty, nowhere), Err(GoldenError::NoGoldensDir)));
        assert!(matches!(dirs(Some(""), None, None).locate_with(|_| None, nowhere), Err(GoldenError::NoGoldensDir)));
        let no_manifests = dirs(Some("/s"), None, None).locate_with(empty, nowhere);
        assert!(matches!(no_manifests, Err(GoldenError::NoManifestsDir)), "{no_manifests:?}");

        let located = dirs(Some("/s"), Some("/m"), Some("")).locate_with(empty, nowhere).unwrap();
        assert_eq!(located.out, Path::new("/s-out"), "an empty OMUZAKI_GOLDEN_OUT does not mean the current directory");
        let report = dirs(Some("/s"), None, None).locate_report_with(empty, nowhere).unwrap();
        assert_eq!(report.evidence, Path::new("/s-out"));
    }

    #[test]
    fn each_checkout_gets_its_own_evidence_folder_and_the_report_reads_the_same_one() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = |name: &str| {
            let root = tmp.path().join(name);
            fs::create_dir_all(root.join("crates/render")).unwrap();
            fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
            root
        };
        let (a, b) = (checkout("192-branch-a"), checkout("201-branch-b"));
        let store = tmp.path().join("goldens");

        let out_of = |start: &Path| dirs(store.to_str(), None, None).locate_with(|_| None, start).unwrap().out;
        let (out_a, out_b) = (out_of(&a.join("crates/render")), out_of(&b));
        assert_eq!(out_a, tmp.path().join("goldens-out/192-branch-a"));
        assert_eq!(out_b, tmp.path().join("goldens-out/201-branch-b"));

        let report = dirs(store.to_str(), None, None).locate_report_with(|_| None, &a.join("crates/render")).unwrap();
        assert_eq!(report.evidence, out_a, "the report reads where the checks of this checkout wrote");
    }

    #[test]
    fn missing_folders_are_errors_that_say_what_to_set() {
        let nowhere = Path::new("/nowhere");
        let no_store = dirs(None, Some("/m"), None).locate_with(|_| None, nowhere);
        assert!(matches!(no_store, Err(GoldenError::NoGoldensDir)));
        let no_manifests = dirs(Some("/s"), None, None).locate_with(|_| None, nowhere);
        assert!(matches!(no_manifests, Err(GoldenError::NoManifestsDir)));
    }

    #[test]
    fn the_default_manifest_folder_is_tests_goldens_of_the_enclosing_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let member = root.join("crates/render");
        fs::create_dir_all(&member).unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/*\"]\n").unwrap();
        fs::write(member.join("Cargo.toml"), "[package]\nname = \"render\"\n\n[lints]\nworkspace = true\n").unwrap();

        assert_eq!(workspace_root(&member).as_deref(), Some(root.as_path()), "a member is not the workspace root");
        let located = dirs(Some("/s"), None, None).locate_with(|_| None, &member).unwrap();
        assert_eq!(located.manifests, root.join("tests/goldens"));
        assert_eq!(workspace_root(tmp.path()), None, "no Cargo.toml above: no root");
    }

    #[test]
    fn the_report_needs_only_the_evidence_folder() {
        let out_only = |key: &str| (key == OUT_ENV).then(|| OsString::from("/only/out"));
        let nowhere = Path::new("/nowhere");
        let report = Dirs::default().locate_report_with(out_only, nowhere).unwrap();
        assert_eq!((report.evidence.as_path(), report.objects), (Path::new("/only/out"), None));

        let from_store = dirs(Some("/r/g"), None, None).locate_report_with(|_| None, nowhere).unwrap();
        assert_eq!(from_store.evidence, Path::new("/r/g-out"));
        assert_eq!(from_store.objects.as_deref(), Some(Path::new("/r/g/objects")));

        assert!(matches!(Dirs::default().locate_report_with(|_| None, nowhere), Err(GoldenError::NoGoldensDir)));
    }
}
