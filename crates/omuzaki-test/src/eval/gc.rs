//! `omuzaki-test eval gc` (design 7.3): removes run folders older than 90 days, unless something still points at them.
//!
//! A run folder is kept when **any** of these holds, and is removed only when none does:
//! - its id says it began less than [`MAX_AGE_DAYS`] days ago;
//! - an open issue's body names its run id (a regression issue links the transcripts of its failing attempts);
//! - a script in a task's `evals/` folder names it (`eval promote` writes the run a script came from);
//! - its id does not begin with a time, so its age cannot be told.
//!
//! The issue bodies are read with `gh`. If `gh` fails the plan fails: a folder is never removed on the strength of an
//! issue list that could not be read. Like the golden `gc`, it is a dry run unless asked to apply.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use super::Discovery;
use super::clock::run_started;

/// How long a run folder is kept at least.
pub const MAX_AGE_DAYS: u64 = 90;
const SECS_PER_DAY: u64 = 86_400;

/// Why `gc` could not make a plan.
#[derive(Debug, thiserror::Error)]
pub enum GcError {
    /// A file or folder could not be read or removed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// The open issues could not be read, so nothing can be said to be unreferenced.
    #[error("couldn't read the open issues ({0}); no run folder is removed on a guess")]
    Issues(String),
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> GcError + '_ {
    move |source| GcError::Io { path: path.to_owned(), source }
}

/// Why a run folder stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// Younger than [`MAX_AGE_DAYS`].
    Recent,
    /// An open issue's body names the run.
    Issue,
    /// A script names the run, by the folder it was promoted from.
    Script(PathBuf),
    /// The run id has no time in it, so the folder cannot be called old.
    Undated,
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recent => write!(f, "younger than {MAX_AGE_DAYS} days"),
            Self::Issue => write!(f, "named in an open issue"),
            Self::Script(path) => write!(f, "named by the script {}", path.display()),
            Self::Undated => write!(f, "its id has no time, so its age is unknown"),
        }
    }
}

/// A run folder and what `gc` would do to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The run's id (the folder's name).
    pub run: String,
    /// The folder.
    pub path: PathBuf,
    /// Why it stays, or `None` when it goes.
    pub kept: Option<Why>,
}

/// What `gc` would do to every run folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Every run folder, in id order.
    pub entries: Vec<Entry>,
}

impl Plan {
    /// The folders that go.
    pub fn removals(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.kept.is_none())
    }

    /// Removes the folders that go, and returns their run ids.
    ///
    /// # Errors
    /// [`GcError::Io`] for the first folder that cannot be removed.
    pub fn apply(&self) -> Result<Vec<String>, GcError> {
        let mut removed = Vec::new();
        for entry in self.removals() {
            fs::remove_dir_all(&entry.path).map_err(io_err(&entry.path))?;
            removed.push(entry.run.clone());
        }
        Ok(removed)
    }
}

/// What points at run folders from outside the evidence folder.
#[derive(Debug, Clone, Default)]
pub struct Named {
    /// The text of every open issue body.
    pub issues: Vec<String>,
    /// Every script in the workspace's `evals/` folders, with its text.
    pub scripts: Vec<(PathBuf, String)>,
}

impl Named {
    /// The scripts beside the tasks of `discovery`, read as text: the script itself is not parsed, so a script that no
    /// longer parses still keeps the run it names.
    #[must_use]
    pub fn with_scripts_of(mut self, discovery: &Discovery) -> Self {
        for file in &discovery.tasks {
            for script in &file.scripts {
                let path = file.path.with_file_name(&script.file_name);
                self.scripts.push((path, String::from_utf8_lossy(&script.bytes).into_owned()));
            }
        }
        self
    }
}

/// Plans what to do to the run folders in `<out>/runs`, as of `now`.
///
/// # Errors
/// [`GcError::Io`] when the folder cannot be read.
pub fn plan(out: &Path, now: SystemTime, named: &Named) -> Result<Plan, GcError> {
    let runs = out.join("runs");
    let mut ids = BTreeSet::new();
    match fs::read_dir(&runs) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(io_err(&runs))?;
                if entry.file_type().map_err(io_err(&entry.path()))?.is_dir() {
                    ids.insert(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(&runs)(e)),
    }
    let oldest = now.checked_sub(Duration::from_secs(MAX_AGE_DAYS * SECS_PER_DAY));
    let entries = ids
        .into_iter()
        .map(|run| {
            let kept = why_kept(&run, oldest, named);
            Entry { path: runs.join(&run), run, kept }
        })
        .collect();
    Ok(Plan { entries })
}

