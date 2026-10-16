//! MCP client transcripts for the agent surface: every message in and out, as a bare JSON-RPC client sees it, for each
//! tool, resource, prompt and task the surface adds. They are committed under `tests/transcripts/` and compared on
//! every run, so what the PR shows is what the server does. Regenerate with `UPDATE_TRANSCRIPTS=1`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::jobs::Fake;
use common::registry::Studio;
use common::wire::{Wire, check_transcript};
use omuzaki_library_api::{
    CompatStatus, ItemDetail, ItemId, ItemInfo, ItemKind, ItemManifest, ItemScope, Library, LibraryError,
    PublisherCheck, Query, ResolvedItem, RightsBadge, SearchPage, Section, Version, VersionReq,
};
use omuzaki_mcp::{InAppSpec, LibraryProvider, McpOptions, McpServer, MetricsProvider, MetricsSource};
use omuzaki_ops_api::{ApprovalId, Decision, EndReason, Grants, JobState, OpError, Scope, Sessions, TxId};
use omuzaki_store_api::{Commercial, ContentHash};
use rmcp::model::ClientCapabilities;
use serde_json::{Map, Value, json};

fn registry_server(studio: &Arc<Studio>) -> McpServer {
    let options = McpOptions::default()
        .with_sessions(studio.clone(), Studio::person())
        .with_pause_hold(Duration::from_millis(100));
    McpServer::new(studio.clone(), options).unwrap()
}

fn tasks_capability() -> Value {
    serde_json::to_value(ClientCapabilities::builder().enable_tasks().build()).unwrap()
}

#[tokio::test]
async fn session_info_and_session_list() {
    let studio = Studio::new();
    let mcp = registry_server(&studio);
    let mut me = Wire::open(&mcp, "claude-code", json!({})).await;
    let _other = Wire::open(&mcp, "codex", json!({})).await;
    me.call("session_info", json!({})).await;
    me.call("session_list", json!({})).await;
    check_transcript(
        "session-tools",
        &me.transcript(
            "session_info and session_list",
            "Two clients are connected. The first asks for its own record, then for every live session.",
        ),
    );
}

