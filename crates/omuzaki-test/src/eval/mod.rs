//! Eval task files (design `docs/design/evals.md` 3, 4, 11; ADR 0022): their format, discovery, loader and lint.
//!
//! - [`task`]: [`task::Task`], its caps and the typed check vocabulary
//! - [`fixture`]: [`fixture::Fixture`], the starting project as an op script
//! - [`provenance`]: the private list and the provenance rules for fixture and eval-set items
//! - [`check`]: the check library that grades an attempt
//! - [`transcript`]: the record of an attempt, and its tool calls as the `log.*` checks read them
//! - [`host`], [`script`], [`driver`]: the server an attempt talks to, the scripts that talk to it and the drivers that play them
//! - [`lint`]: everything `omuzaki-test eval lint` fails on, as a list of findings
//!
//! Tasks live in `crates/<crate>/evals/**/*.toml` (never in a `fixtures/` folder), fixtures in
//! `crates/<crate>/evals/fixtures/*.toml`, and a task's scripts beside it as `<task-stem>.<name>.{pass,fail}.jsonl`.
//! The two files `defaults.toml` and `private.toml` at the top of an `evals/` folder are configuration, not tasks.
//! Adding a task never edits another file (spec §39 rule 2).

pub mod build;
pub mod check;
pub mod clock;
pub mod driver;
pub mod fixture;
pub mod gc;
pub mod history;
pub mod host;
pub mod lint;
mod placeholder;
pub mod provenance;
pub mod run;
pub mod script;
pub mod task;
pub mod transcript;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use self::fixture::Fixture;
use self::provenance::{EvalSet, EvalSets, PrivateList};
use self::task::{Caps, Defaults, Task};

/// Files at the top of an `evals/` folder that configure the harness instead of describing a task.
const CONFIG_FILES: [&str; 2] = ["defaults.toml", "private.toml"];

/// Harness trouble: something that is not a finding about a task (the CLI exits 2).
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// A file or folder could not be read.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// A required configuration file does not parse.
    #[error("{}: {reason}", path.display())]
    Config {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// The directory given is not a Omuzaki workspace.
    #[error("{}: not the Omuzaki workspace (no crates/ folder)", .0.display())]
    NotWorkspace(PathBuf),
}

/// A file that failed to parse: reported by the lint, and the file is left out of the loaded set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unparsed {
    /// The file, relative to the workspace root.
    pub path: PathBuf,
    /// The parser's message.
    pub reason: String,
    /// Check `kind`s outside the vocabulary, when the file is a task with any (the likeliest mistake gets its own
    /// finding).
    pub unknown_kinds: Vec<String>,
}

/// A loaded task with where it came from.
#[derive(Debug, Clone)]
pub struct TaskFile {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// The crate whose `evals/` folder holds it.
    pub crate_name: String,
    /// The parsed task.
    pub task: Task,
    /// The file's bytes, for the content hash.
    pub bytes: Vec<u8>,
    /// Scripts beside it: file name and bytes, sorted by name.
    pub scripts: Vec<Script>,
}

/// A `<task-stem>.<name>.{pass,fail}.jsonl` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// The file name.
    pub file_name: String,
    /// Whether it must pass (`true`) or must fail (`false`).
    pub expect_pass: bool,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// A loaded fixture.
#[derive(Debug, Clone)]
pub struct FixtureFile {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// The parsed fixture.
    pub fixture: Fixture,
    /// The file's bytes, for the content hash.
    pub bytes: Vec<u8>,
}

/// Everything found under the workspace's `evals/` folders.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Tasks, sorted by path.
    pub tasks: Vec<TaskFile>,
    /// Fixtures, sorted by path.
    pub fixtures: Vec<FixtureFile>,
    /// Eval sets, by id (first one wins; duplicates are lint findings through `set_files`).
    pub sets: EvalSets,
    /// Every set manifest read, with its path, so duplicates and bad items can be reported by file.
    pub set_files: Vec<(PathBuf, EvalSet)>,
    /// Files that did not parse.
    pub unparsed: Vec<Unparsed>,
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> EvalError + '_ {
    move |source| EvalError::Io { path: path.to_owned(), source }
}

/// Sorted entries of a folder; a missing folder is empty.
fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, EvalError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(EvalError::Io { path: dir.to_owned(), source }),
    };
    let mut paths = Vec::new();
    for entry in entries {
        paths.push(entry.map_err(io_err(dir))?.path());
    }
    paths.sort();
    Ok(paths)
}

/// Every `.toml` file under `dir`, recursively, sorted; `fixtures` folders are skipped.
fn task_toml_files(dir: &Path, top: bool, out: &mut Vec<PathBuf>) -> Result<(), EvalError> {
    for path in read_dir_sorted(dir)? {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if path.is_dir() {
            if name != "fixtures" {
                task_toml_files(&path, false, out)?;
            }
        } else if path.extension().is_some_and(|e| e == "toml") && !(top && CONFIG_FILES.contains(&name)) {
            out.push(path);
        }
    }
    Ok(())
}

fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))?;
    toml::from_str(text).map_err(|e| e.to_string())
}

/// The `kind`s of a task file's checks that are not in the vocabulary.
fn unknown_kinds(bytes: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(bytes) else { return Vec::new() };
    let Ok(table) = text.parse::<toml::Table>() else { return Vec::new() };
    let Some(toml::Value::Array(checks)) = table.get("check") else { return Vec::new() };
    checks
        .iter()
        .filter_map(|c| c.get("kind").and_then(toml::Value::as_str))
        .filter(|k| !task::CHECK_KINDS.contains(k))
        .map(str::to_owned)
        .collect()
}

