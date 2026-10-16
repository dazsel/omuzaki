//! Agent sessions get the project folder plus the roots the person shared, and nothing else (ADR 0013): a file in the
//! project or in a shared folder is reachable, one in a folder the person merely picked is refused (no granted root covers it). The same
//! holds for the in-app session, and the roots are the folder's real path even when it was named through a link.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "a test sets up folders on disk; the fence is for ops")]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::registry::Studio;
use common::wire::{Wire, check_transcript};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_mcp::{InAppSpec, McpOptions, McpServer};
use omuzaki_ops_api::{Actor, FsMode, Grants, OpCtx, ProjectDir, Sessions};
use serde_json::json;

struct Folders {
    _root: tempfile::TempDir,
    project: PathBuf,
    shared: PathBuf,
    picked: PathBuf,
}

fn folders() -> Folders {
    let root = tempfile::tempdir().expect("temp dir");
    let base = std::fs::canonicalize(root.path()).expect("real path");
    let make = |name: &str| {
        let dir = base.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("model.glb"), b"glb").unwrap();
        dir
    };
    let (project, shared, picked) = (make("project"), make("shared"), make("picked"));
    Folders { _root: root, project, shared, picked }
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

/// Whether an op running with `grants` in `project` may read `file`, and the refusal if not.
fn may_read(grants: &Grants, project: &Path, file: &Path) -> Result<(), String> {
    let (doc, clock, actor) = (MemDoc::from_value(json!({})), FakeClock::new(Seconds(0.0)), Actor::System);
    let dir = ProjectDir::open(project).unwrap();
    let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(grants).with_project_dir(&dir);
    ctx.fs_access(&text(file), FsMode::Read).map(drop).map_err(|e| e.message)
}

fn server(studio: &Arc<Studio>, folders: &Folders) -> McpServer {
    let options = McpOptions::default()
        .with_sessions(studio.clone(), Studio::person())
        .with_roots(text(&folders.project), [text(&folders.shared)]);
    McpServer::new(studio.clone(), options).unwrap()
}

fn assert_reach(grants: &Grants, folders: &Folders) {
    assert_eq!(may_read(grants, &folders.project, &folders.project.join("model.glb")), Ok(()), "the project");
    assert_eq!(may_read(grants, &folders.project, &folders.shared.join("model.glb")), Ok(()), "a shared root");
    let refused = may_read(grants, &folders.project, &folders.picked.join("model.glb")).unwrap_err();
    assert!(refused.contains("no granted file root covers it"), "a picked, unshared folder is refused: {refused}");
}

#[tokio::test]
async fn an_mcp_session_reaches_the_project_and_shared_roots_only() {
    let (studio, folders) = (Studio::new(), folders());
    let mcp = server(&studio, &folders);
    let mut wire = Wire::open(&mcp, "claude-code", json!({})).await;
    let info = wire.call("session_info", json!({})).await;
    let roots = info["structuredContent"]["session"]["grants"]["file_roots"].clone();
    assert_eq!(roots, json!([text(&folders.project), text(&folders.shared)]));
    let granted = Sessions::sessions(studio.as_ref())[0].grants.clone();
    assert_reach(&granted, &folders);

    let shown = wire.transcript(
        "agent roots: the project folder plus the folders the person shared",
        "`session_info` lists the file roots of the session: the project folder (its real path) and the shared folder, \
         and no other.",
    );
    let shown = shown.replace(&text(&folders.project), "<project>").replace(&text(&folders.shared), "<shared>");
    check_transcript("roots", &shown);
}

#[tokio::test]
async fn the_in_app_session_gets_the_same_roots() {
    let (studio, folders) = (Studio::new(), folders());
    let mcp = server(&studio, &folders);
    let link = mcp.connect_in_app(InAppSpec::assistant(Studio::person(), None)).await.unwrap();
    let granted = Sessions::session(studio.as_ref(), link.session()).unwrap().grants;
    assert_eq!(granted.fs_roots(), [text(&folders.project), text(&folders.shared)]);
    assert_reach(&granted, &folders);
}
