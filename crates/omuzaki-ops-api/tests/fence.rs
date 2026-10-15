//! Fences: the helpers' rules (segment-wise, empty fences) and the new types' contract (design `agent-sessions.md`
//! §5.3).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{
    Actor, ClashKind, ErrorCode, ErrorDetail, Event, OpError, SessionChange, SessionId, fence_covers, fence_overlap,
    fence_within, normalize_fence,
};
use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use serde_json::json;

fn p(s: &str) -> DocPath {
    DocPath::parse(s).unwrap()
}

fn ps(paths: &[&str]) -> Vec<DocPath> {
    paths.iter().map(|s| p(s)).collect()
}

fn agent() -> Actor {
    Actor::Agent { id: "claude-code#2".into(), model: None }
}

#[test]
fn normalize_sorts_dedups_and_drops_covered_paths() {
    assert_eq!(normalize_fence(ps(&["a/b", "a", "a/b"])).unwrap(), ps(&["a"]));
    assert_eq!(normalize_fence(ps(&["z", "a/bc", "a/b", "a/b/c"])).unwrap(), ps(&["a/b", "a/bc", "z"]));
    assert_eq!(normalize_fence(vec![]).unwrap(), vec![]);
}

#[test]
fn normalize_rejects_the_root_with_a_hint() {
    let error = normalize_fence(ps(&["a", ""])).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some("fence"));
    assert!(error.hint.unwrap().contains("a fence of the whole project is an empty fence"));
}

#[test]
fn overlap_is_segment_wise() {
    assert_eq!(fence_overlap(&ps(&["a/b"]), &ps(&["a/bc"])), None);
    let (a, b) = (ps(&["a/b"]), ps(&["a/b/c"]));
    assert_eq!(fence_overlap(&a, &b), Some((&a[0], &b[0])));
    assert_eq!(fence_overlap(&b, &a), Some((&b[0], &a[0])));
    assert_eq!(fence_overlap(&[], &ps(&["a"])), None);
    assert_eq!(fence_overlap(&ps(&["a"]), &[]), None);
    assert_eq!(fence_overlap(&[], &[]), None);
}

#[test]
fn covers_and_within() {
    assert!(fence_covers(&[], &p("any/thing")));
    assert!(fence_covers(&ps(&["a/b"]), &p("a/b")));
    assert!(fence_covers(&ps(&["a/b"]), &p("a/b/c")));
    assert!(!fence_covers(&ps(&["a/b"]), &p("a/bc")));
    assert!(!fence_covers(&ps(&["a/b"]), &p("a")));

    assert!(fence_within(&ps(&["a/b/c"]), &ps(&["a/b"])));
    assert!(!fence_within(&ps(&["a"]), &ps(&["a/b"])));
    assert!(fence_within(&ps(&["a"]), &[]), "everything is within the whole project");
    assert!(!fence_within(&[], &ps(&["a"])), "the whole project is not within a part");
    assert!(fence_within(&[], &[]));
}

#[test]
fn fenced_error_is_denied_names_the_holder_and_carries_the_detail() {
    let error = OpError::fenced(p("a/b"), Some(agent()));
    assert_eq!(error.code, ErrorCode::Denied);
    assert!(error.message.contains("claude-code#2") && error.message.contains("a/b"), "{}", error.message);
    assert_eq!(error.detail.as_deref(), Some(&ErrorDetail::Fenced { path: p("a/b"), holder: Some(agent()) }));
    assert!(error.hint.is_some());

    let own = OpError::fenced(p("a/b"), None);
    assert_eq!(own.code, ErrorCode::Denied);
    assert_eq!(own.detail.as_deref(), Some(&ErrorDetail::Fenced { path: p("a/b"), holder: None }));
    assert_eq!(serde_json::to_value(&own).unwrap()["detail"], json!({"kind": "fenced", "path": "a/b", "holder": null}));
}

/// Existing `Event` JSON is unchanged by the new variant: fixed expected text, not a round trip through the types.
#[test]
fn existing_event_json_is_byte_identical() {
    let committed = Event::TxCommitted { tx: omuzaki_ops_api::TxId(1), label: "l".into(), actor: Actor::System };
    assert_eq!(
        serde_json::to_string(&committed).unwrap(),
        r#"{"event":"tx_committed","tx":1,"label":"l","actor":{"kind":"system"}}"#
    );
    let session_changed = SessionChange::Spent;
    assert_eq!(serde_json::to_string(&session_changed).unwrap(), r#"{"change":"spent"}"#);
    let legacy = r#"{"event":"tx_committed","tx":1,"label":"l","actor":{"kind":"system"}}"#;
    assert_eq!(serde_json::from_str::<Event>(legacy).unwrap(), committed);
}

#[test]
fn clash_event_has_a_stable_shape_and_schema() {
    let event = Event::Clash {
        session: SessionId(2),
        actor: agent(),
        op: "x.y".into(),
        path: p("a"),
        kind: ClashKind::Stale,
        other: None,
        at: Seconds(1.0),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "clash");
    assert_eq!(json["kind"], "stale");
    assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
    let schema = serde_json::to_string(&schemars::schema_for!(ClashKind)).unwrap();
    assert!(schema.contains("\"fenced\"") && schema.contains("\"stale\""), "{schema}");
    assert_eq!(serde_json::to_value(SessionChange::Fence { by: agent() }).unwrap()["change"], "fence");
}

fn arb_path() -> impl Strategy<Value = DocPath> {
    // A tiny alphabet so prefixes, near-prefixes (`a` vs `ab`) and equal paths all turn up.
    prop::collection::vec(prop::sample::select(vec!["a", "ab", "b"]), 1..4)
        .prop_map(|segments| DocPath::parse(&segments.join("/")).unwrap())
}

fn arb_fence() -> impl Strategy<Value = Vec<DocPath>> {
    prop::collection::vec(arb_path(), 0..5)
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(0x5EED),
        ..ProptestConfig::default()
    })]

    #[test]
    fn overlap_is_symmetric(a in arb_fence(), b in arb_fence()) {
        prop_assert_eq!(fence_overlap(&a, &b).is_some(), fence_overlap(&b, &a).is_some());
    }

    #[test]
    fn within_implies_covered(x in arb_fence(), y in arb_fence()) {
        if fence_within(&x, &y) {
            prop_assert!(x.iter().all(|path| fence_covers(&y, path)));
        }
    }

    #[test]
    fn normalize_keeps_the_claim(paths in arb_fence(), probe in arb_path()) {
        let normal = normalize_fence(paths.clone()).unwrap();
        prop_assert_eq!(normalize_fence(normal.clone()).unwrap(), normal.clone());
        if !paths.is_empty() {
            prop_assert_eq!(fence_covers(&paths, &probe), fence_covers(&normal, &probe));
        }
        prop_assert!(fence_within(&normal, &paths) && fence_within(&paths, &normal));
    }
}
