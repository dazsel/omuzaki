//! The runner (design 6.2, 6.4, 7): plays tasks, grades them, keeps the evidence and appends the history.
//!
//! One run builds each fixture once, copies it for every attempt into a scratch folder, plays the attempt on a host,
//! grades the result with the check library on **separate** runners (the pristine fixture and the result folder), keeps
//! the transcript and the check report, and deletes the scratch. With `record` it then appends one line per attempt to
//! `eval-history.jsonl` and one for the run to `eval-runs.jsonl`.
//!
//! A scripted run records `attempts` attempts of each task's pass script, and also plays, once, every other pass script,
//! every fail script and the empty script (the oracles of design 5.4). The oracles are never in the history, because
//! a script that must fail would lower the pass rate; they are in the run record, and any that does not end as it must
//! fails the run. The reference driver is a separate task and plugs in through [`Driver`](super::driver::Driver).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime};

use omuzaki_ops_api::Grants;

use super::build::{Built, build, copy_dir};
use super::check::{CheckReport, Checker, Outcome, glob_match};
use super::driver::scripted::Scripted;
use super::driver::{Driver, Stop};
use super::fixture::SetItemRef;
use super::history::{
    self, AttemptOutcome, AttemptRecord, CheckRecord, Counts, FAMILY_TASK, HISTORY_FILE, HistoryError, Oracles,
    RUNS_FILE, RunRecord, SCHEMA, Skipped, Spend, Usage,
};
use super::host::{Host, open_project_runner};
use super::provenance::{PrivateList, fixture_refusals};
use super::script::Script;
use super::task::{Caps, CheckKind};
use super::{Discovery, TaskFile};

/// The harness version recorded with every attempt.
pub const HARNESS: &str = env!("CARGO_PKG_VERSION");
/// The kinds of run (design 7.2).
pub const KINDS: [&str; 4] = ["pr", "nightly", "weekly", "manual"];

/// Which tasks a run plays: those any selector picks. No selector at all means every task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Task id globs (`*` stands for any run of characters).
    pub tasks: Vec<String>,
    /// Tags: a task with any of them is picked (`starter`).
    pub tags: Vec<String>,
    /// Crates: every task whose `evals/` folder is in one of them is picked (the affected crates of a pull request).
    pub crates: Vec<String>,
}

impl Selection {
    fn picks(&self, file: &TaskFile) -> bool {
        if self.tasks.is_empty() && self.tags.is_empty() && self.crates.is_empty() {
            return true;
        }
        self.tasks.iter().any(|glob| glob_match(glob, &file.task.id))
            || self.tags.iter().any(|tag| file.task.tags.contains(tag))
            || self.crates.contains(&file.crate_name)
    }
}

/// Turns a reference to an eval-set item into the path of its verified file.
pub type ItemPath<'a> = &'a (dyn Fn(&SetItemRef) -> Result<String, String> + Sync);

/// Everything a run is told.
pub struct Options<'a> {
    /// The tasks to play.
    pub select: Selection,
    /// Recorded attempts of each task (scripted: of its pass script).
    pub attempts: u32,
    /// Attempts played at once.
    pub jobs: usize,
    /// The `omuzaki` binary to play through (`--binary`), instead of the in-process host.
    pub binary: Option<PathBuf>,
    /// `pr`, `nightly`, `weekly` or `manual`.
    pub kind: String,
    /// The run's id; it begins with the minute the run began (see [`super::clock`]).
    pub run_id: String,
    /// The commit the run is made at.
    pub commit: String,
    /// Where run folders go: `<out>/runs/<run>/<task>/<attempt>/`.
    pub out: PathBuf,
    /// The status folder to append the history to, when recording.
    pub status: Option<PathBuf>,
    /// Where attempts are copied to and played: deleted when the run ends.
    pub scratch: PathBuf,
    /// How a fixture's reference to an eval-set item becomes a file path.
    pub item_path: ItemPath<'a>,
    /// The asset store the render ops write to, for the `render.*` checks. `None` until the stage's render ops exist.
    pub store: Option<std::sync::Arc<dyn omuzaki_store_api::Store>>,
}

