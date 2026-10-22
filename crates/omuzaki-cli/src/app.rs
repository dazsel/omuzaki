//! The whole program, so every binary built on this crate runs the same code: `omuzaki` itself, and the test fixture
//! that links demo ops in.

use std::io;
use std::process::ExitCode;

use omuzaki_ops_api::OpError;

use std::path::Path;

use omuzaki_app::{AppCore, AppError, AppOptions};
use omuzaki_doc::StoreError;

use crate::cli::{Cli, Command};
use crate::commands::execute;
use crate::output::{Reply, finish_reply};
use crate::session::{Session, open_executor_with, project_root};
use crate::store;

/// Parses the command line, runs the command against the project and exits with the code for its result.
///
/// Exits the process itself (via clap) on `--help`, `--version` and usage errors.
pub fn run() -> ExitCode {
    let cli = crate::cli::parse();
    let result = match &cli.command {
        // The store is a folder the person names, not the project, so it needs no runner. Called here rather than
        // through `execute`, which can only hand back text, so the report's exit code survives.
        Command::Store { action } => store::run(action, cli.json),
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

/// Runs `cli`'s command on the project in `dir`, opened through the app core, and closes it.
/// Relays stdio into the studio that holds `dir`, keeping the client's credential in the user's local data folder.
fn bridge_to_studio(dir: &Path) -> Result<Reply, OpError> {
    crate::mcp::log_to_stderr();
    let paths = omuzaki_settings::Paths::from_env()
        .map_err(|e| OpError::unsupported(e.to_string()).hint("set OMUZAKI_DATA_DIR and OMUZAKI_CONFIG_DIR"))?;
    omuzaki_app::bridge::serve_stdio(dir, paths.data()).map_err(|e| e.to_op_error())?;
    Ok(Reply::ok(String::new()))
}

fn run_on_core(cli: &Cli, dir: &Path, mut session: Session) -> Result<Reply, OpError> {
    let core = match AppCore::open(dir, AppOptions::headless(session.actor.clone())) {
        Ok(core) => core,
        // The studio has the project: an agent's `omuzaki mcp` works in it through the bridge (agent-sessions 10.2).
        Err(AppError::Store(StoreError::Locked { .. })) if matches!(cli.command, Command::Mcp) => {
            return bridge_to_studio(dir);
        }
        Err(error) => return Err(error.to_op_error()),
    };
    if !core.recovery().is_empty() {
        // Headless recovery restores to the last committed change and says so (journal.md 8.4); stdout is the reply.
        eprintln!("omuzaki: {}", core.recovery().summary());
    }
    session.sessions = Some(core.sessions());
    session.jobs = Some(core.jobs());
    session.project = Some(project_root(dir)?);
    let result = match &cli.command {
        Command::Mcp => crate::mcp::serve_core(&core).map(|()| String::new()),
        _ => execute(cli, core.runner(), &session, &mut io::stdin()),
    };
    let closed = core.close().map_err(|e| e.to_op_error());
    let text = result?;
    closed?;
    Ok(Reply::ok(text))
}
