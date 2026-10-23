//! The commands. Each is a pure function from a [`Runner`] to the text it prints, so they test without a terminal.
//!
//! Everything shown comes from [`Runner::ops`] and the op schemas; nothing here names an op.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

use omuzaki_ops_api::{Cause, CauseKind, Mode, OpError, OpKind, Request, Runner, Scope};
use serde::Serialize;

use crate::cli::{self, Cli, Command};
use crate::session::Session;
use crate::{args, json, mcp, render, store};

/// Runs the parsed command line against `runner` and returns what to print on stdout. `stdin` is read only for
/// `run --args @-`.
///
/// `mcp` is the one command that doesn't return quickly: it serves the runner on the process's own stdin and stdout
/// until the client disconnects, and then returns an empty string (the protocol is the output).
///
/// # Errors
/// Whatever the runner refuses with, or a bad `--args`, an unknown op for `help`, or an MCP server that can't run.
pub fn execute(cli: &Cli, runner: Arc<dyn Runner>, session: &Session, stdin: &mut dyn Read) -> Result<String, OpError> {
    match &cli.command {
        Command::New { dir, name } => crate::project::create(dir, name.as_deref(), cli.json),
        Command::Ops => ops(runner.as_ref(), cli.json),
        Command::Help { op: None } => Ok(format!("{}\n", cli::command().render_help().to_string().trim_end())),
        Command::Help { op: Some(name) } => help(runner.as_ref(), name, cli.json),
        Command::Run { op, args, dry_run } => {
            let mode = if *dry_run { Mode::DryRun } else { Mode::Apply };
            run(runner.as_ref(), session, op, args::parse(args.as_deref(), stdin)?, mode, cli.json)
        }
        Command::History { action } => {
            let (op, args, dry_run) = action.op();
            run(runner.as_ref(), session, op, args, if dry_run { Mode::DryRun } else { Mode::Apply }, cli.json)
        }
        Command::Undo => report_history(&UNDO, runner.undo(&session.actor, &session.grants)?, cli.json),
        Command::Redo => report_history(&REDO, runner.redo(&session.actor, &session.grants)?, cli.json),
        Command::Mcp(args) if args.branch.is_some() => {
            Err(OpError::invalid("branch", "branches belong to a project")
                .hint("pass the project folder: --project <DIR>"))
        }
        Command::Mcp(args) => mcp::serve_stdio(runner, session, args).map(|()| String::new()),
        // `app::run` runs it on the app core, which holds the branches; without a project there are none.
        Command::Branch { .. } => Err(OpError::invalid("project", "branches belong to a project")
            .hint("pass the project folder: --project <DIR>")),
        Command::Registrations => Ok(registrations()),
        // `app::run` calls `store::run` itself so the exit code survives; this arm keeps the match whole and serves
        // callers that only want the text.
        Command::Store { action } => store::run(action, cli.json).map(|reply| reply.text),
        // `app::run` runs it on the project's files; this arm keeps the match whole.
        Command::Credential { action } => {
            crate::credential::run(cli.project.as_deref(), action, session.actor.clone(), cli.json)
        }
    }
}

/// Every self-registration linked into this binary, one line each (`omuzaki_core::link::Registered`'s text form). The
/// link gate reads it back; it is the same in `--json` mode, since only that test reads it.
fn registrations() -> String {
    omuzaki_core::link::registered().iter().copied().map(registration_line).collect()
}

/// One registration's line, newline included. The name loses its whitespace, which the gate ignores when it compares
/// names: a macro whose first argument spans lines (a struct literal) would otherwise split its record over several
/// lines.
fn registration_line(r: &omuzaki_core::link::Registered) -> String {
    let name: String = r.name.split_whitespace().collect();
    format!("{}\t{}\t{}\t{}:{}\n", r.package, r.kind, name, r.file, r.line)
}

/// One row of `omuzaki ops --json`. The registry's [`OpInfo`](omuzaki_ops_api::OpInfo) also carries the schema; the
/// list leaves it to `omuzaki help <op> --json`.
#[derive(Serialize)]
struct OpRow<'a> {
    name: &'a str,
    kind: OpKind,
    scope: Scope,
    summary: &'a str,
}

