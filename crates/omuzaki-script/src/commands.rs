//! The two commands behind `omuzaki script run <file>` and `omuzaki script check <file>`, as pure functions from a
//! runner and the file's text to the text they print, like the other commands of the CLI. The CLI parses its arguments,
//! reads the file and wires these in; it doesn't know the engine (`docs/design/scripting.md` section 6).

use std::sync::Arc;

use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, ErrorDetail, Grants, OpError, Runner};
use serde::Serialize;

use crate::{CheckReport, Limits, OutputKind, RunRequest, RunnerCalls, ScriptHost, ScriptOutcome};

/// How `omuzaki script run` runs a file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunOptions {
    /// What the script reads as `params` (`--params @file.json`).
    pub params: serde_json::Map<String, Value>,
    /// Plan every call instead of applying it (`--dry-run`).
    pub dry_run: bool,
    /// Print the outcome as JSON (`--json`) instead of text.
    pub json: bool,
}

/// Runs the script `source`, read from the file called `name`, as `actor` within `grants`, under the console limits:
/// one transaction on `runner`, cause `Script` named after the file. Returns what to print: the script's output, one
/// line each (a `log` line starts with `note: `), then its result if it has one; for a dry run, one line per planned
/// call. With [`RunOptions::json`] it is the [`ScriptOutcome`] as pretty JSON.
///
/// The caller syncs before it exits, as every command that changed the project does.
///
/// # Errors
/// The `OpError` the run ended with, which decides the exit code: an inner call's own error, or `invalid` (3) for a
/// script that doesn't parse, breaks a limit or fails itself, with the line and column at the end of the message.
pub fn run_script(
    runner: Arc<dyn Runner>,
    name: &str,
    source: &str,
    actor: &Actor,
    grants: &Grants,
    options: &RunOptions,
) -> Result<String, OpError> {
    let host = ScriptHost::new(&runner.ops(), Limits::console());
    let calls = RunnerCalls::new(runner, name, actor.clone(), grants.clone());
    let request = RunRequest {
        name: name.to_owned(),
        source: source.to_owned(),
        actor: actor.clone(),
        grants: grants.clone(),
        tx: None,
        params: options.params.clone(),
        dry_run: options.dry_run,
    };
    // A command line never cancels a run: Ctrl+C ends the process.
    let outcome = host.run(request, &calls, &crate::NeverCancel).map_err(|error| error.to_op_error())?;
    if options.json { pretty(&outcome) } else { Ok(outcome_text(&outcome)) }
}

/// What `omuzaki script check` prints and how it exits.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckOutput {
    /// What to print: the ops the script names with line and column, then every problem as `line:column: message`, then
    /// the scopes it needs; with `json`, the [`CheckReport`] as pretty JSON. It is the report whether the script is
    /// clean or not, so a failing check still shows everything it found.
    pub text: String,
    /// The error that decides the exit code: `invalid` (3) when the report has a problem, so a script that doesn't check
    /// exits non-zero (the message is the first problem and its position), or `internal` if the JSON could not be
    /// written. `None` for a clean script.
    pub failure: Option<OpError>,
}

/// Checks the script `source` without running it. The result carries both the report to print and, when the script has
/// a problem, the failing status: the CLI prints [`CheckOutput::text`] either way, then exits with the code of
/// [`CheckOutput::failure`] if there is one.
pub fn check_script(runner: &dyn Runner, source: &str, json: bool) -> CheckOutput {
    let report = ScriptHost::new(&runner.ops(), Limits::console()).check(source);
    let (text, mut failure) = match json.then(|| pretty(&report)) {
        Some(Ok(text)) => (text, None),
        Some(Err(error)) => (error.message.clone() + "\n", Some(error)),
        None => (report_text(&report), None),
    };
    if failure.is_none() {
        failure = report.first_problem().map(|problem| {
            OpError::invalid(
                "source",
                format!("{} (line {}, column {})", problem.message, problem.line, problem.column),
            )
            .with_detail(ErrorDetail::Position { line: problem.line, column: problem.column })
            .hint(format!("{} problem(s) in all; run with --json for the report", report.problems().len()))
        });
    }
    CheckOutput { text, failure }
}

fn pretty<T: Serialize>(value: &T) -> Result<String, OpError> {
    serde_json::to_string_pretty(value)
        .map(|text| text + "\n")
        .map_err(|e| OpError::internal(format!("serialising the result failed: {e}")))
}

/// Lines joined with a line break after each.
fn lines(lines: Vec<String>) -> String {
    lines.into_iter().map(|line| line + "\n").collect()
}

fn outcome_text(outcome: &ScriptOutcome) -> String {
    let mut out: Vec<String> = outcome
        .output
        .iter()
        .map(|line| match line.kind {
            OutputKind::Print => line.text.clone(),
            OutputKind::Note => format!("note: {}", line.text),
        })
        .collect();
    out.extend(
        outcome.plans.iter().map(|plan| {
            format!("would run {} with {}: {} change(s)", plan.op, plan.args, plan.plan.patch.changes.len())
        }),
    );
    if !outcome.plans.is_empty() {
        out.push(format!("dry run: {} call(s) planned, nothing changed", outcome.plans.len()));
    }
    if outcome.value != Value::Null {
        out.push(outcome.value.to_string());
    }
    lines(out)
}

fn report_text(report: &CheckReport) -> String {
    let mut out: Vec<String> =
        report.ops.iter().map(|call| format!("{}:{}: calls {}", call.line, call.column, call.name)).collect();
    out.extend(report.problems().iter().map(|p| format!("{}:{}: {}", p.line, p.column, p.message)));
    let scopes: Vec<String> = report.scopes.iter().map(ToString::to_string).collect();
    out.push(format!("needs: {}", if scopes.is_empty() { "read".to_owned() } else { scopes.join(", ") }));
    if let Some(ceiling) = &report.ceiling {
        let ceiling: Vec<String> = ceiling.iter().map(ToString::to_string).collect();
        out.push(format!("ceiling: {}", ceiling.join(", ")));
    }
    lines(out)
}
