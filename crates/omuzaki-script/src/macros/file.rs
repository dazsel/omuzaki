//! The macro file: JSON lines, a header and then one call per line. The format is stable and byte-identical on a round
//! trip (`from_jsonl(to_jsonl(m))` gives `m` back, and writing that gives the same bytes).

use omuzaki_doc_api::Value;
use serde_json::Map;

use super::{Macro, RecordedCall};

/// The file format version this build writes and the only one it reads.
const VERSION: u64 = 1;

/// Why a macro file was refused: the 1-based line and what is wrong with it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {reason}")]
pub struct FileError {
    /// The 1-based line of the problem (1 for an empty file).
    pub line: usize,
    /// What is wrong, in a sentence.
    pub reason: String,
}

impl FileError {
    fn at(line: usize, reason: impl Into<String>) -> Self {
        Self { line, reason: reason.into() }
    }
}

/// The macro as JSON lines: a header line `{"macro":1,"name":"..."}`, then one `{"op":"...","args":{...}}` per line,
/// each ended by a line break.
pub fn to_jsonl(m: &Macro) -> String {
    // Written by hand around `Value`'s `Display`, which can't fail, so the field order is the documented one whatever
    // the JSON map type orders by.
    let mut text = format!("{{\"macro\":{VERSION},\"name\":{}}}\n", Value::String(m.name.clone()));
    for step in &m.steps {
        text.push_str(&step_line(step));
    }
    text
}

fn step_line(step: &RecordedCall) -> String {
    format!("{{\"op\":{},\"args\":{}}}\n", Value::String(step.op.clone()), step.args)
}

/// Reads a macro file.
///
/// # Errors
/// A [`FileError`] with the line number: a line that isn't JSON, an unknown version, a call with no op or an `args`
/// that isn't an object, or a blank line.
pub fn from_jsonl(text: &str) -> Result<Macro, FileError> {
    let mut lines = text.split('\n').enumerate().map(|(i, line)| (i + 1, line));
    let (_, first) = lines.next().unwrap_or((1, ""));
    let header = parse_object(1, first).map_err(|e| FileError::at(1, format!("the header: {}", e.reason)))?;
    let version = header.get("macro").and_then(Value::as_u64);
    match version {
        Some(VERSION) => {}
        Some(other) => {
            return Err(FileError::at(1, format!("unknown macro version {other}; this build reads version {VERSION}")));
        }
        None => return Err(FileError::at(1, "the first line must be a header like {\"macro\":1,\"name\":\"...\"}")),
    }
    let Some(name) = header.get("name").and_then(Value::as_str) else {
        return Err(FileError::at(1, "the header needs a `name`"));
    };
    let mut steps = Vec::new();
    let mut rest: Vec<(usize, &str)> = lines.collect();
    // The line break that ends the last line leaves one empty piece after it.
    if rest.last().is_some_and(|(_, line)| line.is_empty()) {
        rest.pop();
    }
    for (number, line) in rest {
        let object = parse_object(number, line)?;
        let Some(op) = object.get("op").and_then(Value::as_str).filter(|op| !op.is_empty()) else {
            return Err(FileError::at(number, "a call needs an `op` name"));
        };
        let args = object.get("args").cloned().unwrap_or(Value::Null);
        if !args.is_object() {
            return Err(FileError::at(number, format!("`args` of `{op}` must be an object")));
        }
        steps.push(RecordedCall { op: op.to_owned(), args });
    }
    Ok(Macro { name: name.to_owned(), steps })
}

fn parse_object(line: usize, text: &str) -> Result<Map<String, Value>, FileError> {
    if text.trim().is_empty() {
        return Err(FileError::at(line, "the line is empty"));
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(FileError::at(line, "the line must be a JSON object")),
        Err(e) => Err(FileError::at(line, format!("not JSON: {e}"))),
    }
}
