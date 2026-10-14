//! `cargo xtask registry` and `cargo xtask lint-links`: the command-line side of the link gate (ADR 0015). The scan
//! itself is in the library (`xtask::registry`), which the Apps' `tests/registrations.rs` use too.

use std::process::ExitCode;

use clap::Args;
use xtask::registry::{Sources, lint_links};

use crate::error::Result;
use crate::workspace::Workspace;

/// Options of `cargo xtask registry`.
#[derive(Debug, Args)]
pub struct RegistryArgs {
    /// Only what a binary linking this crate must contain: its registrations and its dependencies'.
    #[arg(long, value_name = "CRATE")]
    app: Option<String>,
    /// With `--app`: also the registrations in this binary's own files.
    #[arg(long, value_name = "NAME", requires = "app")]
    bin: Option<String>,
}

/// The `registry` command: prints the expected registry, one registration per line (`package kind name file:line`).
pub fn registry(workspace: &Workspace, args: &RegistryArgs) -> Result<ExitCode> {
    let sources = Sources::scan(&workspace.root)?;
    let registrations = match &args.app {
        Some(app) => sources.expected(app, args.bin.as_deref())?,
        None => sources.registrations(),
    };
    for r in &registrations {
        println!("{r}");
    }
    eprintln!("registry: {} registration(s)", registrations.len());
    Ok(ExitCode::SUCCESS)
}

/// Prints every `link()` wiring violation and a summary; returns whether there were none.
pub fn report(workspace: &Workspace) -> Result<bool> {
    let sources = Sources::scan(&workspace.root)?;
    let found = lint_links(&sources);
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        println!("lint-links: ok ({} crates, {} registrations)", sources.crates().len(), sources.registrations().len());
    } else {
        println!(
            "lint-links: {} violation(s); the rules are in docs/decisions/0015-registration-linking.md",
            found.len()
        );
    }
    Ok(found.is_empty())
}

/// The `lint-links` command.
pub fn run(workspace: &Workspace) -> Result<ExitCode> {
    Ok(if report(workspace)? { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