#[tokio::test]
async fn approvals_and_approval_wait() {
    let studio = Studio::new();
    let mcp = registry_server(&studio);
    let mut wire = Wire::open(&mcp, "claude-code", json!({})).await;
    // The agent lacks `publish`, which the person holds: the export is queued for them.
    wire.call("export_vrm", json!({"character": "mira"})).await;
    wire.call("approval_wait", json!({"id": 1, "timeout_seconds": 1})).await;
    // The person rejects it in Omuzaki's window (not an MCP message); the agent hears of it in the next result.
    studio.resolve(ApprovalId(1), Decision::Reject { comment: Some("not yet".to_owned()) }, &Studio::person()).unwrap();
    for _ in 0..200 {
        if mcp_log_has(&mcp, 3).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    wire.call("project_ping", json!({})).await;
    wire.call("approval_wait", json!({"id": 1})).await;
    check_transcript(
        "approvals",
        &wire.transcript(
            "approvals: waiting, approval_wait, and the outcome in the heads-up",
            "A request that needs the person's approval comes back `waiting`; `approval_wait` times out while it is \
             still pending; once the person has rejected it, the next result's heads-up says so, once, and \
             `approval_wait` still returns the outcome with the person's comment.",
        ),
    );
}

async fn mcp_log_has(mcp: &McpServer, n: u64) -> bool {
    tokio::time::timeout(Duration::from_millis(5), mcp.wait_for_log(n)).await.is_ok()
}

#[tokio::test]
async fn pause_and_fences() {
    let studio = Studio::with_document(json!({"characters": {"mira": {"face": {}}, "noor": {"face": {}}}}));
    let mcp = registry_server(&studio);
    let mut a = Wire::open(&mcp, "claude-code", json!({})).await;
    let _b = Wire::open(&mcp, "codex", json!({})).await;
    let sessions = Sessions::sessions(studio.as_ref());
    studio.fence(&sessions[0].actor, &["characters/mira"]);
    studio.fence(&sessions[1].actor, &["characters/noor"]);
    a.call("face_set_blend", json!({"character": "noor", "blend": 0.5})).await;
    a.call("session_info", json!({})).await;
    studio.pause(sessions[0].id, &Studio::person()).unwrap();
    a.call("face_set_blend", json!({"character": "mira", "blend": 0.5})).await;
    check_transcript(
        "fences-and-pause",
        &a.transcript(
            "a fence refusal and a paused session",
            "The first client holds `characters/mira`, the second `characters/noor`. An edit to the second's part is \
             refused naming the holder and its fence; `session_info` shows the first's own fence; after the person \
             pauses the session, a call is held briefly and then answered `waiting`.",
        ),
    );
}

#[tokio::test]
async fn the_stale_write_guard() {
    use common::FakeRunnerExt;
    let studio = Studio::new();
    let mcp = registry_server(&studio);
    let mut wire = Wire::open(&mcp, "claude-code", json!({})).await;
    wire.call("face_set_blend", json!({"character": "mira", "blend": 0.2})).await;
    studio.person_sets_blend(0.9);
    mcp.wait_for_log(3).await;
    wire.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    wire.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert_eq!(studio.runner.blend(), Some(json!(0.4_f32)));
    check_transcript(
        "stale-write",
        &wire.transcript(
            "a person's edit between the agent's read and write",
            "The write is refused with the conflict and the heads-up that names who changed it; the retry is an informed \
             overwrite and passes.",
        ),
    );
}

#[tokio::test]
async fn long_jobs_as_tasks() {
    let fake = Fake::new();
    let grants = Grants::agent_default().with(Scope::Publish);
    let mcp = McpServer::new(fake.clone(), McpOptions::with_grants(grants)).unwrap().with_jobs(fake.clone());
    let mut wire = Wire::open(&mcp, "claude-code", tasks_capability()).await;
    // A job op returns a task handle at once.
    wire.call_with_progress("export_vrm", json!({"character": "mira"}), "export-1").await;
    wire.result("tasks/get", json!({"taskId": "job-1"})).await;
    fake.progress(1, "encoding", 0.5);
    let progress = wire.next_push().await;
    assert_eq!(progress["method"], "notifications/progress");
    fake.set_state(1, JobState::Running);
    wire.next_push().await;
    let done = fake.runner_outcome("mira");
    fake.set_state(1, JobState::done(TxId(4), Some(9)).with_outcome(Arc::new(done)));
    wire.next_push().await;
    wire.result("tasks/get", json!({"taskId": "job-1"})).await;
    // A second job is cancelled.
    wire.call("export_vrm", json!({"character": "mira"})).await;
    wire.result("tasks/cancel", json!({"taskId": "job-2"})).await;
    wire.result("tasks/get", json!({"taskId": "job-2"})).await;
    check_transcript(
        "tasks",
        &wire.transcript(
            "long jobs as MCP tasks",
            "A client that declares the tasks capability gets a task handle for a job op, progress notifications, a task \
             status push when the job changes, the finished task's result (the same result the blocking call returns), \
             and `tasks/cancel`.",
        ),
    );
}

/// The App's numbers, fixed.
#[derive(Debug)]
struct FixedMetrics;

impl MetricsSource for FixedMetrics {
    fn snapshot(&self) -> Result<Map<String, Value>, OpError> {
        Ok(json!({"fps": 59.9, "frame_ms": 6.1, "latency_ms": 41.0}).as_object().unwrap().clone())
    }
}

const TWINTAILS: &str = r#"
schema = 1
id = "omuzaki/hair/twintails"
version = "1.0.0"
section = "hair"
kind = "part"
target = "character"
slot = "hairstyle"
title = "Twintails"
summary = "Two high tails tied above the ears, with a light fringe."
tags = ["hair", "twintails"]
origin = "generator defaults, design target 3"
compat = { kinds = ["humanoid"], templates = ["*"], requires = ["head"] }
creates.hair = { kind = "hair", id = "hair", layer = 5 }

[rights]
author = "Omuzaki"
licence = "CC0-1.0"
commercial = "ok"
owned = true

[[step]]
op = "part.add"
[step.args]
character = "$character"
kind = "hair"
id = "$parts.hair"
layer = 5
"#;

struct OneItem;

impl Library for OneItem {
    fn list(&self, _: Option<Section>, _: Option<&ItemScope>, _: usize, _: usize) -> Result<SearchPage, LibraryError> {
        Ok(SearchPage::default())
    }
    fn search(&self, _: &Query) -> Result<SearchPage, LibraryError> {
        Ok(SearchPage::default())
    }
    fn describe(&self, item: &ItemId, _: Option<&Version>) -> Result<ItemDetail, LibraryError> {
        if item.as_str() != "omuzaki/hair/twintails" {
            return Err(LibraryError::NotFound(item.as_str().to_owned()));
        }
        let manifest = ItemManifest::parse(TWINTAILS).unwrap();
        let hash = manifest.hash();
        let info = ItemInfo {
            id: item.clone(),
            version: Version::parse("1.0.0").unwrap(),
            hash: hash.clone(),
            title: manifest.title.clone(),
            summary: manifest.summary.clone(),
            section: Section::Hair,
            kind: ItemKind::Part,
            tags: vec!["hair".to_owned()],
            scope: ItemScope::FirstParty,
            rights: RightsBadge {
                licence: "CC0-1.0".to_owned(),
                commercial: Commercial::Ok,
                owned: true,
                private_test: false,
                publish_ok: true,
            },
            compat: CompatStatus::Compatible,
            preview: Some(ContentHash::of(b"preview")),
            unavailable: None,
        };
        Ok(ItemDetail {
            info,
            item: ResolvedItem { manifest, hash, scope: ItemScope::FirstParty, publisher: PublisherCheck::FirstParty },
            versions: vec![Version::parse("1.0.0").unwrap()],
        })
    }
    fn resolve(&self, _: &ItemId, _: &VersionReq) -> Result<ResolvedItem, LibraryError> {
        Err(LibraryError::Unsupported { feature: "resolve" })
    }
    fn by_hash(&self, _: &ContentHash) -> Result<ResolvedItem, LibraryError> {
        Err(LibraryError::Unsupported { feature: "by_hash" })
    }
    fn versions(&self, _: &ItemId) -> Result<Vec<Version>, LibraryError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn resources() {
    let options = McpOptions::default()
        .with_provider(Arc::new(MetricsProvider::new(Arc::new(FixedMetrics))))
        .with_provider(Arc::new(LibraryProvider::new(Arc::new(OneItem))));
    let mcp = McpServer::new(Arc::new(common::fake_runner()), options).unwrap();
    let mut wire = Wire::open(&mcp, "claude-code", json!({})).await;
    wire.result("resources/list", json!({})).await;
    wire.result("resources/templates/list", json!({})).await;
    wire.result("resources/read", json!({"uri": "omuzaki://metrics/live"})).await;
    wire.result("resources/read", json!({"uri": "omuzaki://library/omuzaki/hair/twintails"})).await;
    wire.request("resources/read", json!({"uri": "omuzaki://library/omuzaki/hair/ghost"})).await;
    check_transcript(
        "resources",
        &wire.transcript(
            "the live metrics and a library item",
            "The App registers a metrics source and a library; the resources are listed (the library by template only), \
             read as JSON, and an item that does not exist is `resource_not_found`.",
        ),
    );
}

#[tokio::test]
async fn in_app_link() {
    let studio = Studio::new();
    let mcp = registry_server(&studio);
    let link =
        mcp.connect_in_app(InAppSpec::assistant(Studio::person(), Some("claude-opus-5-5".to_owned()))).await.unwrap();
    let session = link.session();
    let end = link.end();
    let mut wire = Wire::over(link.client_io, "assistant-ui", json!({})).await;
    wire.call("session_info", json!({})).await;
    wire.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    studio.registry_ends(session, EndReason::Revoked { by: Studio::person(), credential: false });
    assert!(matches!(end.closed().await, EndReason::Revoked { .. }));
    check_transcript(
        "in-app",
        &wire.transcript(
            "the in-process link",
            "The host opens the assistant's session with `connect_in_app` and runs a client on the returned stream. The \
             session is `in_app`, acts for the person, and edits in its own name; when the person revokes it the stream \
             ends.",
        ),
    );
}
