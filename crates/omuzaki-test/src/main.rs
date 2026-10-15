//! `omuzaki-test`: the golden-image tools as a command line, for scripts, CI and humans.
//!
//! Exit codes: 0 = images match (or were blessed; `verify` found nothing wrong; `gc` ran), 1 = images differ (or
//! `verify` found a missing or damaged image), 2 = something went wrong (bad arguments, unreadable file, no golden
//! store, no manifest for `verify` to check, or `gc` unable to be sure what is wanted: git failing, a shallow clone, an
//! unreadable worktree, and for `gc --apply` a last `git fetch` too long ago). CI can therefore tell "the render
//! changed" from "the harness broke".

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use clap::{Args, Parser, Subcommand};
use omuzaki_test::compare::{CompareError, Tolerance, compare};
use omuzaki_test::dirs::Dirs;
use omuzaki_test::eval::script::{self, Script};
use omuzaki_test::eval::transcript::EventLog;
use omuzaki_test::eval::{self, lint};
use omuzaki_test::gc::{self, MIN_AGE, Object, Plan, Removal, total_bytes};
use omuzaki_test::goldens::{GoldenId, Goldens, Mode, Outcome, load_png, save_png};
use omuzaki_test::migrate::migrate;
use omuzaki_test::summary::{display_path, load_findings, markdown};
use omuzaki_test::verify::{Fault, Verification, verify};

#[derive(Parser)]
#[command(name = "omuzaki-test", version, about = "Golden-image comparison for Omuzaki's render tests")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compare two PNG files and optionally write a red-overlay diff image.
    Compare {
        /// The image under test.
        actual: PathBuf,
        /// The reference image.
        expected: PathBuf,
        /// Write the diff image here.
        #[arg(long)]
        diff: Option<PathBuf>,
        #[command(flatten)]
        tolerance: ToleranceArgs,
    },
    /// Check an image against the golden `suite/name` in the manifest; `--bless` accepts it as the new golden.
    ///
    /// Blessing stores the image and rewrites `<manifests>/<suite>.toml` in the working tree. The tolerance flags are
    /// recorded for a case that has no manifest entry yet; an existing case keeps the tolerance in its manifest.
    Check {
        /// The golden to check against, as `suite/name`.
        id: String,
        /// The image under test.
        actual: PathBuf,
        /// Accept the image as the golden when it differs (also switched on by `OMUZAKI_BLESS=1`).
        #[arg(long)]
        bless: bool,
        #[command(flatten)]
        tolerance: ToleranceArgs,
        #[command(flatten)]
        store: StoreArgs,
    },
    /// Move goldens from the old `<goldens>/<suite>/<name>.png` layout into manifests and the object store.
    ///
    /// Safe to repeat; the old files are left in place. The tolerance flags are recorded for every moved case.
    Migrate {
        #[command(flatten)]
        tolerance: ToleranceArgs,
        #[command(flatten)]
        store: StoreArgs,
    },
    /// Check that every image the manifests name is in the object store and hashes to its name.
    ///
    /// Lists each failing case and exits 1. Exits 2 when the manifest folder is missing or holds no manifest: checking
    /// nothing is not a pass. Cheap enough to run on every PR.
    Verify {
        #[command(flatten)]
        folders: FolderArgs,
    },
    /// Delete golden images that no branch or worktree names any more; lists them and changes nothing unless `--apply`.
    ///
    /// An image is kept when the manifests at what ANY ref points at (local and remote-tracking branches, tags, the
    /// stash), in any stash entry, at any worktree's HEAD, or in any worktree's files (committed or not) name it, and
    /// always when it is younger than 7 days. Its age counts from the newest of its modification, change and creation
    /// times, so a copy made yesterday with `rsync -a` or `cp -p` is one day old, not as old as the original. Run
    /// `git fetch --all` first so remote branches are current: with the last fetch more than 3 days back it warns, and
    /// `--apply` refuses. Exits 2 and deletes nothing whenever it cannot be sure (shallow or single-branch clone,
    /// unreadable worktree, git failing).
    Gc {
        /// Delete the listed images. Without it nothing is changed.
        #[arg(long)]
        apply: bool,
        /// The default, spelled out for scripts.
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        /// Also keep the images named by the manifests at this git revision (a commit id, say). May be repeated; it
        /// only ever keeps more.
        #[arg(long, value_name = "REV")]
        keep_manifests_of: Vec<String>,
        #[command(flatten)]
        folders: FolderArgs,
    },
    /// Task files for the agent evals (design `docs/design/evals.md`): lint them, list them.
    #[command(subcommand)]
    Eval(EvalCommand),
    /// Print the PR-comment Markdown for everything the checks recorded in the evidence folder.
    Report {
        /// Link to where CI uploaded the evidence folder.
        #[arg(long)]
        artifact_url: Option<String>,
        #[command(flatten)]
        store: StoreArgs,
    },
}

