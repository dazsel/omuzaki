//! Who is running the command, and which runner executes it.

use std::path::Path;
use std::sync::Arc;

use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_mcp::Connections;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, ConnectionService, Grants, OpError, ProjectDir, ProjectDirError, Runner, ServicesBuilder,
};

use crate::cli::Cli;
use crate::project::{NoProject, SavedProject};

/// The actor and grants every request of one `omuzaki` invocation carries.
#[derive(Clone, Debug)]
pub struct Session {
    /// Who is acting; logged with every applied op.
    pub actor: Actor,
    /// What they may do.
    pub grants: Grants,
    /// The agent sessions of this invocation: `omuzaki mcp` counts its clients here, and `agent.connection` reads it.
    pub connections: Connections,
}

impl Session {
    /// A person at the command line holds every scope (spec §6 defaults: people get everything).
    pub fn person(name: impl Into<String>) -> Self {
        Self {
            actor: Actor::Person { name: name.into() },
            grants: Grants::person(),
            connections: Connections::default(),
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
    Ok(Arc::new(match project_dir {
        Some(dir) => executor.with_project_dir(dir),
        None => executor,
    }))
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
