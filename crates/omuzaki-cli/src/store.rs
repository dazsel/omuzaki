//! `omuzaki store verify` and `omuzaki store repair`: the asset store's own checks on the command line.
//!
//! The store is a folder the person names with `--store`, not part of the project, so these commands need neither a
//! project nor an op runner. `verify` changes nothing. `repair` moves what it can't trust into the store's quarantine
//! folder and never deletes, so a wrong call can be undone by moving a folder back.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use omuzaki_ops_api::OpError;
use omuzaki_store::{DEFAULT_STALE_AFTER, DiskStore, Problem, RepairReport, VerifyReport};
use omuzaki_store_api::StoreError;
use serde::Serialize;

use crate::cli::{StoreArgs, StoreCommand};
use crate::exit;
use crate::json;
use crate::output::Reply;

/// Runs a `store` command. The reply's code is [`exit::FOUND_PROBLEMS`] when `verify` finds a problem, or when `repair`
/// leaves one alone; otherwise [`exit::OK`].
///
/// # Errors
/// `NotFound` if the folder has no `objects` folder in it (so a typo isn't mistaken for an empty store); `Invalid` if
/// the store can't be read or written; `Internal` if the store reports a state it should never reach.
pub fn run(action: &StoreCommand, json: bool) -> Result<Reply, OpError> {
    match action {
        StoreCommand::Verify(args) => verify(args, json),
        StoreCommand::Repair(args) => repair(args, json),
    }
}

fn verify(args: &StoreArgs, json: bool) -> Result<Reply, OpError> {
    let store = open(&args.store)?;
    let report = store.verify(threshold(args)).map_err(|e| store_error(&e))?;
    let code = if report.is_clean() { exit::OK } else { exit::FOUND_PROBLEMS };
    let text = if json { json::pretty(&VerifyJson::from(&report))? } else { verify_text(&report) };
    Ok(Reply { text, code })
}

fn repair(args: &StoreArgs, json: bool) -> Result<Reply, OpError> {
    let store = open(&args.store)?;
    let report = store.repair(threshold(args)).map_err(|e| store_error(&e))?;
    let code = if report.left_alone.is_empty() { exit::OK } else { exit::FOUND_PROBLEMS };
    let quarantine = args.store.join("quarantine");
    let text = if json { json::pretty(&RepairJson::from(&report))? } else { repair_text(&report, &quarantine) };
    Ok(Reply { text, code })
}

fn threshold(args: &StoreArgs) -> Duration {
    args.stale_after.unwrap_or(DEFAULT_STALE_AFTER)
}

/// Opens the store at `root` without creating anything: `DiskStore::open` would make an `objects` folder in any
/// folder, so a mistyped `--store` would quietly report an empty, clean store.
#[allow(clippy::disallowed_methods, reason = "the CLI checks the store folder the person typed, as a script would")]
fn open(root: &Path) -> Result<DiskStore, OpError> {
    if !root.join("objects").is_dir() {
        return Err(OpError::not_found(format!("no store at {}", root.display()))
            .hint("a store is a folder with an objects folder in it; pass its folder: --store <DIR>"));
    }
    DiskStore::open(root).map_err(|e| store_error(&e))
}

fn store_error(error: &StoreError) -> OpError {
    let message = error.to_string();
    match error {
        StoreError::Io(_) => OpError::invalid("store", message)
            .hint("check the store folder and its permissions, then run the command again"),
        // `verify` and `repair` report damage as problems; these come only from `get` and `put`, which they never call.
        StoreError::Missing(_) | StoreError::Corrupt(_) => {
            OpError::internal(message).hint("this is a bug in Omuzaki: report it")
        }
    }
}

/// The text for `verify`: the object count, then one line per problem (each names its asset or path).
fn verify_text(report: &VerifyReport) -> String {
    let checked = count(report.checked, "object");
    if report.is_clean() {
        return format!("checked {checked}: the store is clean\n");
    }
    let mut text = format!("checked {checked}: {}\n", count(report.problems.len(), "problem"));
    for problem in &report.problems {
        // Writing to a String cannot fail.
        let _ = writeln!(text, "  {problem}");
    }
    text
}