#[derive(Subcommand)]
enum EvalCommand {
    /// Check every task, fixture and eval set under `crates/*/evals`; lists each finding by file and exits 1 if any.
    ///
    /// Rules: duplicate ids, unknown fixture, unknown spec feature, unknown check kind, missing `.pass.jsonl` or
    /// `.fail.jsonl` script, provenance and private-list refusals, and prompts that share 8+ words with a tool
    /// description or server instruction. Exits 2 when the harness itself cannot run (unreadable folder, no spec).
    Lint {
        #[command(flatten)]
        root: RootArg,
        /// A JSON dump of the MCP tools (an array of `{name, description}`, or `{tools, instructions}`) to check the
        /// prompts against. Without it the prompt-leak rule is skipped, and the lint says so.
        #[arg(long, value_name = "JSON")]
        tools: Option<PathBuf>,
    },
    /// Write a script for a task from the tool calls of a recorded attempt (`transcript.jsonl` of a run folder).
    ///
    /// The script lands beside the task as `<task>.<name>.<pass|fail>.jsonl`; an existing script is never replaced.
    /// An argument that is exactly an id the attempt's fixture exported is written back as `{name}`.
    Promote {
        /// The attempt's `transcript.jsonl`.
        transcript: PathBuf,
        /// The task the script is for, by id.
        #[arg(long, value_name = "ID")]
        task: String,
        /// The script's name: lowercase words joined by dashes.
        #[arg(long = "as", value_name = "NAME")]
        name: String,
        /// Whether the script must pass or must fail.
        #[arg(long, value_enum)]
        expect: Expectation,
        #[command(flatten)]
        root: RootArg,
    },
    /// Print every task as JSON: `[{id, feature, crate, tags, version, hash}]`.
    List {
        /// The only output format; spelled out so scripts say what they expect.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        root: RootArg,
    },
}

/// What a promoted script must do.
#[derive(Clone, Copy, clap::ValueEnum)]
enum Expectation {
    /// The task's checks must pass on it.
    Pass,
    /// The task's checks must fail on it.
    Fail,
}

#[derive(Args)]
struct RootArg {
    /// The workspace root (default: the enclosing Omuzaki workspace of the current directory).
    #[arg(long)]
    root: Option<PathBuf>,
}

#[derive(Args)]
struct ToleranceArgs {
    /// A pixel is bad when any channel differs by more than this (0-255; 0 if left out).
    #[arg(long)]
    per_channel: Option<u8>,
    /// The images match when at most this fraction of pixels is bad (0.0-1.0; 0.0 if left out).
    #[arg(long)]
    max_bad_fraction: Option<f64>,
}

impl ToleranceArgs {
    /// The tolerance asked for; a flag left out means no drift allowed on that axis.
    fn tolerance(&self) -> Tolerance {
        Tolerance { per_channel: self.per_channel.unwrap_or(0), max_bad_fraction: self.max_bad_fraction.unwrap_or(0.0) }
    }

    /// The flags that were given and disagree with `recorded`, in the form they were typed.
    fn disagreeing(&self, recorded: Tolerance) -> Vec<String> {
        let per_channel = self.per_channel.filter(|&n| n != recorded.per_channel).map(|n| format!("--per-channel {n}"));
        let fraction = self
            .max_bad_fraction
            .filter(|f| f.total_cmp(&recorded.max_bad_fraction).is_ne())
            .map(|f| format!("--max-bad-fraction {f}"));
        per_channel.into_iter().chain(fraction).collect()
    }
}

/// The two folders every store command needs.
#[derive(Args)]
struct FolderArgs {
    /// Golden store root, holding `objects/<hash>.png` (default: `$OMUZAKI_GOLDENS`).
    #[arg(long)]
    goldens: Option<PathBuf>,
    /// Manifest folder (default: `$OMUZAKI_GOLDEN_MANIFESTS`, else `tests/goldens` of the enclosing workspace).
    #[arg(long)]
    manifests: Option<PathBuf>,
}

impl FolderArgs {
    fn dirs(self) -> Dirs {
        Dirs { store: self.goldens, manifests: self.manifests, out: None }
    }
}

