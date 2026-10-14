//! The whole program, so every binary built on this crate runs the same code: `omuzaki` itself, and the test fixture
//! that links demo ops in.

use std::io;
use std::process::ExitCode;

use crate::commands::execute;
use crate::output::finish;
use crate::session::{Session, open_runner};

/// Parses the command line, runs the command against the project and exits with the code for its result.
///
/// Exits the process itself (via clap) on `--help`, `--version` and usage errors.
pub fn run() -> ExitCode {
    let cli = crate::cli::parse();
    // Standard input is passed unlocked: `omuzaki mcp` reads the same stream through tokio, which takes the lock for each
    // read, so a lock held here would block the server forever.
    let result = open_runner(&cli).and_then(|runner| execute(&cli, runner, &Session::from_env(), &mut io::stdin()));
    ExitCode::from(finish(result, cli.json, &mut io::stdout().lock(), &mut io::stderr().lock()))
}
