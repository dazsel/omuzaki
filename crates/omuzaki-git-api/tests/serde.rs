//! Serde round trips, JSON Schema snapshots, `CommitId::parse` and error texts.
#![allow(clippy::expect_used)] // tests may panic on a broken fixture

use omuzaki_core::time::Seconds;
use omuzaki_git_api::commit::{Commit, CommitDetail, CommitId, LogRange, SkipReason};
use omuzaki_git_api::error::GitError;
use omuzaki_git_api::git::PullOutcome;
use omuzaki_git_api::status::{ChangeKind, ChangedFile, GitStatus};
use schemars::{JsonSchema, schema_for};
use serde::Serialize;
use serde::de::DeserializeOwned;

const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn id(s: &str) -> CommitId {
    CommitId::parse(s).expect("valid id")
}

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let json = serde_json::to_string(value).expect("serialise");
    let back: T = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(&back, value, "{json}");
}

fn schema<T: JsonSchema>() -> String {
    serde_json::to_string(&schema_for!(T)).expect("schema")
}

fn commit() -> Commit {
    Commit { id: id(SHA1), parents: vec![id(SHA256)], author: "A".into(), at: Seconds(1.5), subject: "s".into() }
}

fn file(path: &str, kind: ChangeKind) -> ChangedFile {
    ChangedFile { path: path.into(), kind }
}

#[test]
fn commit_id_parse_table() {
    let cases: [(&str, bool); 8] = [
        (SHA1, true),
        (SHA256, true),
        (&SHA1[..39], false),
        ("0123456789abcdef0123456789abcdef012345678", false),
        (&SHA1.to_uppercase(), false),
        ("-abc", false),
        ("", false),
        ("-123456789abcdef0123456789abcdef01234567", false),
    ];
    for (text, ok) in cases {
        assert_eq!(CommitId::parse(text).is_ok(), ok, "{text:?}");
        if !ok {
            assert_eq!(CommitId::parse(text), Err(GitError::BadRev));
        }
    }
    assert_eq!(id(SHA1).as_str(), SHA1);
    assert_eq!(id(SHA256).short(), "01234567");
}

#[test]
fn commit_id_serialises_as_a_string_and_refuses_bad_text() {
    assert_eq!(serde_json::to_string(&id(SHA1)).expect("json"), format!("\"{SHA1}\""));
    assert!(serde_json::from_str::<CommitId>("\"-abc\"").is_err());
    assert!(serde_json::from_str::<CommitId>("12").is_err());
}

#[test]
fn types_round_trip() {
    round_trip(&GitStatus {
        branch: Some("main".into()),
        head: Some(id(SHA1)),
        ahead: 2,
        behind: 1,
        changed: vec![
            file("a.txt", ChangeKind::Added),
            file("b.txt", ChangeKind::Modified),
            file("c.txt", ChangeKind::Deleted),
            file("d.txt", ChangeKind::Renamed { from: "e.txt".into() }),
            file("f.txt", ChangeKind::Untracked),
            file("g.txt", ChangeKind::Conflicted),
        ],
        private_tracked: vec![".omuzaki/x".into()],
        merge_in_progress: true,
    });
    round_trip(&GitStatus {
        branch: None,
        head: None,
        ahead: 0,
        behind: 0,
        changed: vec![],
        private_tracked: vec![],
        merge_in_progress: false,
    });
    round_trip(&commit());
    round_trip(&CommitDetail { commit: commit(), files: vec![file("a", ChangeKind::Added)] });
    for r in [LogRange::Head, LogRange::Rev("main".into()), LogRange::Between { from: "a".into(), to: "b".into() }] {
        round_trip(&r);
    }
    assert_eq!(LogRange::default(), LogRange::Head);
    round_trip(&PullOutcome::UpToDate);
    round_trip(&PullOutcome::FastForwarded { from: id(SHA1), to: id(SHA256), commits: 3 });
    round_trip(&SkipReason::TooLarge);
    round_trip(&SkipReason::NotText);
}

fn all_errors() -> Vec<GitError> {
    vec![
        GitError::Missing { found: None },
        GitError::Missing { found: Some("git version 2.20.0".into()) },
        GitError::Unsupported,
        GitError::NotARepo,
        GitError::BadRev,
        GitError::NothingToCommit,
        GitError::BadPath { path: "../x".into() },
        GitError::NotFastForward { ahead: 1, behind: 2 },
        GitError::AuthRequired,
        GitError::HookRejected { output: "no".into() },
        GitError::Timeout,
        GitError::Cancelled,
        GitError::Failed { code: Some(128), stderr: "boom".into() },
        GitError::Failed { code: None, stderr: "killed".into() },
    ]
}

#[test]
fn git_error_round_trips_and_every_variant_has_text() {
    for e in all_errors() {
        assert!(!e.to_string().is_empty(), "{e:?}");
        round_trip(&e);
    }
    assert!(GitError::Missing { found: None }.to_string().contains("Install git 2.30 or later"));
}

#[test]
fn commit_id_schema_matches_parse() {
    let value = serde_json::to_value(schema_for!(CommitId)).expect("schema");
    assert_eq!(value["pattern"], "^([0-9a-f]{40}|[0-9a-f]{64})$");
    assert_eq!(value["type"], "string");
}

#[test]
fn schemas_are_stable() {
    // A change here is a change to the contract. Regenerate with `UPDATE_SCHEMAS=1 cargo test -p omuzaki-git-api`.
    let snaps: [(&str, String); 10] = [
        ("GitStatus", schema::<GitStatus>()),
        ("ChangedFile", schema::<ChangedFile>()),
        ("ChangeKind", schema::<ChangeKind>()),
        ("Commit", schema::<Commit>()),
        ("CommitId", schema::<CommitId>()),
        ("CommitDetail", schema::<CommitDetail>()),
        ("LogRange", schema::<LogRange>()),
        ("PullOutcome", schema::<PullOutcome>()),
        ("SkipReason", schema::<SkipReason>()),
        ("GitError", schema::<GitError>()),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/schemas");
    for (name, actual) in &snaps {
        let path = dir.join(format!("{name}.json"));
        if std::env::var_os("UPDATE_SCHEMAS").is_some() {
            std::fs::create_dir_all(&dir).expect("create schema dir");
            std::fs::write(&path, format!("{actual}\n")).expect("write schema");
        }
        let expected = std::fs::read_to_string(&path).expect("schema snapshot exists");
        assert_eq!(expected.trim_end(), actual, "schema of {name} changed");
    }
}
