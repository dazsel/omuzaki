//! The command line: what `omuzaki` accepts. Everything an op can do goes through `run`; the other commands read the
//! registry (`ops`, `help`) or the history (`undo`, `redo`), so adding an op never touches this file.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};

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
    /// `help`. Each change is journalled before the command answers, and the files are written before it exits.
    #[arg(long, global = true, value_name = "DIR")]
    pub project: Option<PathBuf>,

    /// If Omuzaki stopped without closing the project (a crash, a kill, a power cut), how to open it: `restore`
    /// brings back every change that finished, from the journal; `disk` takes the files as they are. Either way the
    /// folder as it was found is kept in a snapshot for 30 days, and the notice on stderr says which.
    #[arg(long, global = true, value_enum, default_value_t = RecoverFrom::Restore, value_name = "FROM")]
    pub recover: RecoverFrom,

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

    /// Undo your most recent change to the project.
    ///
    /// The history is kept with the project, so this undoes what an earlier `omuzaki run` did, and the files follow
    /// before the command exits. Agents on `omuzaki mcp` have undo as the `history_undo` tool.
    Undo,

    /// Redo the change you last undid.
    ///
    /// Like `undo`, it works across commands: the history is kept with the project.
    Redo,

    /// Serve the ops to an AI agent over MCP on stdin/stdout.
    ///
    /// Reads JSON-RPC messages from stdin and writes only protocol messages to stdout, until the client closes stdin.
    /// The agent can read, edit, render and review, and its changes are saved to the project as it makes them. Point an
    /// MCP client at `omuzaki --project <DIR> mcp`.
    Mcp,

    /// Print every self-registration linked into this binary (ops, part kinds, formats, ...), one per line:
    /// package, kind, name and place, tab-separated. Hidden: it is for the link gate (`tests/registrations.rs`, ADR
    /// 0015), which compares it with the registrations the source declares.
    #[command(hide = true)]
    Registrations,

    /// Check or repair the asset store on disk. The store is a folder you name with `--store`, not the project.
    Store {
        /// What to do with the store.
        #[command(subcommand)]
        action: StoreCommand,
    },
}

/// How `--recover` opens a project that wasn't closed.
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecoverFrom {
    /// Bring back every change that finished, from the journal; unfinished transactions are rolled back.
    #[default]
    Restore,
    /// Take the files as they are; the journal's changes since the last save stay only in the kept snapshot.
    Disk,
}

impl RecoverFrom {
    /// The app core's recovery mode.
    pub fn mode(self) -> omuzaki_app::Recover {
        match self {
            Self::Restore => omuzaki_app::Recover::Restore,
            Self::Disk => omuzaki_app::Recover::Disk,
        }
    }
}

/// A `omuzaki store` subcommand.
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum StoreCommand {
    /// Check every object in the store and report each problem. Changes nothing; exits 12 if anything is wrong.
    Verify(StoreArgs),

    /// Move what the store can't trust into its quarantine folder; nothing is deleted. Exits 12 if a problem needs a
    /// person (listed as left alone), and 0 otherwise.
    Repair(StoreArgs),
}

/// The arguments both `store` subcommands take.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct StoreArgs {
    /// The store's folder: the one with an `objects` folder in it.
    #[arg(long, value_name = "DIR")]
    pub store: PathBuf,

    /// How long a `.tmp-*` staging folder must sit untouched before it counts as a crashed write: a whole number with
    /// `s`, `m` or `h`, as in `90s`, `15m` or `2h`. Defaults to one hour.
    #[arg(long, value_name = "DURATION", value_parser = parse_stale_after)]
    pub stale_after: Option<Duration>,
}

impl Command {
    /// True if the command reads or changes the project's document, so `--project` is required. Listing and
    /// documenting ops only needs the op registry, listing registrations only the binary, and the store commands
    /// name their own folder.
    pub fn needs_project(&self) -> bool {
        !matches!(self, Command::Ops | Command::Help { .. } | Command::Registrations | Command::Store { .. })
    }
}

/// Reads `--stale-after`: a whole number with `s`, `m` or `h`. A bare number is refused so `90` can't mean ninety
/// seconds in one place and ninety hours in another, and zero is refused because every staging folder would then
/// count as crashed, live writers included.
fn parse_stale_after(text: &str) -> Result<Duration, String> {
    let seconds_per_unit = match text.chars().last() {
        Some('s') => 1,
        Some('m') => 60,
        Some('h') => 3600,
        _ => return Err(format!("`{text}` needs a unit: s, m or h, as in 90s, 15m or 2h")),
    };
    // The unit is one ASCII byte, so cutting it off leaves a valid string.
    let digits = &text[..text.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("`{text}` is not a whole number followed by s, m or h"));
    }
    let too_large = || format!("`{text}` is too large");
    let count: u64 = digits.parse().map_err(|_| too_large())?;
    let seconds = count.checked_mul(seconds_per_unit).ok_or_else(too_large)?;
    if seconds == 0 {
        return Err(format!("`{text}` must be more than zero"));
    }
    Ok(Duration::from_secs(seconds))
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
    fn recover_defaults_to_restore_and_takes_disk() {
        assert_eq!(parse(&["undo"]).recover, RecoverFrom::Restore);
        assert_eq!(parse(&["undo", "--recover", "disk"]).recover, RecoverFrom::Disk);
        assert_eq!(parse(&["--recover", "restore", "mcp"]).recover.mode(), omuzaki_app::Recover::Restore);
        assert!(try_parse_from(["omuzaki", "--recover", "files", "undo"]).is_err());
    }

    #[test]
    fn only_listing_and_documenting_ops_and_registrations_work_without_a_project() {
        let needs = |args: &[&str]| parse(args).command.needs_project();
        assert!(!needs(&["ops"]) && !needs(&["help"]) && !needs(&["help", "face.set_profile"]));
        assert!(!needs(&["registrations"]));
        assert!(!needs(&["store", "verify", "--store", "s"]) && !needs(&["store", "repair", "--store", "s"]));
        assert!(needs(&["run", "face.set_profile"]) && needs(&["undo"]) && needs(&["redo"]) && needs(&["mcp"]));
    }

    #[test]
    fn store_commands_parse_with_their_folder_and_an_optional_threshold() {
        assert_eq!(
            parse(&["store", "verify", "--store", "s"]).command,
            Command::Store { action: StoreCommand::Verify(StoreArgs { store: PathBuf::from("s"), stale_after: None }) }
        );
        assert_eq!(
            parse(&["store", "repair", "--store", "s", "--stale-after", "2h"]).command,
            Command::Store {
                action: StoreCommand::Repair(StoreArgs {
                    store: PathBuf::from("s"),
                    stale_after: Some(Duration::from_secs(7200)),
                })
            }
        );
    }

    #[test]
    fn stale_after_takes_seconds_minutes_or_hours_and_refuses_the_rest() {
        assert_eq!(parse_stale_after("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_stale_after("15m"), Ok(Duration::from_mins(15)));
        assert_eq!(parse_stale_after("2h"), Ok(Duration::from_secs(7200)));
        for bad in ["90", "", "s", "0s", "-5m", "+5m", "1.5h", "5d", "h5", "99999999999999999999h"] {
            assert!(parse_stale_after(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_bad_stale_after_or_a_missing_store_folder_is_a_usage_error() {
        for args in [&["store", "verify"][..], &["store", "verify", "--store", "s", "--stale-after", "90"], &["store"]]
        {
            let e = try_parse_from(std::iter::once("omuzaki").chain(args.iter().copied())).unwrap_err();
            assert_eq!(e.exit_code(), i32::from(exit::USAGE), "{args:?}");
        }
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