fn why_kept(run: &str, oldest: Option<SystemTime>, named: &Named) -> Option<Why> {
    let Some(began) = run_started(run) else { return Some(Why::Undated) };
    // An old clock (no instant 90 days back) keeps everything: nothing can be called old.
    if oldest.is_none_or(|oldest| began > oldest) {
        return Some(Why::Recent);
    }
    if named.issues.iter().any(|body| body.contains(run)) {
        return Some(Why::Issue);
    }
    named.scripts.iter().find(|(_, text)| text.contains(run)).map(|(path, _)| Why::Script(path.clone()))
}

/// The bodies of the open issues, read with `gh api` (REST: it works where GraphQL does not).
///
/// # Errors
/// [`GcError::Issues`] when `gh` is missing, fails or says anything but success.
pub fn open_issue_bodies() -> Result<Vec<String>, GcError> {
    let output = Command::new("gh")
        .args([
            "api",
            "--paginate",
            "repos/{owner}/{repo}/issues?state=open&per_page=100",
            "--jq",
            ".[] | .body // \"\"",
        ])
        .output()
        .map_err(|e| GcError::Issues(format!("`gh` did not start: {e}")))?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        return Err(GcError::Issues(format!("`gh` failed: {}", why.trim())));
    }
    Ok(vec![String::from_utf8_lossy(&output.stdout).into_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::clock::utc_minute;

    fn days_ago(now: SystemTime, days: u64) -> String {
        format!("{}-nightly", utc_minute(now - Duration::from_secs(days * SECS_PER_DAY)))
    }

    fn make_run(out: &Path, run: &str) {
        let dir = out.join("runs").join(run).join("task").join("1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("transcript.jsonl"), "{}\n").unwrap();
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(500_000)
    }

    #[test]
    fn old_runs_go_and_recent_ones_stay() {
        let out = tempfile::tempdir().unwrap();
        let (old, recent) = (days_ago(now(), 120), days_ago(now(), 10));
        make_run(out.path(), &old);
        make_run(out.path(), &recent);
        let plan = plan(out.path(), now(), &Named::default()).unwrap();
        let kept: Vec<(&str, bool)> = plan.entries.iter().map(|e| (e.run.as_str(), e.kept.is_some())).collect();
        assert_eq!(kept, [(old.as_str(), false), (recent.as_str(), true)]);
        assert_eq!(plan.apply().unwrap(), std::slice::from_ref(&old));
        assert!(!out.path().join("runs").join(&old).exists() && out.path().join("runs").join(&recent).exists());
    }

    #[test]
    fn an_old_run_an_issue_or_a_script_names_stays_and_only_the_others_are_removed() {
        let out = tempfile::tempdir().unwrap();
        let (in_issue, in_script, loose) = (days_ago(now(), 100), days_ago(now(), 101), days_ago(now(), 102));
        for run in [&in_issue, &in_script, &loose] {
            make_run(out.path(), run);
        }
        let named = Named {
            issues: vec![format!("Eval regression: see {in_issue}/face.more-oval/3 for the transcript")],
            scripts: vec![(
                PathBuf::from("crates/c/evals/t.x.pass.jsonl"),
                format!("{{\"promoted_from\":\"{in_script}/t/2\"}}"),
            )],
        };
        let plan = plan(out.path(), now(), &named).unwrap();
        let why = |run: &str| plan.entries.iter().find(|e| e.run == run).unwrap().kept.clone();
        assert_eq!(why(&in_script), Some(Why::Script(PathBuf::from("crates/c/evals/t.x.pass.jsonl"))));
        assert_eq!(why(&in_issue), Some(Why::Issue));
        assert_eq!(why(&loose), None);
        assert_eq!(plan.apply().unwrap(), std::slice::from_ref(&loose));
        assert!(out.path().join("runs").join(&in_issue).exists() && out.path().join("runs").join(&in_script).exists());
    }

    #[test]
    fn a_run_whose_age_cannot_be_told_is_never_removed() {
        let out = tempfile::tempdir().unwrap();
        make_run(out.path(), "manual-experiment");
        let plan = plan(out.path(), now(), &Named::default()).unwrap();
        assert_eq!(plan.entries[0].kept, Some(Why::Undated));
        assert_eq!(plan.apply().unwrap().len(), 0);
    }

    #[test]
    fn no_runs_folder_is_an_empty_plan() {
        let out = tempfile::tempdir().unwrap();
        assert_eq!(plan(out.path(), now(), &Named::default()).unwrap().entries.len(), 0);
    }
}
