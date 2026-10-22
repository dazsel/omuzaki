//! Why the app core can't open, serve or close a project.

use omuzaki_doc::StoreError;
use omuzaki_doc::journal::JournalError;
use omuzaki_history_api::SyncError;
use omuzaki_mcp::ServerError;
use omuzaki_ops::ExecutorError;
use omuzaki_ops_api::OpError;

/// Why the app core can't open, serve or close a project.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppError {
    /// The folder isn't a project, is open in another Omuzaki, or its files can't be read.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The journal can't be read or started (a newer Omuzaki wrote it, a file can't be written).
    #[error(transparent)]
    Journal(JournalError),
    /// The executor can't be built: two ops share a name, or the history is from a newer Omuzaki.
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    /// The MCP server can't be built or served.
    #[error(transparent)]
    Mcp(#[from] ServerError),
    /// The project folder can't be given to ops (it is a filesystem root, or vanished).
    #[error(transparent)]
    Op(#[from] OpError),
    /// Saving didn't finish: the reason, or that it timed out. Nothing applied is lost; the journal holds it.
    #[error(transparent)]
    Sync(#[from] SyncError),
    /// The async runtime the MCP server runs on can't start.
    #[error("the MCP runtime can't start: {0}")]
    Runtime(#[source] std::io::Error),
}

impl From<JournalError> for AppError {
    /// A store error met while reading the journal is reported as the store's, so a broken project file reads the same
    /// whether or not the project has a journal.
    fn from(error: JournalError) -> Self {
        match error {
            JournalError::Store(store) => Self::Store(store),
            other => Self::Journal(other),
        }
    }
}

impl AppError {
    /// The error as the structured [`OpError`] the CLI and MCP report, with a hint where the person can act.
    pub fn to_op_error(&self) -> OpError {
        match self {
            Self::Op(error) => error.clone(),
            Self::Store(StoreError::Locked { .. }) => {
                OpError::conflict(self.to_string()).hint("close the project in the other Omuzaki window first")
            }
            Self::Store(StoreError::NotAProject { .. }) => {
                OpError::not_found(self.to_string()).hint("pass the folder that holds project.toml: --project <DIR>")
            }
            Self::Store(StoreError::SchemaTooNew { .. }) | Self::Journal(JournalError::TooNew { .. }) => {
                OpError::unsupported(self.to_string())
                    .hint("open the project with the Omuzaki version that wrote it, or update this one")
            }
            Self::Store(StoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                OpError::not_found(self.to_string()).hint("pass the project folder: --project <DIR>")
            }
            Self::Store(_) => OpError::invalid("project", self.to_string())
                .hint("fix the file named above (or restore it from version control), then try again"),
            _ => OpError::internal(self.to_string()).hint("this is a bug in Omuzaki: report it"),
        }
    }
}
