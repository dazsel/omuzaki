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
use omuzaki_test::eval::provenance::{Provenance, Source as ProvenanceSource};
use omuzaki_test::eval::run::{self, Options, RunReport, Selection};
use omuzaki_test::eval::script::{self, Script};
use omuzaki_test::eval::sets::{self, SetStore};
use omuzaki_test::eval::transcript::EventLog;
use omuzaki_test::eval::{self, gate, gc as run_gc, history, lint, rate};
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
    /// Play tasks, grade them and keep the evidence (design 6.4, 7).
    ///
    /// Each fixture is built once, copied for every attempt and the copy deleted afterwards. The scripted driver records
    /// `--attempts` attempts of each task's pass script and also plays its other scripts and the empty script once
    /// (they must end as their names say). Exits 1 when any does not, or when a task is refused before it runs. With
    /// `--record` one line per attempt goes to `eval-history.jsonl` and one for the run to `eval-runs.jsonl`.
    ///
    /// Selectors (`--tasks`, `--tags`, `--crates`) add up: a task any of them picks is played; none given plays all.
    Run {
        /// Who plays the attempts. The reference-model driver spends money and arrives with its own task.
        #[arg(long, value_enum, default_value = "scripted")]
        driver: DriverArg,
        /// Task id globs, comma-separated (`face.*`).
        #[arg(long, value_delimiter = ',', value_name = "GLOB")]
        tasks: Vec<String>,
        /// Tags, comma-separated: a task with any of them is played (`starter`).
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Crates, comma-separated: every task of their `evals/` folders is played (a pull request's affected crates).
        #[arg(long, value_delimiter = ',')]
        crates: Vec<String>,
        /// Recorded attempts of each task.
        #[arg(long, default_value_t = 1)]
        attempts: u32,
        /// Attempts played at once.
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        /// Append the attempts and the run to the history in the status folder.
        #[arg(long)]
        record: bool,
        /// Play through `omuzaki mcp` as a child process instead of in this process. The binary is `--omuzaki-bin`,
        /// else `$OMUZAKI_BIN`.
        #[arg(long)]
        binary: bool,
        /// The `omuzaki` binary for `--binary`.
        #[arg(long, value_name = "PATH")]
        omuzaki_bin: Option<PathBuf>,
        /// The most the run may spend, in dollars. Only the reference driver spends.
        #[arg(long, value_name = "USD")]
        spend_usd: Option<f64>,
        /// What kind of run this is: pr, nightly, weekly or manual.
        #[arg(long, default_value = "manual")]
        kind: String,
        /// The run's id (default: the minute it began, then the kind).
        #[arg(long)]
        run_id: Option<String>,
        #[command(flatten)]
        root: RootArg,
    },
    /// Say which tasks and suites regressed, from the history (design 8).
    ///
    /// Scores the reference driver's attempts: a task regresses when its latest ten scored attempts are improbably few
    /// passes under its own rate of the 28 days before (`P(X <= k | n = 10, p0) < 0.01`), and a model's nightly suite when
    /// it falls below the median of the 14 nights before by more than max(5 points, 3 x MAD). Series with under 20 scored
    /// attempts are provisional and never flag. Exits 1 on a regression, 0 otherwise; warnings do not fail it.
    Gate {
        /// Print the report as JSON: every series with its window, baseline, commits and transcripts.
        #[arg(long)]
        json: bool,
        /// The status folder holding the history (default: `$OMUZAKI_EVAL_STATUS`, else `$OMUZAKI_ASSETS/status`).
        #[arg(long, value_name = "DIR")]
        status: Option<PathBuf>,
    },
    /// Rate a few results yourself, on a page served on this machine only (design 10.3).
    ///
    /// Picks up to 20 unrated results a week (reference attempts of tasks that have questions for people, spread across
    /// the tasks), serves them on `127.0.0.1` and appends your yes/no/unsure answers to `<evals>/ratings/<set>.jsonl`.
    /// Nothing is uploaded. The page works from the keyboard: Y, N and U answer, the arrows move between results.
    Rate {
        /// The review set's name, such as `review-1`.
        #[arg(long)]
        set: String,
        /// How many results to rate this time (at most 20 a week).
        #[arg(long, default_value_t = 20)]
        sample: usize,
        /// Who is rating, as the ratings file records it.
        #[arg(long, default_value = "maintainer")]
        rater: String,
        /// The port to serve on (0 lets the system choose).
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[command(flatten)]
        root: RootArg,
    },
    /// How often a judge agrees with the raters on the clear-cut items of a review set; appended to the history.
    ///
    /// The judge is a check id: its answers are read from the history's attempts. A hard check answers yes when it passed,
    /// a soft one when its score is at least a half. A `judge` check answers its only rubric question; any other check needs
    /// `--question`. A judge may become a hard check at 90% over 40 items (design 10.4) and after its known-negatives test.
    Agreement {
        /// The check id of the judge.
        #[arg(long)]
        judge: String,
        /// The review set the raters' answers are in.
        #[arg(long)]
        set: String,
        /// The rubric question the check answers, when the check does not say.
        #[arg(long)]
        question: Option<String>,
        /// Print the measurement without appending it to the history.
        #[arg(long)]
        no_record: bool,
        #[command(flatten)]
        root: RootArg,
    },
    /// Eval sets: manifests in git, items in the content store (design 11).
    #[command(subcommand)]
    Sets(SetsCommand),
    /// Remove run folders older than 90 days, unless an open issue or a promoted script names them.
    ///
    /// A dry run unless given `--apply`. The open issues are read with `gh`; if that fails nothing is removed.
    Gc {
        /// Remove what the plan lists.
        #[arg(long)]
        apply: bool,
        /// Read the open issues' text from this file instead of asking `gh` (for offline use and tests).
        #[arg(long, value_name = "FILE")]
        issues: Option<PathBuf>,
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

#[derive(Subcommand)]
enum SetsCommand {
    /// List the items the manifests name that the store lacks or holds damaged. Exits 1 if there are any.
    ///
    /// The store is `$OMUZAKI_EVAL_SETS`, else `$OMUZAKI_ASSETS/evals/sets`; every read of an item checks its bytes
    /// against its hash.
    Verify {
        #[command(flatten)]
        root: RootArg,
    },
    /// Store a file as an item of a set and write its manifest entry.
    ///
    /// The file is judged before it is stored: an item with no clean provenance record, or whose path or hash is on the
    /// private list (`docs/licensing/private-material.toml`), is refused and never enters the store. The manifest records
    /// the hash and the provenance, and no machine path. A set that does not exist yet is created with `--new-in`.
    Add(Box<AddArgs>),
    /// Fail when a training manifest names an item of a `real` set or a `holdout` split (ML-12).
    HoldoutCheck {
        /// The training manifests to look in (any text format that spells the item hashes).
        #[arg(required = true)]
        manifests: Vec<PathBuf>,
        #[command(flatten)]
        root: RootArg,
    },
}

/// The arguments of `eval sets add`.
#[derive(Args)]
struct AddArgs {
    /// The set's id.
    set: String,
    /// The file to store.
    file: PathBuf,
    /// Where the item came from.
    #[arg(long, value_enum)]
    source: SourceArg,
    /// Whether commercial use is clear; must be `yes`.
    #[arg(long, default_value = "yes")]
    commercial: String,
    /// A pointer to the consent record, for an `own` item that shows a person.
    #[arg(long)]
    consent: Option<String>,
    /// The item shows a person.
    #[arg(long)]
    person: bool,
    /// The licence, for a `licensed` item.
    #[arg(long)]
    licence: Option<String>,
    /// The generator's commit, for a `synthetic` item.
    #[arg(long)]
    generator: Option<String>,
    /// A relative origin path to record in the manifest (absolute paths are refused).
    #[arg(long = "path", value_name = "RELATIVE")]
    recorded_path: Option<String>,
    /// Create the set in this crate's `evalsets/` folder (with --kind, --split and --summary).
    #[arg(long, value_name = "CRATE")]
    new_in: Option<String>,
    /// The new set's kind: `real` or `synthetic`.
    #[arg(long, requires = "new_in")]
    kind: Option<String>,
    /// The new set's split: `dev` or `holdout`.
    #[arg(long, requires = "new_in")]
    split: Option<String>,
    /// One line about the new set.
    #[arg(long, requires = "new_in")]
    summary: Option<String>,
    #[command(flatten)]
    root: RootArg,
}

/// Where an eval-set item came from (design 11.2).
#[derive(Clone, Copy, clap::ValueEnum)]
enum SourceArg {
    /// Made by our code.
    Synthetic,
    /// Made for the project by its authors.
    Own,
    /// CC0.
    Cc0,
    /// Public domain.
    PublicDomain,
    /// Under a licence that allows commercial use and use in tests.
    Licensed,
}

impl From<SourceArg> for ProvenanceSource {
    fn from(arg: SourceArg) -> Self {
        match arg {
            SourceArg::Synthetic => Self::Synthetic,
            SourceArg::Own => Self::Own,
            SourceArg::Cc0 => Self::Cc0,
            SourceArg::PublicDomain => Self::PublicDomain,
            SourceArg::Licensed => Self::Licensed,
        }
    }
}

/// Who plays the attempts of a run.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DriverArg {
    /// Replays scripts: no model, deterministic, free.
    Scripted,
    /// A pinned model through our own agent loop. Not built yet.
    Reference,
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
        Command::Eval(EvalCommand::Run {
            driver,
            tasks,
            tags,
            crates,
            attempts,
            jobs,
            record,
            binary,
            omuzaki_bin,
            spend_usd,
            kind,
            run_id,
            root,
        }) => {
            let root = workspace_root(root.root)?;
            let select = Selection { tasks, tags, crates };
            let plan = RunArgs { driver, attempts, jobs, record, binary, omuzaki_bin, spend_usd, kind, run_id };
            eval_run(&root, select, plan)
        }
        Command::Eval(EvalCommand::Rate { set, sample, rater, port, root }) => {
            eval_rate(&workspace_root(root.root)?, &set, sample, &rater, port)
        }
        Command::Eval(EvalCommand::Agreement { judge, set, question, no_record, root }) => {
            eval_agreement(&workspace_root(root.root)?, &judge, &set, question.as_deref(), no_record)
        }
        Command::Eval(EvalCommand::Sets(sets)) => eval_sets(sets),
        Command::Eval(EvalCommand::Gate { json, status }) => eval_gate(json, status),
        Command::Eval(EvalCommand::Gc { apply, issues, root }) => {
            eval_gc(&workspace_root(root.root)?, apply, issues.as_deref())
        }
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

/// The settings of `eval run` besides what to play.
struct RunArgs {
    driver: DriverArg,
    attempts: u32,
    jobs: usize,
    record: bool,
    binary: bool,
    omuzaki_bin: Option<PathBuf>,
    spend_usd: Option<f64>,
    kind: String,
    run_id: Option<String>,
}

/// The folder a setting names: the variable `own`, else `sub` under `$OMUZAKI_ASSETS`.
fn assets_folder(own: &str, sub: &str) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(dir) = std::env::var_os(own) {
        return Ok(PathBuf::from(dir));
    }
    match std::env::var_os("OMUZAKI_ASSETS") {
        Some(assets) => Ok(PathBuf::from(assets).join(sub)),
        None => Err(format!("set {own} (or OMUZAKI_ASSETS, whose `{sub}` folder is the default)").into()),
    }
}

/// `eval run`: plays the selected tasks and prints how each attempt ended.
fn eval_run(root: &Path, select: Selection, args: RunArgs) -> Verdict {
    if args.driver == DriverArg::Reference {
        return Err("the reference-model driver is a separate task: it needs the LLM providers and the maintainers' spend decision; use --driver scripted".into());
    }
    if args.spend_usd.is_some() {
        return Err("--spend-usd caps what a run spends, and the scripted driver spends nothing".into());
    }
    let binary = if args.binary {
        let bin = args
            .omuzaki_bin
            .or_else(|| std::env::var_os("OMUZAKI_BIN").map(PathBuf::from))
            .ok_or("--binary needs the `omuzaki` binary: pass --omuzaki-bin or set OMUZAKI_BIN")?;
        Some(bin)
    } else {
        None
    };
    let discovery = eval::discover(root)?;
    // A file that does not parse would silently be left out of the run: refuse, and let `eval lint` say which.
    if let Some(bad) = discovery.unparsed.first() {
        return Err(format!("{}: does not parse: {} (run `eval lint`)", bad.path.display(), bad.reason).into());
    }
    let defaults = eval::load_defaults(root)?;
    let private = eval::load_private(root)?;
    let now = SystemTime::now();
    let run_id = args.run_id.unwrap_or_else(|| run::default_run_id(now, &args.kind));
    let status = if args.record { Some(assets_folder("OMUZAKI_EVAL_STATUS", "status")?) } else { None };
    // Only a fixture that refers to an eval-set item needs the store, so a store that is not configured is not yet a fault.
    let store = SetStore::locate().ok();
    let item_path = |item: &eval::fixture::SetItemRef| -> Result<String, String> {
        let store = store.as_ref().ok_or_else(|| {
            format!("item {} of set `{}` needs the store: set OMUZAKI_EVAL_SETS or OMUZAKI_ASSETS", item.item, item.set)
        })?;
        let hash = sets::store::parse_hash(&item.item).map_err(|e| e.to_string())?;
        store.verified_path(&hash).map(|path| path.display().to_string()).map_err(|e| e.to_string())
    };
    let options = Options {
        select,
        attempts: args.attempts,
        jobs: args.jobs,
        binary,
        kind: args.kind,
        run_id,
        commit: run::head_commit(root),
        out: assets_folder("OMUZAKI_EVAL_OUT", "evals")?,
        status,
        scratch: std::env::temp_dir().join("scratch").join("evals"),
        item_path: &item_path,
    };
    let report = run::run_scripted(&discovery, defaults, &private, &options)?;
    print_run(&report);
    Ok(report.ok())
}

fn print_run(report: &RunReport) {
    for attempt in &report.attempts {
        let reason = attempt.reason.as_deref().map_or_else(String::new, |r| format!(": {r}"));
        println!(
            "{:<7} {} #{} ({}){reason}",
            attempt.outcome.word(),
            attempt.task,
            attempt.attempt,
            attempt.script.as_deref().unwrap_or("-")
        );
    }
    for refused in &report.refused {
        println!("REFUSED {}: {}", refused.task, refused.why);
    }
    for d in &report.deviations {
        println!(
            "DEVIATION {} [{}] {}: expected {}, got {}: {}",
            d.task,
            d.label,
            d.script,
            d.expected.word(),
            d.got.word(),
            d.reason
        );
    }
    let run = &report.run;
    println!(
        "run {}: {} attempts ({} pass, {} fail, {} error); {} oracle scripts played, {} off; {} tasks refused; {}",
        run.run,
        run.attempts.total(),
        run.attempts.pass,
        run.attempts.fail,
        run.attempts.error,
        run.oracles.played,
        run.oracles.deviations,
        report.refused.len(),
        if run.valid { "valid" } else { "NOT valid: more than 10% of the attempts errored" },
    );
}

/// `eval rate`: serves a sample of unrated results until they are rated or the rater finishes.
fn eval_rate(root: &Path, set: &str, sample: usize, rater: &str, port: u16) -> Verdict {
    let evals = assets_folder("OMUZAKI_EVAL_OUT", "evals")?;
    let status = assets_folder("OMUZAKI_EVAL_STATUS", "status")?;
    let ratings_path = rate::ratings_path(&evals, set)?;
    let discovery = eval::discover(root)?;
    let history = history::read_attempts(&status.join(history::HISTORY_FILE))?;
    let ratings = rate::read(&ratings_path)?;
    let now = SystemTime::now();
    let count = sample.min(rate::sample::budget_left(&ratings, now));
    if count == 0 {
        println!("Nothing to rate: this week's limit of {} results is reached (or --sample is 0).", rate::WEEKLY_ITEMS);
        return Ok(true);
    }
    let items = rate::sample::sample(&discovery, &history, &ratings, count);
    if items.is_empty() {
        println!("Nothing to rate: no unrated reference result of a task with questions for people.");
        return Ok(true);
    }
    let listener = rate::server::bind(port)?;
    let session = rate::server::Session {
        items,
        ratings: ratings_path,
        runs: evals.join("runs"),
        rater: rater.to_owned(),
        token: rate::server::new_token()?,
        clock: SystemTime::now,
    };
    let url = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    println!("Rating {} result(s). Open {url} in a browser on this machine; nothing is uploaded.", session.items.len());
    let ended = rate::server::serve(&listener, &session)?;
    println!("{}", if ended == rate::server::Ended::Done { "All rated." } else { "Finished before all were rated." });
    Ok(true)
}

/// `eval agreement`: measures a judge against the ratings and appends the record.
fn eval_agreement(root: &Path, judge: &str, set: &str, question: Option<&str>, no_record: bool) -> Verdict {
    let evals = assets_folder("OMUZAKI_EVAL_OUT", "evals")?;
    let status = assets_folder("OMUZAKI_EVAL_STATUS", "status")?;
    let discovery = eval::discover(root)?;
    let history = history::read_attempts(&status.join(history::HISTORY_FILE))?;
    let ratings = rate::read(&rate::ratings_path(&evals, set)?)?;
    let source = rate::agreement::HistoryJudge::from_history(&discovery, &history, judge, question)?;
    let measured = rate::agreement::agreement(&ratings, &|rating| source.answer(rating));
    let rate_text =
        measured.rate().map_or_else(|| "no clear-cut item was judged".to_owned(), |r| format!("{:.1}%", r * 100.0));
    println!(
        "judge `{judge}` agrees with the raters on {} of {} clear-cut items ({rate_text}); {} unsure left out, {} without a judge answer.",
        measured.agree, measured.items, measured.unsure, measured.unjudged
    );
    println!(
        "To become a hard check it needs {:.0}% over {} items{} and the known-negatives test.",
        rate::agreement::PROMOTE_AGREEMENT * 100.0,
        rate::agreement::PROMOTE_ITEMS,
        if measured.meets_threshold() { " (these numbers meet that)" } else { " (these numbers do not)" }
    );
    if !no_record {
        let now = SystemTime::now();
        let record = rate::agreement::JudgeRecord::new(
            &run::default_run_id(now, "agreement"),
            &run::head_commit(root),
            judge,
            set,
            &measured,
        );
        history::append(&status.join(history::HISTORY_FILE), &record)?;
    }
    Ok(true)
}

/// `eval sets ...`.
fn eval_sets(command: SetsCommand) -> Verdict {
    match command {
        SetsCommand::Verify { root } => {
            let discovery = eval::discover(&workspace_root(root.root)?)?;
            let result = sets::verify::verify(&discovery, &SetStore::locate()?)?;
            for f in &result.findings {
                println!("{}: set `{}` item {}: {}", f.manifest.display(), f.set, f.item, f.problem.describe());
            }
            println!("{} item(s) checked, {} problem(s).", result.checked, result.findings.len());
            Ok(result.findings.is_empty())
        }
        SetsCommand::Add(args) => {
            let AddArgs {
                set,
                file,
                source,
                commercial,
                consent,
                person,
                licence,
                generator,
                recorded_path,
                new_in,
                kind,
                split,
                summary,
                root,
            } = *args;
            let root = workspace_root(root.root)?;
            let discovery = eval::discover(&root)?;
            // A manifest that does not parse could be the very set being added to, or one that shares its id.
            if let Some(bad) = discovery.unparsed.first() {
                return Err(format!("{}: does not parse: {} (run `eval lint`)", bad.path.display(), bad.reason).into());
            }
            let new_set = match new_in {
                Some(crate_name) => Some(sets::add::NewSet {
                    crate_name,
                    kind: kind.ok_or("--new-in needs --kind")?,
                    split: split.ok_or("--new-in needs --split")?,
                    summary: summary.ok_or("--new-in needs --summary")?,
                }),
                None => None,
            };
            let record = Provenance { source: source.into(), commercial, consent, person, licence, generator };
            let request = sets::add::Request { set, file, record, recorded_path, new_set };
            let added = sets::add::add(&root, &discovery, &eval::load_private(&root)?, &SetStore::locate()?, &request)?;
            let what = if added.created_set {
                "created the set and stored"
            } else if added.new_item {
                "stored"
            } else {
                "already in"
            };
            println!("{what} b3:{} in {}", added.hash, added.manifest.display());
            Ok(true)
        }
        SetsCommand::HoldoutCheck { manifests, root } => {
            let discovery = eval::discover(&workspace_root(root.root)?)?;
            let leaks = sets::holdout::holdout_check(&discovery, &manifests)?;
            for leak in &leaks {
                println!(
                    "{}: names item {} of set `{}` ({}), which is test-only and must never be training data",
                    leak.manifest.display(),
                    leak.item,
                    leak.set,
                    leak.why
                );
            }
            println!("{} training manifest(s) checked, {} leak(s).", manifests.len(), leaks.len());
            Ok(leaks.is_empty())
        }
    }
}

/// `eval gate`: reads the history and prints what regressed.
fn eval_gate(json: bool, status: Option<PathBuf>) -> Verdict {
    let status = match status {
        Some(dir) => dir,
        None => assets_folder("OMUZAKI_EVAL_STATUS", "status")?,
    };
    let attempts = history::read_attempts(&status.join(history::HISTORY_FILE))?;
    let runs = history::read_runs(&status.join(history::RUNS_FILE))?;
    let report = gate::gate(&attempts, &runs);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_gate(&report);
    }
    Ok(!report.failed())
}

