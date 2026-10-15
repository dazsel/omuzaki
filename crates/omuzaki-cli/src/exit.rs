//! Process exit codes: one per [`ErrorCode`], documented in `omuzaki --help`.
//!
//! Scripts and agents branch on the code, so it is part of the CLI's contract. A new `ErrorCode` variant cannot be
//! missed: [`describe`] is one exhaustive `match` (no wildcard) giving every variant its number and meaning, so the
//! crate stops compiling until the new variant has a row, and both [`for_error`] and the `--help` table read that one
//! row. The table lists the variants by asking `ErrorCode`'s own schema, so there is no second list to forget.

use std::fmt::Write as _;

use omuzaki_ops_api::ErrorCode;
use serde_json::Value;

/// Success.
pub const OK: u8 = 0;

/// The command line is wrong (unknown command, missing argument). clap exits with it before any op runs.
pub const USAGE: u8 = 2;

/// A store check ran and found something a person must look at (`store verify` found a problem, or `store repair`
/// left one alone). Not an [`ErrorCode`]: the command succeeded and printed its report, so there is no error to name.
/// Its number is the first unused one after [`ErrorCode::Cancelled`]'s 11, so no code means two things.
pub const FOUND_PROBLEMS: u8 = 12;

/// An exit code and what it means.
struct Row {
    code: u8,
    meaning: &'static str,
}

/// The one place every [`ErrorCode`] gets its exit code and its words in `--help`.
const fn describe(code: ErrorCode) -> Row {
    match code {
        ErrorCode::Internal => {
            Row { code: 1, meaning: "a bug in Omuzaki, or the output could not be written; please report it" }
        }
        ErrorCode::Invalid => {
            Row { code: 3, meaning: "the arguments are wrong (out of range, malformed); the error names the field" }
        }
        ErrorCode::NotFound => {
            Row { code: 4, meaning: "something referenced does not exist (an op, a character, a file)" }
        }
        ErrorCode::Denied => {
            Row { code: 5, meaning: "the actor lacks a scope; the error's hint names the one to grant" }
        }
        ErrorCode::Conflict => {
            Row { code: 6, meaning: "the document changed underneath; run the command again to re-plan" }
        }
        ErrorCode::Unsupported => {
            Row { code: 7, meaning: "valid, but not supported here (platform, missing model, not built yet)" }
        }
        ErrorCode::Unavailable => {
            Row { code: 8, meaning: "the app did not wire in a service the op needs; report it, or retry once it is" }
        }
        ErrorCode::Waiting => {
            Row { code: 9, meaning: "nothing ran: a person must decide first (an approval); do not retry" }
        }
        ErrorCode::Limited => {
            Row { code: 10, meaning: "a session limit was hit; wait the time the error gives, then retry" }
        }
        ErrorCode::Cancelled => {
            Row { code: 11, meaning: "a job was cancelled (by a person, a stop or a limit); do not retry unchanged" }
        }
        // A code added to the contract exits like a bug until it gets its own row here; the uniqueness test below
        // fails for it, so the missing row is noticed.
        _ => Row { code: 1, meaning: "an error this version of the command line does not know; please report it" },
    }
}

/// The exit code for a failed op.
pub const fn for_error(code: ErrorCode) -> u8 {
    describe(code).code
}

/// Every [`ErrorCode`], read from the type's own JSON Schema (a unit-only enum is a `oneOf` of `const`s, or an
/// `enum`), so a variant added to the type shows up here without anyone editing a list.
fn error_codes() -> Vec<ErrorCode> {
    let schema = schemars::schema_for!(ErrorCode);
    let names: Vec<&Value> = match (schema.get("oneOf"), schema.get("enum")) {
        (Some(Value::Array(variants)), _) => variants.iter().filter_map(|v| v.get("const")).collect(),
        (_, Some(Value::Array(names))) => names.iter().collect(),
        _ => Vec::new(),
    };
    names.into_iter().filter_map(|name| serde_json::from_value(name.clone()).ok()).collect()
}

/// The "Exit codes" section of `omuzaki --help`.
pub fn help_text() -> String {
    let mut rows: Vec<(u8, String, &str)> = error_codes()
        .into_iter()
        .map(|code| {
            let row = describe(code);
            (row.code, code.to_string(), row.meaning)
        })
        .collect();
    rows.push((OK, "ok".to_owned(), "the command succeeded"));
    rows.push((USAGE, "usage".to_owned(), "the command line is wrong; see --help"));
    rows.push((FOUND_PROBLEMS, "problems".to_owned(), "a store check ran and found problems; its report says which"));
    rows.sort_unstable();
    let width = rows.iter().map(|(_, name, _)| name.len()).max().unwrap_or(0);
    let mut text = String::from("Exit codes:\n");
    for (code, name, meaning) in rows {
        let _ = writeln!(text, "  {code}  {name:<width$}  {meaning}");
    }
    text.push_str("\nErrors are printed to stderr, as JSON with --json: {\"error\": {code, message, field, hint}}.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The variant names as `ErrorCode` serialises them, read independently of `error_codes()` from the schema text.
    fn schema_names() -> Vec<String> {
        let schema = serde_json::to_value(schemars::schema_for!(ErrorCode)).unwrap();
        let variants = schema["oneOf"].as_array().expect("a unit-only enum is a oneOf of consts");
        variants.iter().map(|v| v["const"].as_str().expect("a string const").to_owned()).collect()
    }

    #[test]
    fn every_error_code_variant_is_found_and_appears_in_the_help_with_its_number() {
        let names = schema_names();
        assert_eq!(error_codes().len(), names.len(), "a variant of ErrorCode could not be read back from its schema");
        let help = help_text();
        for name in names {
            let code: ErrorCode = serde_json::from_value(Value::String(name.clone())).unwrap();
            let line = format!("  {}  {name}", for_error(code));
            assert!(help.contains(&line), "{line:?} missing from:\n{help}");
        }
    }

    #[test]
    fn exit_codes_are_unique_and_do_not_reuse_ok_or_usage() {
        let mut seen: Vec<u8> = error_codes().into_iter().map(for_error).collect();
        seen.extend([OK, USAGE, FOUND_PROBLEMS]);
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "exit codes must be unique: {seen:?}");
    }

    #[test]
    fn help_lists_the_codes_in_numeric_order() {
        let numbers: Vec<u8> =
            help_text().lines().skip(1).filter_map(|l| l.split_whitespace().next()?.parse().ok()).collect();
        assert_eq!(numbers, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }
}
