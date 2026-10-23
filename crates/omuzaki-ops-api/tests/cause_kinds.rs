//! `CauseKind::Review` and `CauseKind::Plugin` (ADR 0056, ADR 0058; #1550): their JSON and round trips, and both
//! enums that grew a variant here being `#[non_exhaustive]`, which a match outside the crate must answer with a
//! wildcard arm.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_ops_api::{Cause, CauseKind, EndReason};
use serde_json::json;

#[test]
fn a_review_cause_carries_the_comment_id_only_and_round_trips() {
    let cause = Cause { kind: CauseKind::Review, text: Some("c-7f3a".into()) };
    let value = serde_json::to_value(&cause).unwrap();
    assert_eq!(value, json!({ "kind": "review", "text": "c-7f3a" }));
    assert_eq!(serde_json::from_value::<Cause>(value).unwrap(), cause);
}

#[test]
fn a_plugin_cause_names_the_plugin_version_and_entry_and_round_trips() {
    let cause = Cause { kind: CauseKind::Plugin, text: Some("example@0.1.0 init".into()) };
    let value = serde_json::to_value(&cause).unwrap();
    assert_eq!(value, json!({ "kind": "plugin", "text": "example@0.1.0 init" }));
    assert_eq!(serde_json::from_value::<Cause>(value).unwrap(), cause);
    let schema = serde_json::to_string(&schemars::schema_for!(CauseKind)).unwrap();
    for name in ["direct", "prompt", "chat", "rule", "script", "replay", "review", "plugin"] {
        assert!(schema.contains(&format!("\"{name}\"")), "the CauseKind schema lacks {name}");
    }
}

/// A consumer outside the crate: without the wildcard arms this would not compile, because both enums are
/// `#[non_exhaustive]`.
fn label(kind: CauseKind, reason: &EndReason) -> (&'static str, &'static str) {
    let kind = match kind {
        CauseKind::Plugin => "plugin",
        CauseKind::Review => "review",
        _ => "other",
    };
    let reason = match reason {
        EndReason::Faulted { .. } => "faulted",
        _ => "other",
    };
    (kind, reason)
}

#[test]
fn matches_outside_the_crate_need_a_wildcard() {
    assert_eq!(label(CauseKind::Plugin, &EndReason::faulted("trap in init")), ("plugin", "faulted"));
    assert_eq!(label(CauseKind::Direct, &EndReason::Closed), ("other", "other"));
}