/// The text for `repair`: what moved where, what was left alone, and where the quarantine is.
fn repair_text(report: &RepairReport, quarantine: &Path) -> String {
    let mut text = format!("checked {}\n", count(report.checked, "object"));
    if report.quarantined.is_empty() && report.left_alone.is_empty() {
        text.push_str("nothing to repair: the store is clean\n");
        return text;
    }
    if !report.quarantined.is_empty() {
        text.push_str("moved to quarantine:\n");
        for moved in &report.quarantined {
            let _ = writeln!(text, "  {}\n    -> {}", moved.problem, moved.moved_to.display());
        }
        let _ = writeln!(text, "the quarantine folder {} is yours to delete once you are sure", quarantine.display());
    }
    if !report.left_alone.is_empty() {
        text.push_str("left alone (needs a person):\n");
        for problem in &report.left_alone {
            let _ = writeln!(text, "  {problem}");
        }
    }
    text
}

/// `1 object`, `2 objects`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

/// The name a script branches on for each kind of problem; the text says the same in words.
fn kind(problem: &Problem) -> &'static str {
    match problem {
        Problem::Corrupt(_) => "corrupt",
        Problem::MissingData(_) => "missing_data",
        Problem::MissingProvenance(_) => "missing_provenance",
        Problem::BadProvenance { .. } => "bad_provenance",
        Problem::Unreadable { .. } => "unreadable",
        Problem::StaleStaging { .. } => "stale_staging",
        Problem::Stray(_) => "stray",
    }
}

/// One problem in `--json` output.
#[derive(Serialize)]
struct ProblemJson {
    kind: &'static str,
    text: String,
}

impl From<&Problem> for ProblemJson {
    fn from(problem: &Problem) -> Self {
        Self { kind: kind(problem), text: problem.to_string() }
    }
}

/// `verify --json`.
#[derive(Serialize)]
struct VerifyJson {
    checked: usize,
    clean: bool,
    problems: Vec<ProblemJson>,
}

impl From<&VerifyReport> for VerifyJson {
    fn from(report: &VerifyReport) -> Self {
        Self {
            checked: report.checked,
            clean: report.is_clean(),
            problems: report.problems.iter().map(ProblemJson::from).collect(),
        }
    }
}

/// One problem repair moved, in `--json` output.
#[derive(Serialize)]
struct QuarantinedJson {
    kind: &'static str,
    text: String,
    moved_to: String,
}

/// `repair --json`.
#[derive(Serialize)]
struct RepairJson {
    checked: usize,
    quarantined: Vec<QuarantinedJson>,
    left_alone: Vec<ProblemJson>,
}

impl From<&RepairReport> for RepairJson {
    fn from(report: &RepairReport) -> Self {
        Self {
            checked: report.checked,
            quarantined: report
                .quarantined
                .iter()
                .map(|moved| QuarantinedJson {
                    kind: kind(&moved.problem),
                    text: moved.problem.to_string(),
                    moved_to: moved.moved_to.display().to_string(),
                })
                .collect(),
            left_alone: report.left_alone.iter().map(ProblemJson::from).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn counts_read_naturally_in_both_numbers() {
        assert_eq!(count(0, "object"), "0 objects");
        assert_eq!(count(1, "problem"), "1 problem");
        assert_eq!(count(2, "object"), "2 objects");
    }

    #[test]
    fn a_clean_store_says_so_and_a_repair_with_nothing_to_do_says_so() {
        let clean = VerifyReport { checked: 3, problems: vec![] };
        assert_eq!(verify_text(&clean), "checked 3 objects: the store is clean\n");
        let nothing = RepairReport { checked: 3, quarantined: vec![], left_alone: vec![] };
        assert_eq!(repair_text(&nothing, Path::new("q")), "checked 3 objects\nnothing to repair: the store is clean\n");
    }

    #[test]
    fn every_problem_kind_has_a_name_for_scripts() {
        let hash = omuzaki_store_api::ContentHash::of(b"x");
        assert_eq!(kind(&Problem::Corrupt(hash)), "corrupt");
        assert_eq!(kind(&Problem::Stray(PathBuf::from("x"))), "stray");
    }
}
