//! `cargo xtask <command>`: repo automation. Each command lives in its own module (§39 rule 2 applies here too).
//!
//! - `lint-deps`: enforce the crate layering table (`crates/README.md`).
//! - `lint-links`: enforce the `link()` wiring that keeps self-registrations in binaries (ADR 0015).
//! - `lint-ui-writes`: UI code changes the document only through the `Runner` (QG-11).
//! - `models`: every shipped model's manifest is backed by an allowed licence-register row (ML-01, ADR 0017).
//! - `lint-seed`: a published seed version keeps its content, and no seed item names private material (library D15, D16).
//! - `registry`: print the self-registrations the source declares, the expected side of the link gate.
//! - `dist`: pack the Windows zip (`omuzaki.exe`, models, notices).
//! - `check`: run the CI gates on the crates a change affects (`--json` prints the scope for the CI plan job).

mod affected;
mod check;
mod error;
mod links;
mod lint_deps;
mod lint_seed;
mod lint_ui_writes;
mod models;
mod process;
mod report;
mod workspace;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::error::Result;
use crate::workspace::Workspace;

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Repo automation for the Omuzaki workspace")]
struct Cli {
    /// The workspace's Cargo.toml (default: the workspace cargo finds from the current directory).
    #[arg(long, global = true, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Fail on any workspace dependency the layering table forbids (impl -> impl, API -> mock, ...).
    LintDeps,
    /// Fail on a crate that registers without a `link()`, or depends on one without calling it (ADR 0015).
    LintLinks,
    /// Fail on UI code that changes the document outside the `Runner` (QG-11).
    LintUiWrites,
    /// Fail on a shipped model whose manifest isn't backed by an allowed row in `docs/licensing/register.md` (ML-01, ADR 0017).
    Models,
    /// Fail on a published seed version whose content changed without a bump, or a seed item that names private material.
    LintSeed(lint_seed::LintSeedArgs),
    /// Print every self-registration in the source (or, with `--app`, those a binary must contain), one per line.
    Registry(links::RegistryArgs),
    /// Run fmt, clippy, tests, cargo-deny, lint-deps, lint-links and lint-ui-writes on the crates a change affects, through the build
    /// queue.
    Check(check::CheckArgs),
    /// Pack the Windows zip: omuzaki.exe, the models, the notices (and the ONNX Runtime DLL), deterministically.
    Dist(xtask::dist::DistArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode> {
    let workspace = Workspace::load(cli.manifest_path.as_deref())?;
    if let Command::Dist(args) = &cli.command {
        return Ok(match xtask::dist::run(&workspace.root, args) {
            Ok(zip) => {
                println!("{}", zip.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("xtask: dist: {error}");
                ExitCode::from(2)
            }
        });
    }
    match &cli.command {
        Command::LintDeps => Ok(lint_deps::run(&workspace)),
        Command::LintLinks => links::run(&workspace),
        Command::LintUiWrites => lint_ui_writes::run(&workspace),
        Command::Models => Ok(models::run(&workspace)),
        Command::LintSeed(args) => lint_seed::run(&workspace, args),
        Command::Registry(args) => links::registry(&workspace, args),
        Command::Check(args) => check::run(&workspace, args),
        Command::Dist(_) => unreachable!("handled above"),
    }
}