/// Why a run could not be made at all: harness trouble, not a result.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A file or folder could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// The history could not be written.
    #[error(transparent)]
    History(#[from] HistoryError),
    /// The options are not usable.
    #[error("{0}")]
    Options(String),
    /// The evidence of an attempt could not be made: a transcript or a report that will not serialise, or a render that the
    /// store does not hold. The run stops rather than keep an attempt whose evidence disagrees with its record.
    #[error("evidence of {task}/{label}: {reason}")]
    Evidence {
        /// The task.
        task: String,
        /// The attempt's folder name.
        label: String,
        /// What went wrong.
        reason: String,
    },
    /// Another run has the run id: its folder exists or its record is in the history. Nothing was written.
    #[error("run `{0}` already has a folder or a record; give --run-id another name")]
    Taken(String),
}

/// An attempt that did not end as it must.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deviation {
    /// The task.
    pub task: String,
    /// The script that was played.
    pub script: String,
    /// The attempt's folder name: its number, or `oracle-<script>`.
    pub label: String,
    /// What it had to be.
    pub expected: AttemptOutcome,
    /// What it was.
    pub got: AttemptOutcome,
    /// Why.
    pub reason: String,
}

/// What a run did.
#[derive(Debug, Clone, PartialEq)]
pub struct RunReport {
    /// The run's record.
    pub run: RunRecord,
    /// The recorded attempts, by task then attempt number.
    pub attempts: Vec<AttemptRecord>,
    /// Attempts, recorded or oracle, that did not end as they must.
    pub deviations: Vec<Deviation>,
    /// Tasks refused before any attempt: a fixture that is not allowed or does not build.
    pub refused: Vec<Skipped>,
}

impl RunReport {
    /// Whether everything ended as it must: no deviation and no refused task.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.deviations.is_empty() && self.refused.is_empty()
    }
}

/// What a job is for.
enum Label {
    /// Recorded attempt number `n`.
    Attempt(u32),
    /// An oracle, named after its script.
    Oracle(String),
}

impl Label {
    fn folder(&self) -> String {
        match self {
            Self::Attempt(n) => n.to_string(),
            Self::Oracle(name) => format!("oracle-{name}"),
        }
    }
}

/// One attempt to play.
struct Job<'a> {
    file: &'a TaskFile,
    label: Label,
    /// The script's file name, or `<null>`.
    script_name: String,
    script: Result<Script, String>,
    expected: AttemptOutcome,
}

/// One attempt, played.
struct Done {
    record: AttemptRecord,
    recorded: bool,
    label: String,
    transcript: Result<String, String>,
    checks: Result<String, String>,
    /// `(file name, bytes)` of the renders the checks made, fetched from the store while it was at hand.
    pictures: Result<Vec<(String, Vec<u8>)>, String>,
    deviation: Option<Deviation>,
}

/// A fixture, built once for the run.
type BuiltFixture = Result<(PathBuf, Built), String>;

/// Plays the selected tasks with the scripted driver.
///
/// # Errors
/// [`RunError`] when the harness itself fails: a folder cannot be written, the history cannot be appended, the options
/// are unusable. A task that fails, or a script that does not end as it must, is in the [`RunReport`], not an error.
pub fn run_scripted(
    discovery: &Discovery,
    defaults: Caps,
    private: &PrivateList,
    options: &Options<'_>,
) -> Result<RunReport, RunError> {
    if options.attempts == 0 {
        return Err(RunError::Options("--attempts must be at least 1".to_owned()));
    }
    if !KINDS.contains(&options.kind.as_str()) {
        return Err(RunError::Options(format!("run kind `{}` is not one of {}", options.kind, KINDS.join(", "))));
    }
    claim_run_id(options)?;
    let result = run_claimed(discovery, defaults, private, options);
    if result.is_err() {
        release_run_id(options);
    }
    result
}

/// Lets go of a run id whose run failed: the evidence folder goes if it is still empty (a run that wrote something keeps
/// its evidence and its id), and so does the scratch.
fn release_run_id(options: &Options<'_>) {
    release_run_id_with(options, || ());
}

/// [`release_run_id`], running `between` after the first of its two steps: the moment a concurrent retry could find out
/// whether the id is free.
fn release_run_id_with(options: &Options<'_>, between: impl FnOnce()) {
    // The scratch goes first and the evidence folder, which is the claim on the id, last: a retry that claims the id the
    // moment the claim is gone must find its own new scratch untouched. (`remove_dir` only removes an empty folder.)
    drop(fs::remove_dir_all(options.scratch.join(&options.run_id)));
    between();
    drop(fs::remove_dir(options.out.join("runs").join(&options.run_id)));
}

