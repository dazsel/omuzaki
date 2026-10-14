//! The command line: what `omuzaki` accepts. Everything an op can do goes through `run`; the other commands read the
//! registry (`ops`, `help`) or the history (`undo`, `redo`), so adding an op never touches this file.

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::exit;

/// The `omuzaki` command line.
#[derive(Parser, Debug, PartialEq, Eq)]
#[command(
    name = "omuzaki",
    version,
    about = "Run any Omuzaki op from the shell: listing, help and arguments are generated from the op registry",
    disable_help_subcommand = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Print JSON instead of text: results on stdout, errors on stderr. Field order is stable.
    #[arg(long, global = true)]
    pub json: bool,

    /// The Omuzaki project directory to act on (the folder with `project.toml`). Needed by everything but `ops` and
    /// `help`. Changes are saved as they are applied.
    #[arg(long, global = true, value_name = "DIR")]
    pub project: Option<PathBuf>,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// A `omuzaki` subcommand.
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Command {
    /// List every op: name, kind, scope and summary.
    Ops,

    /// Show an op's arguments (names, types, ranges, descriptions), or this help when no op is named.
    Help {
        /// The op's name, e.g. `face.set_profile`.
        op: Option<String>,
    },

    /// Run an op, or preview it with --dry-run.
    Run {
        /// The op's name, e.g. `face.set_profile`; `omuzaki ops` lists them.
        op: String,

        /// The op's arguments as a JSON object; `@file.json` reads them from a file and `@-` from standard input
        /// (at most 16 MiB). Defaults to `{}`.
        #[arg(long, value_name = "JSON")]
        args: Option<String>,

        /// Validate and show what would change, without changing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Undo your most recent change in this session.
    ///
    /// The history lives in the running process and isn't saved between commands yet, so a separate `omuzaki undo`
    /// finds nothing to undo. Agents on `omuzaki mcp` have undo as the `history_undo` tool.
    Undo,

    /// Redo the change you last undid in this session.
    ///
    /// Like `undo`, it only sees the running process's history.
    Redo,

    /// Serve the ops to an AI agent over MCP on stdin/stdout.
    ///
    /// Reads JSON-RPC messages from stdin and writes only protocol messages to stdout, until the client closes stdin.
    /// The agent can read, edit and render, and its changes are saved to the project as it makes them. Point an
    /// MCP client at `omuzaki --project <DIR> mcp`.
    Mcp,

    /// Print every self-registration linked into this binary (ops, part kinds, formats, ...), one per line:
    /// package, kind, name and place, tab-separated. Hidden: it is for the link gate (`tests/registrations.rs`, ADR
    /// 0015), which compares it with the registrations the source declares.
    #[command(hide = true)]
    Registrations,
}

impl Command {
    /// True if the command reads or changes the project's document, so `--project` is required. Listing and
    /// documenting ops only needs the op registry, and listing registrations only the binary.
    pub fn needs_project(&self) -> bool {
        !matches!(self, Command::Ops | Command::Help { .. } | Command::Registrations)
    }
}

/// The clap command, with the exit-code table appended to `--help`.
pub fn command() -> clap::Command {
    Cli::command().after_help(exit::help_text())
}

/// Parses an argument list; `args[0]` is the program name.
///
/// # Errors
/// clap's usage error, including the `--help` and `--version` requests.
pub fn try_parse_from<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Cli::from_arg_matches(&command().try_get_matches_from(args)?)
}

/// Parses `std::env::args`. Prints help or a usage error and exits when the command line asks for it or is wrong.
pub fn parse() -> Cli {
    Cli::from_arg_matches(&command().get_matches()).unwrap_or_else(|e| e.exit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        try_parse_from(std::iter::once("omuzaki").chain(args.iter().copied())).expect("valid command line")
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(parse(&["ops"]).command, Command::Ops);
        assert_eq!(parse(&["help"]).command, Command::Help { op: None });
        assert_eq!(parse(&["help", "face.set_profile"]).command, Command::Help { op: Some("face.set_profile".into()) });
        assert_eq!(parse(&["undo"]).command, Command::Undo);
        assert_eq!(parse(&["redo"]).command, Command::Redo);
        assert_eq!(parse(&["mcp"]).command, Command::Mcp);
        assert_eq!(parse(&["registrations"]).command, Command::Registrations);
        assert_eq!(
            parse(&["run", "face.set_profile", "--args", "{}", "--dry-run"]).command,
            Command::Run { op: "face.set_profile".into(), args: Some("{}".into()), dry_run: true }
        );
        assert_eq!(
            parse(&["run", "face.set_profile"]).command,
            Command::Run { op: "face.set_profile".into(), args: None, dry_run: false }
        );
    }

    #[test]
    fn global_flags_work_before_or_after_the_command() {
        for args in [&["--json", "--project", "p", "ops"][..], &["ops", "--json", "--project", "p"]] {
            let cli = parse(args);
            assert!(cli.json);
            assert_eq!(cli.project, Some(PathBuf::from("p")));
        }
        assert!(!parse(&["ops"]).json);
    }

    #[test]
    fn only_listing_and_documenting_ops_and_registrations_work_without_a_project() {
        let needs = |args: &[&str]| parse(args).command.needs_project();
        assert!(!needs(&["ops"]) && !needs(&["help"]) && !needs(&["help", "face.set_profile"]));
        assert!(!needs(&["registrations"]));
        assert!(needs(&["run", "face.set_profile"]) && needs(&["undo"]) && needs(&["redo"]) && needs(&["mcp"]));
    }

    #[test]
    fn a_missing_op_name_or_unknown_command_is_a_usage_error() {
        for args in [&["run"][..], &["help", "a", "b"], &["frobnicate"], &[]] {
            let e = try_parse_from(std::iter::once("omuzaki").chain(args.iter().copied())).unwrap_err();
            assert_eq!(e.exit_code(), i32::from(exit::USAGE), "{args:?}");
        }
    }

    #[test]
    fn the_clap_definition_is_consistent() {
        command().debug_assert();
    }
}