fn crate_dirs(root: &Path) -> Result<Vec<PathBuf>, EvalError> {
    let crates = root.join("crates");
    if !crates.is_dir() {
        return Err(EvalError::NotWorkspace(root.to_owned()));
    }
    Ok(read_dir_sorted(&crates)?.into_iter().filter(|p| p.is_dir()).collect())
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_owned()
}

/// Scripts of the task whose file stem is `stem`, in `dir`.
fn scripts_of(dir: &Path, stem: &str) -> Result<Vec<Script>, EvalError> {
    let mut scripts = Vec::new();
    for path in read_dir_sorted(dir)? {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(rest) = name.strip_prefix(stem).and_then(|r| r.strip_prefix('.')) else { continue };
        let (middle, expect_pass) = if let Some(m) = rest.strip_suffix(".pass.jsonl") {
            (m, true)
        } else if let Some(m) = rest.strip_suffix(".fail.jsonl") {
            (m, false)
        } else {
            continue;
        };
        if !middle.is_empty() {
            let bytes = fs::read(&path).map_err(io_err(&path))?;
            scripts.push(Script { file_name: name.to_owned(), expect_pass, bytes });
        }
    }
    Ok(scripts)
}

/// Finds and parses every task, fixture and eval set under `root/crates`.
///
/// # Errors
/// [`EvalError`] when a folder cannot be read. A file that does not parse is not an error here: it is recorded in
/// [`Discovery::unparsed`] so the lint can name it.
pub fn discover(root: &Path) -> Result<Discovery, EvalError> {
    let mut found = Discovery::default();
    for crate_dir in crate_dirs(root)? {
        let crate_name = crate_dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let evals = crate_dir.join("evals");
        let mut files = Vec::new();
        task_toml_files(&evals, true, &mut files)?;
        for path in files {
            let bytes = fs::read(&path).map_err(io_err(&path))?;
            match parse::<Task>(&bytes) {
                Ok(task) => {
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                    let scripts = scripts_of(path.parent().unwrap_or(&evals), stem)?;
                    let path = relative(root, &path);
                    found.tasks.push(TaskFile { path, crate_name: crate_name.clone(), task, bytes, scripts });
                }
                Err(reason) => {
                    let unknown_kinds = unknown_kinds(&bytes);
                    found.unparsed.push(Unparsed { path: relative(root, &path), reason, unknown_kinds });
                }
            }
        }
        for path in read_dir_sorted(&evals.join("fixtures"))? {
            if path.extension().is_some_and(|e| e == "toml") {
                let bytes = fs::read(&path).map_err(io_err(&path))?;
                match parse::<Fixture>(&bytes) {
                    Ok(fixture) => found.fixtures.push(FixtureFile { path: relative(root, &path), fixture, bytes }),
                    Err(reason) => {
                        found.unparsed.push(Unparsed {
                            path: relative(root, &path),
                            reason,
                            unknown_kinds: Vec::new(),
                        });
                    }
                }
            }
        }
        for path in read_dir_sorted(&crate_dir.join("evalsets"))? {
            if path.extension().is_some_and(|e| e == "toml") {
                let bytes = fs::read(&path).map_err(io_err(&path))?;
                match parse::<EvalSet>(&bytes) {
                    Ok(set) => {
                        found.sets.entry(set.id.clone()).or_insert_with(|| set.clone());
                        found.set_files.push((relative(root, &path), set));
                    }
                    Err(reason) => {
                        found.unparsed.push(Unparsed {
                            path: relative(root, &path),
                            reason,
                            unknown_kinds: Vec::new(),
                        });
                    }
                }
            }
        }
    }
    Ok(found)
}

/// Reads `crates/omuzaki-test/evals/defaults.toml`.
///
/// # Errors
/// [`EvalError`] when it is missing or does not parse: without defaults no task has caps.
pub fn load_defaults(root: &Path) -> Result<Caps, EvalError> {
    let path = root.join("crates/omuzaki-test/evals/defaults.toml");
    let bytes = fs::read(&path).map_err(io_err(&path))?;
    let defaults: Defaults = parse(&bytes).map_err(|reason| EvalError::Config { path, reason })?;
    Ok(defaults.caps)
}

/// Reads `crates/omuzaki-test/evals/private.toml`.
///
/// # Errors
/// [`EvalError`] when it is missing or does not parse: a missing private list must never mean "nothing is private".
pub fn load_private(root: &Path) -> Result<PrivateList, EvalError> {
    let path = root.join("crates/omuzaki-test/evals/private.toml");
    let bytes = fs::read(&path).map_err(io_err(&path))?;
    parse(&bytes).map_err(|reason| EvalError::Config { path, reason })
}

impl Discovery {
    /// The fixture with this id, if any.
    #[must_use]
    pub fn fixture(&self, id: &str) -> Option<&FixtureFile> {
        self.fixtures.iter().find(|f| f.fixture.id == id)
    }

    /// The content hash of a task: BLAKE3 over its file, its scripts and its fixture, as `b3:<hex>`.
    ///
    /// Each part is length-prefixed so moving bytes between parts changes the hash. History keys on this to know when
    /// a task's meaning changed.
    #[must_use]
    pub fn task_hash(&self, task: &TaskFile) -> String {
        let mut hasher = blake3::Hasher::new();
        let mut part = |name: &str, bytes: &[u8]| {
            hasher.update(&(name.len() as u64).to_le_bytes()).update(name.as_bytes());
            hasher.update(&(bytes.len() as u64).to_le_bytes()).update(bytes);
        };
        part("task", &task.bytes);
        for script in &task.scripts {
            part(&script.file_name, &script.bytes);
        }
        if let Some(fixture) = self.fixture(&task.task.fixture) {
            part("fixture", &fixture.bytes);
        }
        format!("b3:{}", hasher.finalize().to_hex())
    }
}
