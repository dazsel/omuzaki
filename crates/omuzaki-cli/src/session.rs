//! Who is running the command, and which runner executes it.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_mcp::Connections;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, ConnectionService, Grants, Jobs, OpError, ProjectDir, ProjectDirError, Runner, ServicesBuilder, Sessions,
};

use crate::cli::Cli;
use crate::project::{NoProject, SavedProject};

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

/// Opens the runner that executes `cli`'s command: the op executor over the `--project` folder.
///
/// The executor runs every op linked into the binary, and with `--project` it knows the project folder, so ops can
/// read project-relative files (and nothing outside it without an `fs` root). Commands that only read the op registry (`ops`, `help`) get an
/// empty document when no project is given; the others refuse to start without one.
///
/// # Errors
/// - `Invalid` (field `project`) if `--project` is a file or a filesystem root, if the command needs a project and `--project` is missing, or the project's files
///   are broken.
/// - `NotFound` if `--project` names no project; `Unsupported` if it was written by a newer Omuzaki.
/// - `Internal` if two linked ops share a name (a bug in whichever crate registered them).
pub fn open_runner(cli: &Cli) -> Result<Arc<dyn Runner>, OpError> {
    open_runner_with(cli, &Connections::default())
}

/// As [`open_runner`], with `connections` wired into the executor's services so `agent.connection` can read it. Give
/// the same handle to the MCP server (`omuzaki_mcp::McpOptions::with_connections`).
///
/// # Errors
/// As [`open_runner`].
pub fn open_runner_with(cli: &Cli, connections: &Connections) -> Result<Arc<dyn Runner>, OpError> {
    Ok(open_executor_with(cli, connections)?)
}

/// As [`open_runner_with`], keeping the executor's type so its session registry can be reached too.
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
        Some(dir) => Box::new(SavedProject::open(dir)?),
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
