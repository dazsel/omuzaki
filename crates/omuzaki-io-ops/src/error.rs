//! How a failed import or export reaches a caller: [`IoError`] becomes a structured [`OpError`].
//!
//! The mapping is the one `omuzaki-io-api` documents (`IoError`): bad or oversized files and rejected reports are
//! `Invalid`, missing features are `Unsupported`, unreadable sidecars are `NotFound`, bugs stay `Internal`.

use omuzaki_io_api::{Finding, IoError};
use omuzaki_ops_api::{ErrorCode, OpError};

/// Findings spelled out in the message of a rejection; the rest are counted. An `OpError` carries no report, so the
/// message has to be enough to act on.
const SHOWN_FINDINGS: usize = 5;

/// The op error for `error`. `field` names the argument at fault for the errors that blame the input (`path`).
#[must_use]
pub fn from_io(error: IoError, field: &str) -> OpError {
    match error {
        IoError::Malformed { message, at } => {
            let place = at.map(|at| format!(" (at {at})")).unwrap_or_default();
            OpError::invalid(field, format!("malformed file: {message}{place}"))
                .hint("the file may be truncated or damaged; export it again from the tool that made it")
        }
        IoError::Unsupported { what, hint } => {
            let error = OpError::unsupported(format!("unsupported: {what}"));
            match hint {
                Some(hint) => error.hint(hint),
                None => error,
            }
        }
        IoError::Resource { uri, reason } => OpError::not_found(format!("couldn't read {uri}: {reason}")),
        limit @ IoError::Limit { .. } => OpError::invalid(field, limit.to_string())
            .hint("the limit protects against files that would exhaust memory; simplify the model or split it"),
        IoError::Rejected(report) => {
            let errors: Vec<&Finding> = report.errors().collect();
            let mut listed: Vec<String> = errors.iter().take(SHOWN_FINDINGS).map(|f| describe(f)).collect();
            if errors.len() > SHOWN_FINDINGS {
                listed.push(format!("and {} more", errors.len() - SHOWN_FINDINGS));
            }
            let error =
                OpError::invalid(field, format!("rejected with {} error(s): {}", errors.len(), listed.join("; ")));
            match errors.iter().find_map(|f| f.hint.as_deref()) {
                Some(hint) => error.hint(hint),
                None => error,
            }
        }
        IoError::Cancelled => {
            // The job manager reads `Cancelled` as a cancellation, not a failure, and attaches the detail naming who and
            // why. `OpError::cancelled` needs the job id, which this conversion does not have.
            let error = OpError {
                code: ErrorCode::Cancelled,
                message: "cancelled before it finished".to_owned(),
                field: None,
                hint: None,
                detail: None,
            };
            error.hint("nothing was changed; run it again")
        }
        IoError::Internal(message) => OpError::internal(message),
    }
}

/// `rule at location: message`, the way a report line reads.
fn describe(finding: &Finding) -> String {
    match &finding.location {
        Some(at) => format!("{} at {at}: {}", finding.rule, finding.message),
        None => format!("{}: {}", finding.rule, finding.message),
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_io_api::Report;
    use omuzaki_ops_api::ErrorCode;

    use super::*;

    #[test]
    fn each_io_error_maps_to_the_documented_class() {
        let rejected = IoError::Rejected(Report { findings: vec![Finding::error("scene.x", "bad")] });
        let cases = [
            (IoError::malformed("bad magic"), ErrorCode::Invalid),
            (IoError::Limit { what: "node count", limit: 1, got: 2 }, ErrorCode::Invalid),
            (rejected, ErrorCode::Invalid),
            (IoError::unsupported("sparse accessors", "densify them"), ErrorCode::Unsupported),
            (IoError::Resource { uri: "a.bin".into(), reason: "no such file".into() }, ErrorCode::NotFound),
            (IoError::Cancelled, ErrorCode::Cancelled),
            (IoError::Internal("oops".into()), ErrorCode::Internal),
        ];
        for (error, code) in cases {
            let shown = error.to_string();
            assert_eq!(from_io(error, "path").code, code, "{shown}");
        }
    }

    #[test]
    fn errors_that_blame_the_input_name_the_field_and_say_how_to_fix_it() {
        let malformed =
            from_io(IoError::Malformed { message: "chunk is cut short".into(), at: Some("12".into()) }, "path");
        assert_eq!(malformed.field.as_deref(), Some("path"));
        assert!(malformed.message.contains("chunk is cut short") && malformed.message.contains("at 12"));
        assert!(malformed.hint.is_some());
        let unsupported = from_io(IoError::unsupported("sparse accessors", "densify them"), "path");
        assert_eq!(unsupported.hint.as_deref(), Some("densify them"));
    }

    #[test]
    fn a_cancel_is_left_for_the_job_manager_to_name() {
        let error = from_io(IoError::Cancelled, "path");
        assert!(error.detail.is_none(), "the executor attaches who and why");
        assert_eq!(error.hint.as_deref(), Some("nothing was changed; run it again"));
    }

    #[test]
    fn a_rejection_lists_the_first_errors_and_counts_the_rest() {
        let mut report = Report::new();
        report.push(Finding::warning("scene.w", "not an error"));
        for i in 0..7 {
            report.push(
                Finding::error("scene.index_out_of_range", format!("index {i}")).at(format!("nodes/{i}")).hint("fix"),
            );
        }
        let error = from_io(IoError::Rejected(report), "path");
        assert!(error.message.starts_with("rejected with 7 error(s): "), "{}", error.message);
        assert!(error.message.contains("scene.index_out_of_range at nodes/0: index 0"));
        assert!(error.message.contains("index 4") && !error.message.contains("index 5"));
        assert!(error.message.ends_with("and 2 more"));
        assert!(!error.message.contains("not an error"), "warnings are not listed");
        assert_eq!(error.hint.as_deref(), Some("fix"));
    }
}