fn run_claimed(
    discovery: &Discovery,
    defaults: Caps,
    private: &PrivateList,
    options: &Options<'_>,
) -> Result<RunReport, RunError> {
    let started = Instant::now();
    let mut selected: Vec<&TaskFile> = discovery.tasks.iter().filter(|f| options.select.picks(f)).collect();
    selected.sort_by(|a, b| a.task.id.cmp(&b.task.id));

    let mut refused = Vec::new();
    let mut skipped = Vec::new();
    let mut runnable = Vec::new();
    for file in selected {
        match refusal(discovery, private, file, options) {
            Some(Refusal::Refused(why)) => refused.push(Skipped { task: file.task.id.clone(), why }),
            Some(Refusal::Skipped(why)) => skipped.push(Skipped { task: file.task.id.clone(), why }),
            None => runnable.push(file),
        }
    }

    let scratch = options.scratch.join(&options.run_id);
    let (done, tasks_run) = play(discovery, defaults, &scratch, options, runnable, &mut refused);
    // The scratch is deleted whether the run worked or not; a leftover would only be the previous run's copies.
    let cleanup = match fs::remove_dir_all(&scratch) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(RunError::Io { path: scratch, source: e }),
        _ => Ok(()),
    };

    let mut counts = Counts::default();
    let mut oracles = Oracles::default();
    let mut attempts = Vec::new();
    let mut deviations = Vec::new();
    for item in done {
        write_evidence(&options.out, &options.run_id, &item)?;
        if item.recorded {
            counts.add(item.record.outcome);
            attempts.push(item.record);
        } else {
            oracles.played += 1;
        }
        if let Some(deviation) = item.deviation {
            if !item.recorded {
                oracles.deviations += 1;
            }
            deviations.push(deviation);
        }
    }
    let mut tasks_skipped = skipped;
    tasks_skipped.extend(refused.iter().cloned());
    let run = RunRecord {
        v: SCHEMA,
        run: options.run_id.clone(),
        kind: options.kind.clone(),
        driver: "scripted".to_owned(),
        model: None,
        commit: options.commit.clone(),
        tasks_run,
        tasks_skipped,
        attempts: counts,
        oracles,
        spend: Spend::default(),
        wall_ms: millis(started),
        valid: history::valid(&counts),
    };
    if let Some(status) = &options.status {
        for record in &attempts {
            history::append(&status.join(HISTORY_FILE), record)?;
        }
        history::append(&status.join(RUNS_FILE), &run)?;
    }
    // Reported only now, so a scratch that could not be removed does not throw a finished run away.
    cleanup?;
    Ok(RunReport { run, attempts, deviations, refused })
}

/// The characters of a run id: it becomes a folder name under the evidence and scratch folders, on every platform.
const RUN_ID_CHARS: &str = "letters, digits and `._-`";

/// Refuses a run id that is not a plain folder name, then claims it: the evidence and scratch folders are created
/// (`create_dir` fails when the folder exists, so of two runs that start together only one gets the id) and a record in
/// the history counts as taken. Two runs that shared an id would share scratch and overwrite each other's evidence.
fn claim_run_id(options: &Options<'_>) -> Result<(), RunError> {
    let id = &options.run_id;
    let plain = !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !plain {
        return Err(RunError::Options(format!(
            "run id `{id}` is not a folder name: use {RUN_ID_CHARS}, not starting with a dot"
        )));
    }
    if let Some(status) = &options.status
        && history::read_runs(&status.join(RUNS_FILE))?.iter().any(|r| &r.run == id)
    {
        return Err(RunError::Taken(id.clone()));
    }
    let evidence = options.out.join("runs").join(id);
    let scratch = options.scratch.join(id);
    let claim = |dir: &Path| -> Result<(), RunError> {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent).map_err(|source| RunError::Io { path: parent.to_owned(), source })?;
        }
        match fs::create_dir(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(RunError::Taken(id.clone())),
            Err(source) => Err(RunError::Io { path: dir.to_owned(), source }),
        }
    };
    claim(&evidence)?;
    if let Err(e) = claim(&scratch) {
        // The evidence folder is the one just made, and empty.
        let _ = fs::remove_dir(&evidence);
        return Err(e);
    }
    Ok(())
}

