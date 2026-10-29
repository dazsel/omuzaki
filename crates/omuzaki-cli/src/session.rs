//! Who is running the command, and which runner executes it.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_mcp::Connections;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, ConnectionService, Grants, Jobs, OpError, ProjectDir, ProjectDirError, Runner, ServicesBuilder,
    Sessions,
};
use omuzaki_settings_api::service::Settings;

use omuzaki_app::AppOptions;

use crate::cli::{Cli, Command, McpArgs};
use crate::project::{NoProject, ProjectRunner, open_in_memory};

/// The actor and grants every request of one `omuzaki` invocation carries.
#[derive(Clone)]
pub struct Session {
    /// Who is acting; logged with every applied op.
    pub actor: Actor,
    /// What they may do.
    pub grants: Grants,
    /// The agent sessions of this invocation: `omuzaki mcp` counts its clients here, and `agent.connection` reads it.
    pub connections: Connections,
    /// The executor's session registry, once the runner is open: `omuzaki mcp` opens one session per client in it.
    pub sessions: Option<Arc<dyn Sessions>>,
    /// The executor's job manager, once the runner is open: `omuzaki mcp` serves long ops to clients that ask for tasks.
    pub jobs: Option<Arc<dyn Jobs>>,
    /// The `--project` folder: the one place besides the document that `omuzaki mcp` lets an agent's files reach
    /// (ADR 0013).
    pub project: Option<String>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("actor", &self.actor)
            .field("grants", &self.grants)
            .field("connections", &self.connections)
            .field("sessions", &self.sessions.is_some())
            .field("jobs", &self.jobs.is_some())
            .field("project", &self.project)
            .finish()
    }
}

impl Session {
    /// A person at the command line holds every scope (spec §6 defaults: people get everything).
    pub fn person(name: impl Into<String>) -> Self {
        Self {
            actor: Actor::Person { name: name.into() },
            grants: Grants::person(),
            connections: Connections::default(),
            sessions: None,
            jobs: None,
            project: None,
        }
    }

    /// The person named by the OS environment (`USER`, or `USERNAME` on Windows), or `cli` when neither is set.
    pub fn from_env() -> Self {
        Self::person(std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "cli".to_owned()))
    }
}

/// Opens the runner that executes `cli`'s command: with `--project`, the project through the app core
/// ([`ProjectRunner`]), journalled and recovered as `omuzaki run` opens it, with the files written before each change
/// returns and the project closed when the last handle drops; without one, an executor over an empty document.
///
/// The executor runs every op linked into the binary, and with `--project` it knows the project folder, so ops can
/// read project-relative files (and nothing outside it without an `fs` root). Commands that only read the op registry
/// (`ops`, `help`) get an empty document when no project is given; the others refuse to start without one.
///
/// # Errors
/// - `Invalid` (field `project`) if `--project` is a file or a filesystem root, if the command needs a project and
///   `--project` is missing, or the project's files are broken.
/// - `NotFound` if `--project` names no project; `Unsupported` if it was written by a newer Omuzaki; `Conflict` if
///   another Omuzaki has it open.
/// - `Internal` if two linked ops share a name (a bug in whichever crate registered them).
pub fn open_runner(cli: &Cli) -> Result<Arc<dyn Runner>, OpError> {
    open_runner_with(cli, &Connections::default())
}

/// As [`open_runner`], with `connections` wired into the executor's services so `agent.connection` can read it. Give
/// the same handle to the MCP server (`omuzaki_mcp::McpOptions::with_connections`). A project opened through the app
/// core counts its agents in the core's own handle instead.
///
/// # Errors
/// As [`open_runner`].
pub fn open_runner_with(cli: &Cli, connections: &Connections) -> Result<Arc<dyn Runner>, OpError> {
    match cli.project.as_deref() {
        Some(dir) => Ok(Arc::new(open_project(cli, dir, Session::from_env().actor)?)),
        None => Ok(open_executor_with(cli, connections)?),
    }
}

/// Opens the project in `dir` through the app core, with `cli`'s `--recover`, for `actor`: the runner
/// [`open_runner`] returns, keeping its type so the core can be reached.
///
/// # Errors
/// As [`open_runner`].
pub fn open_project(cli: &Cli, dir: &Path, actor: Actor) -> Result<ProjectRunner, OpError> {
    // The folder is vetted first: `ProjectDir` names the reason in its own words, which beat the document's.
    open_project_dir(dir)?;
    ProjectRunner::open(dir, core_options(cli, actor)?).map_err(|e| e.to_op_error())
}

/// What [`run_with`](crate::app::run_with) applies to every core's options.
pub(crate) static ADJUST: std::sync::OnceLock<fn(AppOptions) -> AppOptions> = std::sync::OnceLock::new();