/// The folders plus the evidence folder, for the commands that write evidence.
#[derive(Args)]
struct StoreArgs {
    #[command(flatten)]
    folders: FolderArgs,
    /// Evidence folder (default: `$OMUZAKI_GOLDEN_OUT`, else `<goldens>-out/<name of the workspace folder>`).
    #[arg(long)]
    out: Option<PathBuf>,
}

impl StoreArgs {
    fn dirs(self) -> Dirs {
        Dirs { out: self.out, ..self.folders.dirs() }
    }
}

/// Whether the images matched; errors travel separately as `Err`.
type Verdict = Result<bool, Box<dyn Error>>;

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("omuzaki-test: error: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Verdict {
    match cli.command {
        Command::Compare { actual, expected, diff, tolerance } => {
            match compare(&load_png(&actual)?, &load_png(&expected)?, tolerance.tolerance()) {
                Ok(report) => {
                    if let Some(path) = diff {
                        save_png(&report.diff, &path)?;
                    }
                    println!("{} {}", if report.stats.passed { "ok:" } else { "FAILED:" }, report.stats);
                    Ok(report.stats.passed)
                }
                // Differently sized images are different images (as under `check`), not a harness problem.
                Err(e @ CompareError::SizeMismatch { .. }) => {
                    println!("FAILED: {e}");
                    Ok(false)
                }
                Err(e) => Err(e.into()),
            }
        }
        Command::Check { id, actual, bless, tolerance, store } => {
            let id = GoldenId::parse(&id)?;
            let goldens = Goldens::resolve(store.dirs())?;
            warn_about_ignored_flags(&id, &tolerance, &goldens)?;
            let outcome =
                goldens.check(&id, &load_png(&actual)?, tolerance.tolerance(), Mode::from_flag_or_env(bless))?;
            print_outcome(&id, &outcome, &goldens);
            Ok(outcome.is_ok())
        }
        Command::Migrate { tolerance, store } => {
            let goldens = Goldens::resolve(store.dirs())?;
            let report = migrate(&goldens, tolerance.tolerance())?;
            print_migration(&report, &goldens);
            Ok(report.conflicts.is_empty())
        }
        Command::Verify { folders } => {
            let goldens = Goldens::resolve(folders.dirs())?;
            let result = verify(&goldens)?;
            print_verification(&result, &goldens);
            Ok(result.problems.is_empty())
        }
        Command::Gc { apply, dry_run: _, keep_manifests_of, folders } => {
            let goldens = Goldens::resolve(folders.dirs())?;
            let plan = gc::plan(&goldens, &keep_manifests_of, gc_now(&goldens)?)?;
            print_plan(&plan);
            if apply {
                print_removal(&plan.apply()?);
            } else {
                print_dry_run(&plan);
            }
            Ok(true)
        }
        Command::Eval(EvalCommand::Lint { root, tools }) => eval_lint(&workspace_root(root.root)?, tools.as_deref()),
        Command::Eval(EvalCommand::Promote { transcript, task, name, expect, root }) => {
            eval_promote(&workspace_root(root.root)?, &transcript, &task, &name, matches!(expect, Expectation::Pass))
        }
        Command::Eval(EvalCommand::List { json: _, root }) => eval_list(&workspace_root(root.root)?),
        Command::Report { artifact_url, store } => {
            let dirs = store.dirs().locate_report()?;
            print!("{}", markdown(&load_findings(&dirs.evidence)?, &dirs, artifact_url.as_deref()));
            Ok(true)
        }
    }
}

/// The workspace to read evals from: `--root`, else the nearest folder above the current one that holds `crates/`.
fn workspace_root(given: Option<PathBuf>) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(root) = given {
        return Ok(root);
    }
    let start = std::env::current_dir()?;
    start
        .ancestors()
        .find(|dir| dir.join("crates").is_dir() && dir.join("Cargo.toml").is_file())
        .map(PathBuf::from)
        .ok_or_else(|| "not inside the Omuzaki workspace: run there or pass --root".into())
}

/// `eval lint`: prints each finding as `file: message` and passes when there are none.
fn eval_lint(root: &Path, tools: Option<&Path>) -> Verdict {
    let discovery = eval::discover(root)?;
    let private = eval::load_private(root)?;
    eval::load_defaults(root)?;
    let spec_path = root.join("docs/product/02-studio-spec-v3.md");
    let features = lint::spec_feature_ids(
        &std::fs::read_to_string(&spec_path).map_err(|e| format!("{}: {e}", spec_path.display()))?,
    );
    if features.is_empty() {
        return Err(format!("{}: no feature IDs found", spec_path.display()).into());
    }
    let texts = if let Some(path) = tools {
        let json = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Some(lint::tool_texts_from_json(&json).map_err(|e| format!("{}: {e}", path.display()))?)
    } else {
        eprintln!("note: prompt-leak check skipped: no tool descriptions given (--tools <json>)");
        None
    };
    let findings = lint::lint(&lint::Inputs {
        discovery: &discovery,
        features: &features,
        private: &private,
        tool_texts: texts.as_deref(),
    });
    for finding in &findings {
        println!("{}: {}", finding.file.display(), finding.message);
    }
    println!("{} tasks, {} fixtures, {} findings.", discovery.tasks.len(), discovery.fixtures.len(), findings.len());
    Ok(findings.is_empty())
}