/// Whether a task id can name a folder: lowercase letters, digits, dots and dashes, not starting with a dot.
fn task_id_is_a_folder_name(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'))
}

/// Whether an id can be part of a path under the run's scratch or evidence: no path separator, no `..`, no leading dot.
/// Fixture ids and check ids (a render check's pictures are named after it) must be.
pub(crate) fn is_plain_name(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Why a task is not played.
enum Refusal {
    /// A fault in the task or its fixture: the run fails.
    Refused(String),
    /// A limit of this run, such as the binary host's grants: the task is reported and the run goes on.
    Skipped(String),
}

fn refusal(discovery: &Discovery, private: &PrivateList, file: &TaskFile, options: &Options<'_>) -> Option<Refusal> {
    if !task_id_is_a_folder_name(&file.task.id) {
        return Some(Refusal::Refused(format!(
            "task id `{}` cannot name a folder (lowercase letters, digits, dots, dashes)",
            file.task.id
        )));
    }
    if !is_plain_name(&file.task.fixture) {
        return Some(Refusal::Refused(format!(
            "fixture id `{}` cannot name a folder (letters, digits, dots, dashes, underscores)",
            file.task.fixture
        )));
    }
    if let Some(check) = file.task.checks.iter().find(|c| !is_plain_name(&c.id)) {
        return Some(Refusal::Refused(format!(
            "check id `{}` cannot name a file (letters, digits, dots, dashes, underscores)",
            check.id
        )));
    }
    if options.store.is_none()
        && file
            .task
            .checks
            .iter()
            .any(|c| matches!(c.kind, CheckKind::RenderDiff { .. } | CheckKind::RenderSame { .. }))
    {
        return Some(Refusal::Refused(
            "it has a `render.*` check, and `eval run` has no asset store to fetch renders from until the stage render \
             ops exist (#336)"
                .to_owned(),
        ));
    }
    let Some(fixture) = discovery.fixture(&file.task.fixture) else {
        return Some(Refusal::Refused(format!("unknown fixture `{}`", file.task.fixture)));
    };
    let problems = fixture_refusals(&fixture.fixture, &discovery.sets, private);
    if !problems.is_empty() {
        return Some(Refusal::Refused(format!("its fixture is refused: {}", problems.join("; "))));
    }
    if !file.scripts.iter().any(|s| s.expect_pass) {
        return Some(Refusal::Refused("no pass script to play".to_owned()));
    }
    if options.binary.is_some() && Grants::of(file.task.scopes.iter().copied()) != Grants::agent_default() {
        return Some(Refusal::Skipped(
            "its scopes are not the agent default, which is all `omuzaki mcp` grants".to_owned(),
        ));
    }
    None
}

/// Builds the fixtures, plays every job and returns what happened, in a fixed order, with the tasks that ran.
fn play<'a>(
    discovery: &'a Discovery,
    defaults: Caps,
    scratch: &Path,
    options: &Options<'_>,
    runnable: Vec<&'a TaskFile>,
    refused: &mut Vec<Skipped>,
) -> (Vec<Done>, Vec<String>) {
    let mut fixtures: BTreeMap<String, BuiltFixture> = BTreeMap::new();
    for file in &runnable {
        let id = &file.task.fixture;
        if fixtures.contains_key(id) {
            continue;
        }
        let Some(fixture) = discovery.fixture(id) else { continue };
        let dir = scratch.join("fixtures").join(id);
        let built =
            build(&fixture.fixture, &dir, options.item_path).map(|built| (dir, built)).map_err(|e| e.to_string());
        fixtures.insert(id.clone(), built);
    }
    let mut jobs = Vec::new();
    let mut tasks_run = Vec::new();
    for file in runnable {
        if let Some(Err(why)) = fixtures.get(&file.task.fixture) {
            refused.push(Skipped { task: file.task.id.clone(), why: format!("its fixture did not build: {why}") });
            continue;
        }
        tasks_run.push(file.task.id.clone());
        jobs.extend(plan(file, options.attempts));
    }
    let next = AtomicUsize::new(0);
    let finished: Mutex<Vec<(usize, Done)>> = Mutex::new(Vec::new());
    let workers = options.jobs.clamp(1, jobs.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(index) else { break };
                    let built = fixtures.get(&job.file.task.fixture).and_then(|b| b.as_ref().ok());
                    let done = built.map(|(dir, built)| {
                        let caps = job.file.task.caps.over(defaults);
                        execute(discovery, job, dir, built, caps, scratch, options)
                    });
                    if let Some(done) = done {
                        finished.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((index, done));
                    }
                }
            });
        }
    });
    let mut done = finished.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    // Jobs finish in any order; the report and the history do not.
    done.sort_by_key(|(index, _)| *index);
    (done.into_iter().map(|(_, d)| d).collect(), tasks_run)
}

