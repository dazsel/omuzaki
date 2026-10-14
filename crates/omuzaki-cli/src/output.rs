//! Printing a command's result and turning it into the process exit code.

use std::fmt::Write as _;
use std::io::{self, Write};

use omuzaki_ops_api::{ErrorCode, OpError};
use serde::Serialize;

use crate::{exit, json};

/// Writes a command's result: the text to `out` on success, or the error to `err`. Returns the exit code
/// (see [`exit`]).
///
/// A reader that closes the pipe early (`omuzaki ops | head`) is not a failure, so a broken pipe leaves the code alone.
pub fn finish(result: Result<String, OpError>, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let (code, written) = match result {
        Ok(text) => (exit::OK, send(out, &text)),
        Err(error) => (exit::for_error(error.code), send(err, &error_text(&error, json))),
    };
    match written {
        Ok(()) => code,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => code,
        // If stderr itself can't be written there is no one left to tell; the op's own code stays.
        Err(_) if code != exit::OK => code,
        Err(e) => {
            let internal = ErrorCode::Internal;
            let _ = writeln!(err, "error: {internal}: writing the output failed: {e}");
            exit::for_error(internal)
        }
    }
}

fn send(stream: &mut dyn Write, text: &str) -> io::Result<()> {
    stream.write_all(text.as_bytes())?;
    stream.flush()
}

/// Text for a failed command, for stderr: `error: <code>: <message>` plus the field and hint when there are any, or
/// `{"error": {...}}` with `--json`.
pub fn error_text(error: &OpError, json: bool) -> String {
    #[derive(Serialize)]
    struct Body<'a> {
        error: &'a OpError,
    }
    if json && let Ok(text) = json::pretty(&Body { error }) {
        return text;
    }
    let mut text = format!("error: {error}\n");
    if let Some(field) = &error.field {
        let _ = writeln!(text, "  field: {field}");
    }
    if let Some(hint) = &error.hint {
        let _ = writeln!(text, "  hint: {hint}");
    }
    text
}