fn print_gate(report: &gate::GateReport) {
    for task in &report.tasks {
        let model = task.model.as_deref().unwrap_or("-");
        let [low, high] = task.window.wilson;
        let base = task.baseline.map_or_else(
            || "no baseline".to_owned(),
            |b| format!("baseline {}/{} = {:.0}%", b.passes, b.n, b.rate * 100.0),
        );
        println!(
            "{:<10} {} v{} ({model}): window {}/{} = {:.0}% [{:.0}%..{:.0}%], {base}{}",
            format!("{:?}", task.status).to_lowercase(),
            task.task,
            task.version,
            task.window.passes,
            task.window.n,
            task.window.rate * 100.0,
            low * 100.0,
            high * 100.0,
            if task.weak { ", WEAK: fix the tools" } else { "" },
        );
    }
    for suite in &report.suites {
        let said = match (suite.rate, suite.median, suite.tolerance) {
            (Some(rate), Some(median), Some(tolerance)) => format!(
                "{:.1}% against a median of {:.1}% (tolerance {:.1} points){}",
                rate * 100.0,
                median * 100.0,
                tolerance * 100.0,
                if suite.regression { ": REGRESSION" } else { "" }
            ),
            (Some(rate), ..) => format!("{:.1}%, fewer than 14 nights before it to judge by", rate * 100.0),
            _ => "no non-provisional task ran".to_owned(),
        };
        println!("suite {} ({}): {said}", suite.model, suite.run);
    }
    for warning in &report.warnings {
        println!("warning {}: {}", warning.task, warning.message);
    }
}