/// `eval promote`: writes the recorded attempt's calls as a script beside its task.
fn eval_promote(root: &Path, transcript: &Path, task: &str, name: &str, expect_pass: bool) -> Verdict {
    let text = std::fs::read_to_string(transcript).map_err(|e| format!("{}: {e}", transcript.display()))?;
    let log = EventLog::from_jsonl(&text).map_err(|e| format!("{}: {e}", transcript.display()))?;
    let script = Script::from_log(&log);
    if script.lines.is_empty() {
        return Err(format!(
            "{}: the attempt made no tool calls, so there is nothing to promote",
            transcript.display()
        )
        .into());
    }
    let discovery = eval::discover(root)?;
    let file = discovery
        .tasks
        .iter()
        .find(|t| t.task.id == task)
        .ok_or_else(|| format!("no task `{task}` under {}", root.display()))?;
    let path = root.join(script::promoted_path(&file.path, name, expect_pass)?);
    // `create_new` so a promoted script never replaces one that is already the oracle of a task.
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::io::Write::write_all(&mut out, script.to_jsonl()?.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {} ({} calls)", path.display(), script.lines.len());
    Ok(true)
}

/// One row of `eval list --json`.
#[derive(serde::Serialize)]
struct TaskRow<'a> {
    id: &'a str,
    feature: &'a str,
    #[serde(rename = "crate")]
    crate_name: &'a str,
    tags: &'a [String],
    version: u32,
    hash: String,
}

