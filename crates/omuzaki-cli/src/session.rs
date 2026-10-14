//! Who is running the command, and which runner executes it.

use std::path::Path;
use std::sync::Arc;

use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, Grants, OpError, ProjectDir, ProjectDirError, Runner};

use crate::cli::Cli;
use crate::project::{NoProject, SavedProject};

/// The actor and grants every request of one `omuzaki` invocation carries.
#[derive(Clone, Debug)]
pub struct Session {
    /// Who is acting; logged with every applied op.
    pub actor: Actor,
    /// What they may do.
    pub grants: Grants,
}

impl Session {
    /// A person at the command line holds every scope (spec §6 defaults: people get everything).
    pub fn person(name: impl Into<String>) -> Self {
        Self { actor: Actor::Person { name: name.into() }, grants: Grants::person() }
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