/// `eval gc`: lists what would go, and removes it with `--apply`.
fn eval_gc(root: &Path, apply: bool, issues: Option<&Path>) -> Verdict {
    let discovery = eval::discover(root)?;
    let bodies = match issues {
        Some(file) => vec![std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?],
        None => run_gc::open_issue_bodies()?,
    };
    let named = run_gc::Named { issues: bodies, scripts: Vec::new() }.with_scripts_of(&discovery);
    let out = assets_folder("OMUZAKI_EVAL_OUT", "evals")?;
    let plan = run_gc::plan(&out, SystemTime::now(), &named)?;
    for entry in &plan.entries {
        match &entry.kept {
            Some(why) => println!("keep   {}: {why}", entry.run),
            None => println!("remove {}", entry.run),
        }
    }
    let going = plan.removals().count();
    if apply {
        let removed = plan.apply()?;
        println!("removed {} run folder(s).", removed.len());
    } else {
        println!("{going} run folder(s) would be removed; pass --apply to remove them.");
    }
    Ok(true)
}

/// `eval promote`: writes the recorded attempt's calls as a script beside its task.
fn eval_promote(root: &Path, transcript: &Path, task: &str, name: &str, expect_pass: bool) -> Verdict {
    let text = std::fs::read_to_string(transcript).map_err(|e| format!("{}: {e}", transcript.display()))?;
    let log = EventLog::from_jsonl(&text).map_err(|e| format!("{}: {e}", transcript.display()))?;
    let script = Script::from_log(&log);
    let script = match attempt_folder(transcript) {
        Some(source) => script.promoted_from(source),
        None => script,
    };
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

/// `<run>/<task>/<attempt>` of a `transcript.jsonl` that lies in a run folder (`.../runs/<run>/<task>/<attempt>/`).
fn attempt_folder(transcript: &Path) -> Option<String> {
    let folder = transcript.parent()?;
    let parts: Vec<&str> = folder.components().filter_map(|c| c.as_os_str().to_str()).collect();
    let runs = parts.iter().rposition(|p| *p == "runs")?;
    let tail = parts.get(runs + 1..)?;
    (tail.len() == 3).then(|| tail.join("/"))
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