/// `eval list --json`: every task, in file order; fails (printing nothing) when any file under `evals/` is malformed.
fn eval_list(root: &Path) -> Verdict {
    let discovery = eval::discover(root)?;
    // A catalogue that silently leaves out a malformed task would be published as complete.
    if !discovery.unparsed.is_empty() {
        for bad in &discovery.unparsed {
            eprintln!("{}: does not parse: {}", bad.path.display(), bad.reason);
        }
        return Ok(false);
    }
    let rows: Vec<TaskRow<'_>> = discovery
        .tasks
        .iter()
        .map(|t| TaskRow {
            id: &t.task.id,
            feature: &t.task.feature,
            crate_name: &t.crate_name,
            tags: &t.task.tags,
            version: t.task.version,
            hash: discovery.task_hash(t),
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(true)
}

/// The time `gc` works as of: the clock, unless [`gc::TEST_CLOCK_ENV`] is set (the test-suite's seam, which `gc::clock`
/// refuses for any store that is not marked as a test store).
fn gc_now(goldens: &Goldens) -> Result<SystemTime, Box<dyn Error>> {
    let setting = match std::env::var(gc::TEST_CLOCK_ENV) {
        Ok(text) => Some(text),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => return Err(format!("{}: {e}", gc::TEST_CLOCK_ENV).into()),
    };
    Ok(gc::clock(goldens, setting.as_deref())?)
}

/// Says so when tolerance flags were given for a case whose manifest entry already has its own: the entry wins, and a
/// flag that silently did nothing would look like it had worked.
fn warn_about_ignored_flags(id: &GoldenId, flags: &ToleranceArgs, goldens: &Goldens) -> Result<(), Box<dyn Error>> {
    if let Some(recorded) = goldens.recorded_tolerance(id)? {
        let ignored = flags.disagreeing(recorded);
        if !ignored.is_empty() {
            eprintln!(
                "note: {id} already has per_channel = {}, max_bad_fraction = {} in {}; {} ignored. Edit the manifest to \
                 change a tolerance.",
                recorded.per_channel,
                recorded.max_bad_fraction,
                goldens.manifest_path(id.suite()).display(),
                ignored.join(" and ") + if ignored.len() == 1 { " is" } else { " are" },
            );
        }
    }
    Ok(())
}

fn print_outcome(id: &GoldenId, outcome: &Outcome, goldens: &Goldens) {
    match outcome {
        Outcome::Passed(stats) => println!("ok: {id}: {stats}"),
        Outcome::Changed(finding) => {
            let numbers = finding.stats.map_or_else(String::new, |s| format!(": {s}"));
            let verb = if finding.is_failure() { "FAILED" } else { "ok" };
            println!("{verb}: {id} ({}){numbers}", finding.status.label());
            if !finding.is_failure() {
                let before = finding.expected_hash.map_or_else(|| "none".to_owned(), |hash| hash.to_string());
                println!(
                    "manifest: {} (hash {before} -> {})",
                    goldens.manifest_path(id.suite()).display(),
                    finding.actual_hash
                );
            }
            println!("evidence: {}", display_path(goldens.out()));
        }
    }
}

fn print_migration(report: &omuzaki_test::migrate::Migration, goldens: &Goldens) {
    for id in &report.migrated {
        println!("migrated: {id}");
    }
    for id in &report.conflicts {
        println!("CONFLICT: {id} already has a different image in its manifest; kept the manifest's");
    }
    for path in &report.skipped {
        println!("skipped: {} (not a golden)", path.display());
    }
    println!(
        "{} migrated, {} already there, {} conflicts, {} skipped. Manifests: {}. The old files are untouched.",
        report.migrated.len(),
        report.unchanged.len(),
        report.conflicts.len(),
        report.skipped.len(),
        goldens.manifests().display()
    );
}

fn print_verification(result: &Verification, goldens: &Goldens) {
    for problem in &result.problems {
        let path = goldens.objects().path(&problem.hash).display().to_string();
        match &problem.fault {
            Fault::Missing => println!("MISSING: {}: no such image in the object store ({path})", problem.id),
            Fault::Corrupt => println!("CORRUPT: {}: {path} no longer hashes to its name", problem.id),
            Fault::Unreadable(why) => println!("UNREADABLE: {}: {why}", problem.id),
        }
    }
    println!(
        "{} of {} have a missing or damaged image, in {}.",
        result.problems.len(),
        plural(result.cases, "case"),
        plural(result.manifests, "manifest")
    );
}

/// "1 image" / "2 images".
fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// Whole days in `span`.
fn days(span: Duration) -> u64 {
    span.as_secs() / 86_400
}

fn describe(object: &Object) -> String {
    // Without this a copied image reads "0 days old" while everyone knows it is a month old.
    let copied = if object.changed_since_stored() {
        format!(
            "; last stored {} days ago, but the file itself changed since (copied or restored?)",
            days(object.modified_age)
        )
    } else {
        String::new()
    };
    format!("{} ({} bytes, {} days old{copied})", object.path.display(), object.bytes, days(object.age))
}

/// The most warnings printed: a clock that is far off would otherwise print one line per image.
const MAX_WARNINGS_SHOWN: usize = 10;

/// Warnings go to stderr, so a script reading the list of images on stdout does not have to skip them.
fn print_warnings(warnings: &[gc::Warning]) {
    for warning in warnings.iter().take(MAX_WARNINGS_SHOWN) {
        eprintln!("warning: {warning}");
    }
    if warnings.len() > MAX_WARNINGS_SHOWN {
        eprintln!("warning: and {} more like these", warnings.len() - MAX_WARNINGS_SHOWN);
    }
}

/// What was read and what was found, shared by the dry run and the real thing.
fn print_plan(plan: &Plan) {
    print_warnings(&plan.warnings);
    for object in &plan.young {
        println!("kept (younger than {} days): {}", days(MIN_AGE), describe(object));
    }
    let scan = plan.scan;
    println!(
        "read {} (ref tips, stash entries, worktree HEADs), {} ({} missing on disk) and {}.",
        plural(scan.commits, "commit"),
        plural(scan.worktrees, "worktree"),
        scan.missing_worktrees,
        plural(scan.manifests, "manifest file"),
    );
    println!(
        "{} named by a manifest, {} unreferenced but too young to delete.",
        plural(plan.wanted, "image"),
        plan.young.len()
    );
}

fn print_dry_run(plan: &Plan) {
    for object in &plan.garbage {
        println!("would delete: {}", describe(object));
    }
    println!(
        "dry run: {} ({} bytes) would be deleted; nothing was changed. Run with --apply to delete them.",
        plural(plan.garbage.len(), "image"),
        total_bytes(&plan.garbage)
    );
}

fn print_removal(removal: &Removal) {
    for object in &removal.removed {
        println!("deleted: {}", describe(object));
    }
    for object in &removal.touched {
        println!("kept (changed since the scan): {}", describe(object));
    }
    println!("deleted {} ({} bytes).", plural(removal.removed.len(), "image"), total_bytes(&removal.removed));
}