/// The jobs of one task: its recorded attempts, then its oracles.
fn plan(file: &TaskFile, attempts: u32) -> Vec<Job<'_>> {
    let passes: Vec<&super::Script> = file.scripts.iter().filter(|s| s.expect_pass).collect();
    let fails: Vec<&super::Script> = file.scripts.iter().filter(|s| !s.expect_pass).collect();
    let job = |script: &super::Script, label: Label, expected: AttemptOutcome| Job {
        file,
        label,
        script_name: script.file_name.clone(),
        script: parse_script(script),
        expected,
    };
    let stem = file.path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
    let mut jobs = Vec::new();
    let mut used = BTreeSet::new();
    for n in 1..=attempts {
        let index = usize::try_from(n - 1).unwrap_or(0) % passes.len().max(1);
        if let Some(script) = passes.get(index) {
            used.insert(index);
            jobs.push(job(script, Label::Attempt(n), AttemptOutcome::Pass));
        }
    }
    for script in passes.iter().enumerate().filter(|(i, _)| !used.contains(i)).map(|(_, s)| s) {
        jobs.push(job(script, Label::Oracle(label_of(&stem, &script.file_name)), AttemptOutcome::Pass));
    }
    for script in fails {
        jobs.push(job(script, Label::Oracle(label_of(&stem, &script.file_name)), AttemptOutcome::Fail));
    }
    let null_expected = if file.task.null_passes { AttemptOutcome::Pass } else { AttemptOutcome::Fail };
    jobs.push(Job {
        file,
        label: Label::Oracle("null".to_owned()),
        script_name: "<null>".to_owned(),
        script: Ok(Script::default()),
        expected: null_expected,
    });
    jobs
}

/// The folder label of an oracle script: `<stem>.<name>.<pass|fail>.jsonl` as `<name>-<pass|fail>`. The whole name and the
/// verdict are kept, so no two script files of a task share a label (and none can be the empty script's `null`).
fn label_of(stem: &str, file_name: &str) -> String {
    let without = file_name.strip_suffix(".jsonl").unwrap_or(file_name);
    let (rest, verdict) = match (without.strip_suffix(".pass"), without.strip_suffix(".fail")) {
        (Some(rest), _) => (rest, "pass"),
        (_, Some(rest)) => (rest, "fail"),
        _ => (without, "x"),
    };
    let name = rest.strip_prefix(stem).and_then(|r| r.strip_prefix('.')).unwrap_or(rest);
    format!("{name}-{verdict}")
}

fn parse_script(script: &super::Script) -> Result<Script, String> {
    let text = std::str::from_utf8(&script.bytes).map_err(|e| format!("{}: not UTF-8: {e}", script.file_name))?;
    Script::parse(text).map_err(|e| format!("{}: {e}", script.file_name))
}

