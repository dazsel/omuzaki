//! Everything that can go wrong opening, creating or saving a project.

use std::io;
use std::path::PathBuf;

/// Why a project couldn't be opened, created or saved.
///
/// Patches have their own error ([`omuzaki_doc_api::PatchError`]); this one is about files.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A file or folder couldn't be read, written or removed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What the operating system said.
        #[source]
        source: io::Error,
    },
    /// A project file isn't valid TOML.
    #[error("{}: {source}", path.display())]
    Syntax {
        /// The file.
        path: PathBuf,
        /// The parser's message, with line and column.
        #[source]
        source: toml_edit::TomlError,
    },
    /// A project file is valid TOML that the document can't hold (`nan`, `inf`).
    #[error("{}: {reason}", path.display())]
    Unsupported {
        /// The file.
        path: PathBuf,
        /// Where in the file and what is wrong.
        reason: String,
    },
    /// The folder has no `project.toml`.
    #[error("{} isn't a Omuzaki project: it has no project.toml", dir.display())]
    NotAProject {
        /// The folder.
        dir: PathBuf,
    },
    /// Another Omuzaki has the project open (see [`ProjectLock`](crate::ProjectLock)). `pid` is 0 when the holder's
    /// details aren't known (its owner file is missing or unreadable); the message then leaves them out.
    #[error("the project is open in another Omuzaki{}; close that one first", holder_text(*.pid, .host))]
    Locked {
        /// The process holding the lock.
        pid: u32,
        /// The machine it runs on.
        host: String,
        /// When it took the lock, in seconds since the Unix epoch.
        since: u64,
    },
    /// A file this save would replace is not the one this process last read or wrote: it was changed (or created)
    /// outside Omuzaki. The file is left untouched.
    #[error("{} was changed outside Omuzaki since it was loaded; it was not overwritten", path.display())]
    ChangedOnDisk {
        /// The file.
        path: PathBuf,
    },
    /// `create` found a project already there.
    #[error("{} already holds a project", dir.display())]
    AlreadyExists {
        /// The folder.
        dir: PathBuf,
    },
    /// `create` was given a blank name.
    #[error("a project needs a name")]
    EmptyName,
    /// A file and a folder both define the same key, so the tree has no single answer.
    #[error("{} and {} both define '{key}'; keep one of them", file.display(), folder.display())]
    Ambiguous {
        /// The shared key.
        key: String,
        /// The `<key>.toml` file.
        file: PathBuf,
        /// The `<key>` folder.
        folder: PathBuf,
    },
    /// Saving would write through a symbolic link, or replace one.
    #[error(
        "{} is a symbolic link, and the project store never writes through or over links; \
         replace it with a real file or folder (or move what it points to into the project)",
        path.display()
    )]
    Symlink {
        /// The link.
        path: PathBuf,
    },
    /// `project.toml` has no usable `schema = N`.
    #[error("project.toml: {0}")]
    BadSchema(String),
    /// The project was saved by a newer Omuzaki.
    #[error(
        "this project uses schema {found} but this version of Omuzaki only understands up to {supported}; update Omuzaki to open it"
    )]
    SchemaTooNew {
        /// The schema in `project.toml`.
        found: u32,
        /// The newest schema this build can open.
        supported: u32,
    },
    /// No migration produces schema `to`, so an old project can't be brought up to date.
    #[error("no migration upgrades schema {} to {to}", to - 1)]
    MissingMigration {
        /// The schema the missing migration should produce.
        to: u32,
    },
    /// Two migrations claim to produce the same schema.
    #[error("more than one migration produces schema {to}")]
    DuplicateMigration {
        /// The contested schema.
        to: u32,
    },
    /// A migration reported a failure.
    #[error("migration {name} (schema {} to {to}) failed: {source}", to - 1)]
    Migration {
        /// The migration's name.
        name: &'static str,
        /// The schema it produces.
        to: u32,
        /// Its message.
        #[source]
        source: crate::MigrationError,
    },
    /// A migration left the project in a shape that can't be stored.
    #[error("the upgraded project can't be stored: {0}")]
    MigratedTree(String),
    /// The store's own bookkeeping disagrees with the document. This is a bug in `omuzaki-doc`.
    #[error("internal error in omuzaki-doc: {0}")]
    Internal(String),
}

impl StoreError {
    /// An I/O error for `path`.
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}

/// " (pid N on HOST)" for a known holder; nothing when the holder is unknown (pid 0).
fn holder_text(pid: u32, host: &str) -> String {
    if pid == 0 { String::new() } else { format!(" (pid {pid} on {host})") }
}