/// Where a headless agent's credential secret comes from when `--token-file` doesn't say.
pub const TOKEN_VAR: &str = "OMUZAKI_MCP_TOKEN";

/// What a headless `omuzaki mcp` agent may do and spend (design `agent-sessions.md` 7.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ceiling {
    /// Its grants.
    pub grants: Grants,
    /// The budget its session starts with.
    pub budget: Budget,
    /// The label of the credential it runs from, if it runs from one.
    pub credential: Option<String>,
}

/// The ceiling of `args`' agent on `project`: the credential the secret in `--token-file` or [`TOKEN_VAR`] presents
/// (its grants and budget, set when the person issued it), or else the default grants plus `--grant` and
/// `--spend-cap`. `token` is the environment variable's value.
///
/// # Errors
/// `invalid` for a secret with no project, or with `--grant` or `--spend-cap` beside it (the credential decides);
/// `denied` for a secret no credential of the project matches (wrong, or revoked); `invalid` when `--token-file`
/// can't be read.
#[allow(clippy::disallowed_methods, reason = "the CLI reads the `--token-file` the person typed; it is not an op")]
pub fn ceiling(args: &McpArgs, project: Option<&Path>, token: Option<String>) -> Result<Ceiling, OpError> {
    let secret = match &args.token_file {
        Some(file) => Some(std::fs::read_to_string(file).map_err(|e| {
            OpError::invalid("token-file", format!("the credential file {} can't be read: {e}", file.display()))
        })?),
        None => token,
    };
    let Some(secret) = secret.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) else {
        return Ok(Ceiling { grants: args.grants(), budget: args.budget(), credential: None });
    };
    if !args.grant.is_empty() || args.spend_cap.is_some() {
        return Err(OpError::invalid("grant", "a credential carries its own grants and budget")
            .hint("drop --grant and --spend-cap, or issue a credential with what the agent needs"));
    }
    let Some(project) = project else {
        return Err(OpError::invalid("project", "a credential belongs to a project").hint("pass --project <DIR>"));
    };
    let record = omuzaki_app::credentials::find(project, &secret)?.ok_or_else(|| {
        OpError::denied("this credential isn't one the project issued, or it was revoked")
            .hint("issue one with `omuzaki --project <DIR> credential issue --label <NAME>`")
    })?;
    Ok(Ceiling { grants: record.grants, budget: record.budget.unwrap_or_default(), credential: Some(record.label) })
}

/// The app core's options for a command-line host: headless (nobody can be asked to approve), recovering as
/// `--recover` says, and for `mcp` with its [`Ceiling`] and `--tools`.
///
/// # Errors
/// As [`ceiling`], for `mcp`.
pub fn core_options(cli: &Cli, actor: Actor) -> Result<AppOptions, OpError> {
    let mut options = AppOptions::headless(actor).with_recover(cli.recover.mode());
    if let Some(adjust) = ADJUST.get() {
        options = adjust(options);
    }
    // The asset store and the settings are the person's, the ones the studio uses: an import here is there in the
    // studio, and `settings.set` changes the same file. Downloads (a starter character) go through the verified cache
    // the studio's models use.
    if let Ok(paths) = omuzaki_settings::Paths::from_env() {
        let settings: Arc<dyn Settings> = Arc::new(omuzaki_settings::Store::open(&paths));
        let cache = omuzaki_ai::VerifiedCache::new(paths.data().join(omuzaki_ai::CACHE_DIR), omuzaki_http::HttpFetcher::new());
        options = options.with_data_dir(paths.data()).with_services(
            ServicesBuilder::new()
                .insert::<dyn Settings>(settings)
                .insert::<dyn omuzaki_ai_api::FileSource>(Arc::new(cache))
                .build(),
        );
    }
    Ok(match &cli.command {
        // The person running `omuzaki mcp --grant ...`, or who issued its credential, says yes in advance, since nobody
        // is at a window to be asked.
        Command::Mcp(args) => {
            let ceiling = ceiling(args, cli.project.as_deref(), std::env::var(TOKEN_VAR).ok())?;
            if let Some(label) = &ceiling.credential {
                eprintln!("omuzaki: the agent runs from the credential \"{label}\"");
            }
            options
                .with_agent_grants(ceiling.grants)
                .with_agent_budget(ceiling.budget)
                .with_mcp_tools(args.tools.mode())
        }
        _ => options,
    })
}