/// Plays one job: copy the fixture, play, grade, classify.
fn execute(
    discovery: &Discovery,
    job: &Job<'_>,
    fixture_dir: &Path,
    built: &Built,
    caps: Caps,
    scratch: &Path,
    options: &Options<'_>,
) -> Done {
    let task = &job.file.task;
    let label = job.label.folder();
    let work = scratch.join("attempts").join(&task.id).join(&label).join("project");
    let started = Instant::now();
    let played = match &job.script {
        Ok(script) => play_one(job, script, fixture_dir, &work, built, caps, options),
        Err(why) => Err(why.clone()),
    };
    let (outcome, reason, report, log, tool_calls, tools_hash) = match played {
        Ok(p) => {
            let (outcome, reason) = classify(&p.stop, &p.report);
            (outcome, reason, Some(p.report), p.log, p.tool_calls, p.tools_hash)
        }
        Err(why) => (AttemptOutcome::Error, Some(why), None, super::transcript::EventLog::new(), 0, String::new()),
    };
    let transcript = log.transcript();
    let tool_errors = transcript.calls.iter().filter(|c| !c.ok).count();
    let record = AttemptRecord {
        v: SCHEMA,
        run: options.run_id.clone(),
        commit: options.commit.clone(),
        family: FAMILY_TASK.to_owned(),
        driver: "scripted".to_owned(),
        model: None,
        effort: None,
        task: task.id.clone(),
        task_version: task.version,
        task_hash: discovery.task_hash(job.file),
        feature: task.feature.clone(),
        attempt: match job.label {
            Label::Attempt(n) => n,
            Label::Oracle(_) => 0,
        },
        outcome,
        reason: reason.clone(),
        checks: report.as_ref().map(CheckRecord::of_report).unwrap_or_default(),
        turns: 0,
        tool_calls,
        tool_errors: u64::try_from(tool_errors).unwrap_or(u64::MAX),
        retries: 0,
        usage: Usage::default(),
        usd_micros: 0,
        wall_ms: millis(started),
        tools_hash,
        prompt_hash: format!("b3:{}", blake3::hash(task.prompt.as_bytes()).to_hex()),
        harness: HARNESS.to_owned(),
        transcript: format!("evals/runs/{}/{}/{}/", options.run_id, task.id, label),
        script: Some(job.script_name.clone()),
    };
    let deviation = (outcome != job.expected).then(|| Deviation {
        task: task.id.clone(),
        script: job.script_name.clone(),
        label: label.clone(),
        expected: job.expected,
        got: outcome,
        reason: reason.unwrap_or_else(|| "it ended otherwise".to_owned()),
    });
    // The scratch of this attempt is deleted now, to keep the disk small while others run; the evidence is in the run
    // folder. A folder that stays is removed with the whole run's scratch when the run ends, and reported there.
    drop(fs::remove_dir_all(work.parent().unwrap_or(&work)));
    let pictures = report.as_ref().map_or_else(|| Ok(Vec::new()), |r| pictures_of(r, options.store.as_deref()));
    Done {
        record,
        recorded: matches!(job.label, Label::Attempt(_)),
        label,
        transcript: log.to_jsonl(true).map_err(|e| format!("the transcript cannot be written: {e}")),
        checks: report.map_or_else(
            || Ok(String::new()),
            |r| r.to_json().map_err(|e| format!("the check report cannot be written: {e}")),
        ),
        pictures,
        deviation,
    }
}

/// The renders the checks of a report made, as the files the rating page looks for: the first render check's pair is
/// `before.png` and `after.png`, a later one's `<check id>.before.png` and `<check id>.after.png`.
fn pictures_of(
    report: &CheckReport,
    store: Option<&dyn omuzaki_store_api::Store>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut pictures = Vec::new();
    for (n, result) in report.results.iter().filter(|r| !r.renders.is_empty()).enumerate() {
        let store = store
            .ok_or_else(|| format!("check `{}` made renders, and there is no store to fetch them from", result.id))?;
        for (hash, side) in result.renders.iter().zip(["before", "after"]) {
            let parsed =
                hash.parse::<omuzaki_store_api::ContentHash>().map_err(|e| format!("render hash `{hash}`: {e}"))?;
            let bytes = store.get(&parsed).map_err(|e| format!("render {hash} of check `{}`: {e}", result.id))?;
            let name = if n == 0 { format!("{side}.png") } else { format!("{}.{side}.png", result.id) };
            pictures.push((name, bytes));
        }
    }
    Ok(pictures)
}

/// The parts of a played attempt.
struct Played {
    log: super::transcript::EventLog,
    stop: Stop,
    tool_calls: u64,
    tools_hash: String,
    report: CheckReport,
}

