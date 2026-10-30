//! The whole program, so every binary built on this crate runs the same code: `omuzaki` itself, and the test fixture
//! that links demo ops in.

use std::io;
use std::process::ExitCode;

use omuzaki_ops_api::OpError;

use std::path::Path;

use omuzaki_app::{AppCore, AppError, RecoveryReport};
use omuzaki_doc::StoreError;

use crate::cli::{Cli, Command};
use crate::commands::execute;
use crate::output::{Reply, finish_reply};
use crate::session::{Session, core_options, open_executor_with, project_root};
use crate::store;

/// Parses the command line, runs the command against the project and exits with the code for its result.
///
/// Exits the process itself (via clap) on `--help`, `--version` and usage errors.
pub fn run() -> ExitCode {
    run_inner()
}

/// [`run`] with `adjust` applied to the app core's options of every project it opens: how a test fixture adds a
/// hook the product doesn't have (the crash tests' kill at a branch switch step). Call it once, as the whole program.
pub fn run_with(adjust: fn(omuzaki_app::AppOptions) -> omuzaki_app::AppOptions) -> ExitCode {
    // The first call wins; a second one in the same process (there is none) would keep the first hook.
    let _ = crate::session::ADJUST.set(adjust);
    run_inner()
}

fn run_inner() -> ExitCode {
    let cli = crate::cli::parse();
    let result = match &cli.command {
        // The store is a folder the person names, not the project, so it needs no runner. Called here rather than
        // through `execute`, which can only hand back text, so the report's exit code survives.
        Command::Store { action } => store::run(action, cli.json),
        // The credentials are the project's private files, not its document: no runner, so the studio may hold it.
        Command::Credential { action } => {
            let person = crate::session::Session::from_env().actor;
            crate::credential::run(cli.project.as_deref(), action, person, cli.json).map(Reply::ok)
        }
        // A new project has no runner to open yet.
        Command::New { dir, name } => crate::project::create(dir, name.as_deref(), cli.json).map(Reply::ok),
        // Standard input is passed unlocked: `omuzaki mcp` reads the same stream through tokio, which takes the lock for
        // each read, so a lock held here would block the server forever.
        _ => run_with_runner(&cli),
    };
    ExitCode::from(finish_reply(result, cli.json, &mut io::stdout().lock(), &mut io::stderr().lock()))
}

/// Opens the runner for `cli` with the invocation's agent connections wired in, and runs the command on it.
///
/// A command on a project opens it through the app core ([`omuzaki_app::AppCore`]): journalled, recovered if the
/// last session crashed, with its undo and redo history restored, and closed cleanly before the process exits, so
/// `omuzaki undo` in a new process undoes what the last one did.
fn run_with_runner(cli: &Cli) -> Result<Reply, OpError> {
    let mut session = Session::from_env();
    if let (Some(dir), true) = (cli.project.as_deref(), cli.command.needs_project()) {
        return run_on_core(cli, dir, session);
    }
    let executor = open_executor_with(cli, &session.connections)?;
    session.sessions = Some(executor.clone());
    session.jobs = Some(executor.clone());
    session.project = cli.project.as_deref().map(project_root).transpose()?;
    execute(cli, executor, &session, &mut io::stdin()).map(Reply::ok)
}

/// Relays stdio into the studio that holds `dir`, keeping the client's credential in the user's local data folder.
fn bridge_to_studio(dir: &Path) -> Result<Reply, OpError> {
    crate::mcp::log_to_stderr();
    let paths = omuzaki_settings::Paths::from_env()
        .map_err(|e| OpError::unsupported(e.to_string()).hint("set OMUZAKI_DATA_DIR and OMUZAKI_CONFIG_DIR"))?;
    omuzaki_app::bridge::serve_stdio(dir, paths.data()).map_err(|e| e.to_op_error())?;
    Ok(Reply::ok(String::new()))
}

/// Runs `cli`'s command on the project in `dir`, opened through the app core, and closes it.
fn run_on_core(cli: &Cli, dir: &Path, mut session: Session) -> Result<Reply, OpError> {
    let core = match AppCore::open(dir, core_options(cli, session.actor.clone())?) {
        Ok(core) => core,
        // The studio has the project: an agent's `omuzaki mcp` works in it through the bridge (agent-sessions 10.2).
        Err(AppError::Store(StoreError::Locked { .. })) if matches!(cli.command, Command::Mcp(_)) => {
            if let Command::Mcp(args) = &cli.command
                && let Some(branch) = &args.branch
            {
                return Err(OpError::invalid("branch", "the studio has this project open, so it decides the lanes")
                    .hint(format!("connect without --branch, then call branch_work {{\"branch\": \"{branch}\"}}")));
            }
            return bridge_to_studio(dir);
        }
        Err(error) => return Err(error.to_op_error()),
    };
    if !core.recovery().is_empty() {
        // Headless recovery restores to the last committed change and says so (journal.md 8.4); stdout is the reply.
        eprintln!("{}", recovery_notice(core.recovery(), cli.json));
    }
    session.sessions = Some(core.sessions());
    session.jobs = Some(core.jobs());
    session.project = Some(project_root(dir)?);
    let result = match &cli.command {
        Command::Mcp(args) => {
            crate::mcp::serve_core(&core, args.branch.as_deref(), &session).map(|()| Reply::ok(String::new()))
        }
        Command::Branch { action } => crate::branch::run(&core, action, &session, cli.json),
        // A render that failed for want of a GPU says so, as `unavailable`, rather than as the renderer's error.
        _ => execute(cli, core.runner(), &session, &mut io::stdin()).map(Reply::ok).map_err(crate::gpu::explain),
    };
    let closed = core.close().map_err(|e| e.to_op_error());
    let reply = result?;
    closed?;
    Ok(reply)
}

/// What recovery did, for stderr: one line, or with `--json` a `{"recovery": {...}}` object.
fn recovery_notice(report: &RecoveryReport, json: bool) -> String {
    if !json {
        return format!("omuzaki: {}", report.summary());
    }
    let body = serde_json::json!({ "recovery": {
        "summary": report.summary(),
        "unclean": report.unclean,
        "from_disk": report.from_disk,
        "replayed": report.replayed,
        "rolled_back": report.rolled_back,
        "kept": report.kept,
        "interrupted": report.interrupted,
        "stopped": report.stopped,
        "untrusted": report.untrusted,
        "archived": report.archived.as_ref().map(ToString::to_string),
    }});
    crate::json::pretty(&body).map_or_else(|_| body.to_string(), |text| text.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recovery_notice_is_a_line_or_a_json_object() {
        let mut report = RecoveryReport::default();
        report.unclean = true;
        report.replayed = 3;
        report.rolled_back = vec!["design".to_owned()];
        assert!(recovery_notice(&report, false).starts_with("omuzaki: Omuzaki closed unexpectedly; 3 saved changes"));
        let json: serde_json::Value = serde_json::from_str(&recovery_notice(&report, true)).unwrap();
        assert_eq!(json["recovery"]["replayed"], 3);
        assert_eq!(json["recovery"]["rolled_back"][0], "design");
        assert_eq!(json["recovery"]["from_disk"], false);
        assert!(json["recovery"]["archived"].is_null());
    }
}
