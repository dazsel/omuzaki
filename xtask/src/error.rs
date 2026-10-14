//! The one error type every xtask command returns; `main` prints it and exits with code 2.

use std::io;

/// Why an xtask command could not run to a verdict (a failed *check* is a verdict, not an error).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A program could not be started at all (missing binary, bad working directory).
    #[error("cannot run `{command}`: {source}")]
    Spawn {
        /// The command line, for the message.
        command: String,
        /// The OS error.
        source: io::Error,
    },
    /// A helper program ran but exited non-zero.
    #[error("`{command}` failed: {stderr}")]
    Failed {
        /// The command line, for the message.
        command: String,
        /// Whatever the program printed to stderr.
        stderr: String,
    },
    /// `cargo metadata` printed something other than the JSON we expect.
    #[error("cannot parse `cargo metadata` output: {0}")]
    Metadata(#[from] serde_json::Error),
    /// The sources could not be scanned for registrations.
    #[error(transparent)]
    Registry(#[from] xtask::registry::Error),
}

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;