fn play_one(
    job: &Job<'_>,
    script: &Script,
    fixture_dir: &Path,
    work: &Path,
    built: &Built,
    caps: Caps,
    options: &Options<'_>,
) -> Result<Played, String> {
    let task = &job.file.task;
    copy_dir(fixture_dir, work).map_err(|e| format!("couldn't copy the fixture: {e}"))?;
    let grants = Grants::of(task.scopes.iter().copied());
    let host = match &options.binary {
        Some(bin) => Host::binary(bin, work, &grants),
        None => Host::in_process(work, grants),
    }
    .map_err(|e| e.to_string())?;
    let driver = Scripted::new(script.clone(), built.exports.clone());
    let attempt = driver.run(task, &host, &caps).map_err(|e| e.to_string())?;
    let names = host.tool_names().clone();
    let tools_hash = host.tools_hash().to_owned();
    host.finish().map_err(|e| e.to_string())?;
    // The checker's own runners: one on the pristine fixture, one on the result folder, reopened from disk.
    let fixture_runner = open_project_runner(fixture_dir).map_err(|e| e.to_string())?;
    let result_runner = open_project_runner(work).map_err(|e| e.to_string())?;
    let checker = Checker::new(fixture_runner, result_runner, attempt.log.transcript())
        .with_exports(built.exports.clone())
        .with_tool_names(names);
    let checker = match &options.store {
        Some(store) => checker.with_store(store.clone()),
        None => checker,
    };
    let report = checker.grade(task);
    Ok(Played { log: attempt.log, stop: attempt.stop, tool_calls: attempt.tool_calls, tools_hash, report })
}

/// How an attempt ended (design 7.1): a cap on tool calls is a fail, a wall-clock timeout and any trouble of the
/// harness's own an error.
fn classify(stop: &Stop, report: &CheckReport) -> (AttemptOutcome, Option<String>) {
    match stop {
        Stop::ScriptFailed { line, reason } => {
            (AttemptOutcome::Error, Some(format!("the script failed at line {line}: {reason}")))
        }
        Stop::WallClock => (AttemptOutcome::Error, Some("the wall-clock cap was reached".to_owned())),
        Stop::ToolCallCap => (AttemptOutcome::Fail, Some("the tool-call cap was reached".to_owned())),
        Stop::Finished => {
            if let Some(broken) = report.results.iter().find(|r| r.outcome == Outcome::Error) {
                return (AttemptOutcome::Error, Some(format!("check `{}` errored: {}", broken.id, broken.detail)));
            }
            match report.results.iter().find(|r| r.hard && r.outcome != Outcome::Pass) {
                None => (AttemptOutcome::Pass, None),
                Some(failed) => {
                    (AttemptOutcome::Fail, Some(format!("check `{}` failed: {}", failed.id, failed.detail)))
                }
            }
        }
    }
}

/// Writes `<out>/runs/<run>/<task>/<label>/{transcript.jsonl,checks.json}`.
fn write_evidence(out: &Path, run: &str, done: &Done) -> Result<(), RunError> {
    let dir = out.join("runs").join(run).join(&done.record.task).join(&done.label);
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| RunError::Io { path: path.clone(), source }
    };
    let evidence = |reason: &String| RunError::Evidence {
        task: done.record.task.clone(),
        label: done.label.clone(),
        reason: reason.clone(),
    };
    let transcript = done.transcript.as_ref().map_err(evidence)?;
    let checks = done.checks.as_ref().map_err(evidence)?;
    let pictures = done.pictures.as_ref().map_err(evidence)?;
    fs::create_dir_all(&dir).map_err(io(&dir))?;
    for (name, bytes) in [("transcript.jsonl", transcript.as_bytes()), ("checks.json", checks.as_bytes())]
        .into_iter()
        .chain(pictures.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice())))
    {
        let path = dir.join(name);
        fs::write(&path, bytes).map_err(io(&path))?;
    }
    Ok(())
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The commit `root` is at, as seven hex digits, or `unknown` when git cannot say.
#[must_use]
pub fn head_commit(root: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_owned(), |s| s.trim().to_owned())
}

