//! The whole program, so every binary built on this crate runs the same code: `omuzaki` itself, and the test fixture
//! that links demo ops in.

use std::io;
use std::process::ExitCode;

use crate::cli::Command;
use crate::commands::execute;
use crate::output::{Reply, finish_reply};
use crate::session::{Session, open_runner};
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
        _ => open_runner(&cli)
            .and_then(|runner| execute(&cli, runner, &Session::from_env(), &mut io::stdin()))
            .map(Reply::ok),
    };
    ExitCode::from(finish_reply(result, cli.json, &mut io::stdout().lock(), &mut io::stderr().lock()))
}