/// An executor over `--project` as it is on disk, in memory (or over an empty document without one), keeping the
/// executor's type so its session registry can be reached too. Nothing it applies is written: a host that saves opens
/// the project with [`open_runner`] or [`open_project`].
///
/// The command line is headless: no window is open to ask a person, so a request of an agent session that would wait
/// for an approval is refused at once, with a hint that says how to grant what it needs in advance (design
/// `agent-sessions.md` §7.6). A host with a window opens the executor with [`open_executor_unshared`] instead.
///
/// # Errors
/// As [`open_runner`].
pub fn open_executor_with(cli: &Cli, connections: &Connections) -> Result<Arc<Executor>, OpError> {
    Ok(Arc::new(open_executor_unshared(cli, connections)?.with_approver(false)))
}

/// The executor of [`open_executor_with`] before it is shared and before it is told whether a person can be asked: it
/// has an approver, as the studio's window gives it. A host sets what only it knows (`Executor::with_approver`) and
/// then shares it.
///
/// # Errors
/// As [`open_runner`].
pub fn open_executor_unshared(cli: &Cli, connections: &Connections) -> Result<Executor, OpError> {
    // The folder is vetted first: `ProjectDir` names the reason in its own words, which beat the document's.
    let project_dir = cli.project.as_deref().map(open_project_dir).transpose()?;
    let doc: Box<dyn DocWrite> = match cli.project.as_deref() {
        Some(dir) => Box::new(open_in_memory(dir)?),
        None if cli.command.needs_project() => return Err(project_required()),
        None => Box::new(NoProject),
    };
    let executor = Executor::new(doc, Arc::new(SystemClock::new())).map_err(|e| {
        OpError::internal(e.to_string()).hint("this is a bug in the crate that registers the op: report it")
    })?;
    let services = ServicesBuilder::new().insert::<dyn ConnectionService>(Arc::new(connections.clone())).build();
    let executor = executor.with_services(services);
    Ok(match project_dir {
        Some(dir) => executor.with_project_dir(dir),
        None => executor,
    })
}

/// The project folder as agents are granted it: its real path, relative names and symlinks resolved (ADR 0013), because
/// a file root that is relative or reaches the folder through a link covers nothing.
///
/// # Errors
/// As [`open_runner`] for a folder that can't be opened.
pub fn project_root(dir: &Path) -> Result<String, OpError> {
    Ok(open_project_dir(dir)?.as_path().display().to_string())
}

fn open_project_dir(dir: &Path) -> Result<ProjectDir, OpError> {
    ProjectDir::open(dir).map_err(|e| match e {
        // A missing project stays `not_found` (exit 4), as it was before the folder was vetted here.
        ProjectDirError::NotFound(_) => OpError::not_found(e.to_string()),
        _ => OpError::invalid("project", e.to_string()),
    })
}

fn project_required() -> OpError {
    OpError::invalid("project", "this command needs a project").hint(
        "pass the project folder: omuzaki --project <DIR> <command> (`omuzaki ops` and `omuzaki help` don't need one)",
    )
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{Mode, Request};
    use serde_json::json;

    use super::*;

    /// The roots `omuzaki mcp` grants are real paths: a relative `--project`, or one reached through a link, would
    /// otherwise cover nothing (ADR 0013 rule 5).
    #[test]
    #[allow(clippy::disallowed_methods, reason = "a test sets up folders on disk; the fence is for ops")]
    fn the_granted_project_root_is_the_real_path_of_a_relative_or_linked_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let real = std::fs::canonicalize(dir.path()).expect("real path");
        let project = real.join("project");
        std::fs::create_dir(&project).expect("create");
        assert_eq!(project_root(&project).unwrap(), project.display().to_string());
        assert_eq!(project_root(&real.join("project/../project")).unwrap(), project.display().to_string());
        #[cfg(unix)]
        {
            let link = real.join("link");
            std::os::unix::fs::symlink(&project, &link).expect("symlink");
            assert_eq!(project_root(&link).unwrap(), project.display().to_string());
        }
        let relative = project_root(Path::new(".")).unwrap();
        assert!(Path::new(&relative).is_absolute(), "{relative}");
    }

    /// The real executor answers `agent.connection` from the handle `open_runner_with` wires, which is the one the MCP
    /// server counts sessions in. (`ops` needs no project.)
    #[test]
    fn the_runner_answers_agent_connection_from_the_wired_handle() {
        let cli = crate::cli::try_parse_from(["omuzaki", "ops"]).expect("valid command line");
        let connections = Connections::default();
        let runner = open_runner_with(&cli, &connections).expect("opens");
        let session = Session::person("tester");
        let request = Request {
            op: "agent.connection".to_owned(),
            args: json!({}),
            mode: Mode::DryRun,
            actor: session.actor,
            grants: session.grants,
            cause: None,
            tx: None,
            base: None,
        };
        let outcome = runner.run(request).expect("the op is wired, so it answers");
        assert_eq!(outcome.plan.output, Some(json!({"connected": false, "sessions": 0})));
    }
}