/// A run id for a run that begins at `now`: the minute, then the kind (`2026-10-13T1000Z-nightly`).
#[must_use]
pub fn default_run_id(now: SystemTime, kind: &str) -> String {
    format!("{}-{kind}", super::clock::run_stamp(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_file_name_gives_its_label_with_its_whole_name_and_verdict() {
        assert_eq!(label_of("face-more-oval", "face-more-oval.rounder.fail.jsonl"), "rounder-fail");
        assert_eq!(label_of("sample-rename", "sample-rename.extra-edit.fail.jsonl"), "extra-edit-fail");
        assert_eq!(label_of("t", "t.run-1.pass.jsonl"), "run-1-pass");
        // Names with dots, the same name as a pass and a fail script, and a script called `null` stay apart.
        let labels = [
            label_of("t", "t.a.b.fail.jsonl"),
            label_of("t", "t.c.b.fail.jsonl"),
            label_of("t", "t.x.pass.jsonl"),
            label_of("t", "t.x.fail.jsonl"),
            label_of("t", "t.null.fail.jsonl"),
        ];
        let distinct: std::collections::BTreeSet<&String> = labels.iter().collect();
        assert_eq!(distinct.len(), labels.len(), "{labels:?}");
        assert!(!labels.contains(&"null".to_owned()));
    }

    #[test]
    fn a_run_id_and_a_task_id_must_be_plain_folder_names() {
        for good in ["2026-10-13T1000Z-nightly", "manual-2", "a.b_c"] {
            assert!(good.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-')), "{good}");
        }
        for good in ["face.more-oval", "a", "x-1.y"] {
            assert!(task_id_is_a_folder_name(good), "{good}");
        }
        for bad in ["", "..", ".hidden", "a/b", "a..b", "A", "a b", "/etc"] {
            assert!(!task_id_is_a_folder_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_retry_that_claims_a_released_id_never_loses_its_scratch() {
        let tmp = std::env::temp_dir().join(format!("release-order-{}", std::process::id()));
        let no_items = |_: &SetItemRef| Err(String::new());
        let options = Options {
            select: Selection::default(),
            attempts: 1,
            jobs: 1,
            binary: None,
            kind: "manual".to_owned(),
            run_id: "2026-10-13T1000Z-manual".to_owned(),
            commit: "c".to_owned(),
            out: tmp.join("out"),
            status: None,
            scratch: tmp.join("scratch"),
            item_path: &no_items,
            store: None,
        };
        claim_run_id(&options).unwrap();
        let scratch = options.scratch.join(&options.run_id);
        // The failed run has already cleaned up its scratch, as `run_claimed` does before it returns.
        fs::remove_dir_all(&scratch).unwrap();
        // A retry tries to claim the id between the two steps of the release.
        let mut retry = None;
        release_run_id_with(&options, || retry = Some(claim_run_id(&options)));
        match retry.unwrap() {
            // Still held: the retry waits, as it must.
            Err(RunError::Taken(_)) => {}
            // Claimed: then what the claim made must have survived the rest of the release.
            Ok(()) => assert!(scratch.is_dir(), "the retry's scratch was removed by the run that let go of the id"),
            Err(other) => panic!("{other}"),
        }
        // And once the release is done the id can be claimed again.
        release_run_id(&options);
        claim_run_id(&options).unwrap();
        release_run_id(&options);
        drop(fs::remove_dir_all(&tmp));
    }

    #[test]
    fn a_run_id_begins_with_the_minute_and_ends_with_the_kind() {
        let id = default_run_id(SystemTime::UNIX_EPOCH, "nightly");
        assert_eq!(id, "1970-01-01T0000Z-nightly");
    }

    #[test]
    fn selectors_are_a_union_and_no_selector_picks_everything() {
        let file = |id: &str, tags: &[&str], krate: &str| TaskFile {
            path: PathBuf::from("x"),
            crate_name: krate.to_owned(),
            task: toml::from_str(&format!(
                "id='{id}'\nfeature='QG-03'\ntitle='t'\nprompt='p'\nfixture='f'\nscopes=['read']\ntags={tags:?}\n"
            ))
            .unwrap(),
            bytes: Vec::new(),
            scripts: Vec::new(),
        };
        let face = file("face.rounder", &["starter"], "omuzaki-face");
        let hair = file("hair.silver", &[], "omuzaki-char");
        let none = Selection::default();
        assert!(none.picks(&face) && none.picks(&hair));
        let glob = Selection { tasks: vec!["face.*".to_owned()], ..Selection::default() };
        assert!(glob.picks(&face) && !glob.picks(&hair));
        let union = Selection {
            tags: vec!["starter".to_owned()],
            crates: vec!["omuzaki-char".to_owned()],
            ..Selection::default()
        };
        assert!(union.picks(&face) && union.picks(&hair));
    }
}