fn ops(runner: &dyn Runner, json: bool) -> Result<String, OpError> {
    let ops = runner.ops();
    if json {
        let rows: Vec<OpRow<'_>> =
            ops.iter().map(|op| OpRow { name: op.name, kind: op.kind, scope: op.scope, summary: op.summary }).collect();
        return json::pretty(&rows);
    }
    Ok(render::ops_table(&ops))
}

fn help(runner: &dyn Runner, name: &str, json: bool) -> Result<String, OpError> {
    let info = runner.ops().into_iter().find(|op| op.name == name).ok_or_else(|| unknown_op(name))?;
    if json {
        return json::pretty(&info);
    }
    Ok(render::op_help(&info))
}

/// `help` is also the word people try for the CLI's own commands; point them at the right place.
fn unknown_op(name: &str) -> OpError {
    let error = OpError::not_found(format!("no op called {name}"));
    if cli::command().find_subcommand(name).is_some() {
        error.hint(format!("`{name}` is a omuzaki command, not an op; see `omuzaki {name} --help`"))
    } else {
        error.hint("list the ops with `omuzaki ops`")
    }
}

fn run(
    runner: &dyn Runner,
    session: &Session,
    op: &str,
    args: serde_json::Value,
    mode: Mode,
    json: bool,
) -> Result<String, OpError> {
    let request = Request {
        op: op.to_owned(),
        args,
        mode,
        actor: session.actor.clone(),
        grants: session.grants.clone(),
        cause: Some(Cause { kind: CauseKind::Direct, text: None }),
        tx: None,
        base: None,
    };
    let outcome = runner.run(request)?;
    if json { json::pretty(&outcome) } else { Ok(render::outcome(op, mode, &outcome)) }
}

/// The words `undo` and `redo` use to report their result.
struct HistoryWords {
    /// The JSON key holding the transaction label.
    key: &'static str,
    /// Text when a step was taken: `<verb> "<label>"`.
    verb: &'static str,
    /// Text when there was nothing to do.
    nothing: &'static str,
}

const UNDO: HistoryWords = HistoryWords { key: "undone", verb: "undid", nothing: "nothing to undo" };
const REDO: HistoryWords = HistoryWords { key: "redone", verb: "redid", nothing: "nothing to redo" };

/// The result of `undo` or `redo`: the transaction's label, or `None` when there was nothing to do.
fn report_history(words: &HistoryWords, label: Option<String>, json: bool) -> Result<String, OpError> {
    if json {
        return json::pretty(&BTreeMap::from([(words.key, label)]));
    }
    Ok(match label {
        Some(label) => format!("{} \"{label}\"\n", words.verb),
        None => format!("{}\n", words.nothing),
    })
}

#[cfg(test)]
mod tests {
    use omuzaki_core::link::Registered;
    use xtask::registry::Registration;

    use super::registration_line;

    #[test]
    fn a_registration_whose_name_spans_lines_prints_as_one_line_the_gate_reads() {
        let multi_line = Registered {
            package: "omuzaki-render",
            kind: "shading_program",
            name: "ProgramSpec\n{\n    name: \"hair\", params:\n    &[ParamSpec::colour(\"tint\")]\n}",
            file: "crates/omuzaki-render/src/program/hair.rs",
            line: 39,
        };
        let line = registration_line(&multi_line);
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");
        let parsed = Registration::parse(line.trim_end()).expect("the gate reads the line");
        assert_eq!(parsed.name, "ProgramSpec{name:\"hair\",params:&[ParamSpec::colour(\"tint\")]}");
        assert_eq!(
            (parsed.package.as_str(), parsed.kind.as_str(), parsed.line),
            ("omuzaki-render", "shading_program", 39)
        );
    }

    #[test]
    fn a_plain_name_prints_as_registered_displays_it() {
        let plain = Registered { package: "omuzaki-char", kind: "op", name: "Rename", file: "src/ops.rs", line: 3 };
        assert_eq!(registration_line(&plain), format!("{plain}\n"));
    }
}
