//! `--args`: an op's arguments as inline JSON, `@file.json`, `@-` (standard input), or nothing for `{}`.

use std::fs::File;
use std::io::{self, Read};

use omuzaki_ops_api::OpError;
use serde_json::{Map, Value};

/// The most `--args @file` or `@-` will read. Arguments are one op's JSON object; anything bigger is a mistake (a
/// wrong path such as `/dev/zero`, or data that belongs in a file the op reads), and reading it unbounded would let
/// that mistake exhaust memory.
pub const MAX_ARGS_BYTES: u64 = 16 * 1024 * 1024;

const SOURCES_HINT: &str = "pass --args '<json>', --args @file.json, or --args @- to read standard input";

/// Reads and parses the arguments.
///
/// # Errors
/// - `NotFound` if the `@file` doesn't exist.
/// - `Invalid` (field `args`) if the file or stdin can't be read, is larger than [`MAX_ARGS_BYTES`], or isn't JSON.
#[allow(clippy::disallowed_methods, reason = "the CLI reads the `@file` the person typed; it is not an op")]
pub fn parse(raw: Option<&str>, stdin: &mut dyn Read) -> Result<Value, OpError> {
    let Some(raw) = raw else { return Ok(Value::Object(Map::new())) };
    match raw.strip_prefix('@') {
        None => from_json(raw.as_bytes(), "--args"),
        Some("-") => from_json(&read_capped(stdin, "standard input")?, "standard input"),
        Some(path) => {
            let file = File::open(path).map_err(|e| read_error(path, &e))?;
            from_json(&read_capped(file, path)?, path)
        }
    }
}

/// Reads at most [`MAX_ARGS_BYTES`]; one byte more is read to tell "exactly at the limit" from "over it".
fn read_capped(reader: impl Read, what: &str) -> Result<Vec<u8>, OpError> {
    let mut bytes = Vec::new();
    reader.take(MAX_ARGS_BYTES + 1).read_to_end(&mut bytes).map_err(|e| read_error(what, &e))?;
    if bytes.len() as u64 > MAX_ARGS_BYTES {
        return Err(invalid(format!("{what} is larger than 16 MiB, the most --args accepts"))
            .hint("arguments are one op's JSON object; keep big data in a file and pass its path in the arguments"));
    }
    Ok(bytes)
}

fn from_json(bytes: &[u8], what: &str) -> Result<Value, OpError> {
    serde_json::from_slice(bytes).map_err(|e| {
        invalid(format!("invalid JSON in {what}: {e}"))
            .hint("arguments must be a JSON object, e.g. --args '{\"blend\": 0.5}' (quote it for your shell)")
    })
}

/// A failed open or read. A missing file is `NotFound` (exit code 4); the rest are bad arguments.
fn read_error(what: &str, e: &io::Error) -> OpError {
    let error = match e.kind() {
        io::ErrorKind::NotFound => OpError::not_found(format!("no arguments file {what}")),
        _ => OpError::invalid("args", format!("can't read {what}: {e}")),
    };
    let hint = match e.kind() {
        io::ErrorKind::NotFound => "check the path after `@`; relative paths start in the current directory".to_owned(),
        io::ErrorKind::PermissionDenied => format!("you can't read {what}; check its permissions"),
        _ => SOURCES_HINT.to_owned(),
    };
    OpError { field: Some("args".to_owned()), ..error }.hint(hint)
}

fn invalid(message: String) -> OpError {
    OpError::invalid("args", message)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]
mod tests {
    use std::io::Cursor;

    use omuzaki_ops_api::ErrorCode;
    use serde_json::json;

    use super::*;

    fn parse_with(raw: Option<&str>, stdin: &[u8]) -> Result<Value, OpError> {
        parse(raw, &mut Cursor::new(stdin.to_vec()))
    }

    #[test]
    fn inline_json_and_the_empty_default() {
        assert_eq!(parse_with(None, b"").unwrap(), json!({}));
        assert_eq!(parse_with(Some(r#"{"blend": 0.5}"#), b"").unwrap(), json!({"blend": 0.5}));
    }

    #[test]
    fn at_dash_reads_standard_input() {
        assert_eq!(parse_with(Some("@-"), br#"{"from": "stdin"}"#).unwrap(), json!({"from": "stdin"}));
    }

    #[test]
    fn at_path_reads_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("args.json");
        std::fs::write(&file, r#"{"from": "file"}"#).unwrap();
        assert_eq!(parse_with(Some(&format!("@{}", file.display())), b"").unwrap(), json!({"from": "file"}));
    }

    #[test]
    fn a_missing_file_is_not_found_with_the_field_and_a_hint() {
        let e = parse_with(Some("@/definitely/not/here.json"), b"").unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::NotFound, Some("args")));
        assert!(e.hint.is_some_and(|h| h.contains("check the path")));
    }

    #[test]
    fn other_read_failures_are_invalid_and_say_how_to_pass_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let e = parse_with(Some(&format!("@{}", dir.path().display())), b"").unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("args")));
        assert!(e.hint.is_some_and(|h| h.contains("@-")), "a directory is not an arguments file");
    }

    #[test]
    fn bad_json_names_its_source() {
        let from_flag = parse_with(Some("{nope"), b"").unwrap_err();
        assert!(from_flag.message.starts_with("invalid JSON in --args:"), "{from_flag:?}");
        assert!(from_flag.hint.is_some_and(|h| h.contains("--args")));
        let from_stdin = parse_with(Some("@-"), b"{nope").unwrap_err();
        assert!(from_stdin.message.starts_with("invalid JSON in standard input:"), "{from_stdin:?}");
    }

    #[test]
    fn input_over_the_cap_is_refused_without_reading_it_all() {
        // An endless reader would never finish if the cap didn't stop it.
        let e = parse(Some("@-"), &mut io::repeat(b' ')).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("args")));
        assert!(e.message.contains("16 MiB"), "{e:?}");
        assert!(e.hint.is_some());
    }

    #[test]
    fn input_exactly_at_the_cap_is_accepted() {
        let padded = Cursor::new(b"1".to_vec()).chain(io::repeat(b' ').take(MAX_ARGS_BYTES - 1));
        assert_eq!(read_capped(padded, "test").unwrap().len() as u64, MAX_ARGS_BYTES);
        let over = Cursor::new(b"1".to_vec()).chain(io::repeat(b' ').take(MAX_ARGS_BYTES));
        assert_eq!(read_capped(over, "test").unwrap_err().code, ErrorCode::Invalid);
    }
}
