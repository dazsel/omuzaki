//! Thin wrappers over `std::process` shared by the commands.

use std::env;
use std::ffi::OsString;
use std::process::Command;

use crate::error::{Error, Result};

/// The cargo to invoke: the one that started us (`cargo run` sets `CARGO`), else the one on `PATH`.
pub fn cargo() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// Runs `command` to completion and returns its stdout; a non-zero exit becomes an error that carries stderr.
pub fn capture(command: &mut Command) -> Result<Vec<u8>> {
    let output = command.output().map_err(|source| Error::Spawn { command: describe(command), source })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::Failed {
            command: describe(command),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// The command line as a person would type it, for messages.
pub fn describe(command: &Command) -> String {
    let mut parts = vec![command.get_program().to_string_lossy().into_owned()];
    parts.extend(command.get_args().map(|arg| arg.to_string_lossy().into_owned()));
    parts.join(" ")
}
