//! The journal (ADR 0021, #254): a reply to a change waits until the journal has saved it, and says so when it can't.
//! Queries and previews never wait; without a journal nothing changes.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{FakeRunner, FakeRunnerExt, data, fake_runner, text};
use omuzaki_history_api::{HistoryEntry, JournalSink, JournalStatus, Lsn, SyncError};
use omuzaki_history_mock::RecordingSink;
use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::OpError;
use rmcp::ClientLifecycleMode;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientServiceExt};
use serde_json::{Value, json};

/// A journal that counts its `sync` calls, and optionally holds each one until the test lets it finish.
struct Probe {
    /// Everything else a journal does, scripted by the test (`script_sync`, `script_status`).
    inner: RecordingSink,
    syncs: AtomicUsize,
    gate: Option<Gate>,
}

/// Holds `sync`: it says it has been entered on `entered`, then waits for a message on `release`.
struct Gate {
    entered: Mutex<Sender<()>>,
    release: Mutex<Receiver<()>>,
}

impl Probe {
    fn new(gate: Option<Gate>) -> Arc<Self> {
        Arc::new(Self { inner: RecordingSink::new(), syncs: AtomicUsize::new(0), gate })
    }

    /// A probe whose `sync` waits, returning the channel that announces its entry and the one that releases it.
    fn gated() -> (Arc<Self>, Receiver<()>, Sender<()>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let gate = Gate { entered: Mutex::new(entered_tx), release: Mutex::new(release_rx) };
        (Self::new(Some(gate)), entered_rx, release_tx)
    }

    fn syncs(&self) -> usize {
        self.syncs.load(Ordering::SeqCst)
    }
}

impl JournalSink for Probe {
    fn append(&self, entry: HistoryEntry) -> Lsn {
        self.inner.append(entry)
    }

    fn admit(&self) -> Result<(), OpError> {
        self.inner.admit()
    }

    fn wants_mark(&self) -> bool {
        self.inner.wants_mark()
    }

    fn sync(&self, timeout: Duration) -> Result<Lsn, SyncError> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.entered.lock().unwrap().send(()).expect("the test is listening");
            gate.release.lock().unwrap().recv().expect("the test releases the save");
        }
        self.inner.sync(timeout)
    }

    fn status(&self) -> JournalStatus {
        self.inner.status()
    }
}

/// A client that speaks the legacy `initialize` handshake, as the server tests' clients do.
struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1.0"))
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
    }
}

struct Harness {
    runner: Arc<FakeRunner>,
    client: RunningService<RoleClient, Client>,
}

impl Harness {
    async fn connect(options: McpOptions) -> Self {
        let runner = Arc::new(fake_runner());
        let mcp = McpServer::new(runner.clone(), options).expect("server builds");
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let _server = tokio::spawn(async move { mcp.serve(server_io).await });
        let client =
            Client.serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize).await.expect("client connects");
        Self { runner, client }
    }

    async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let args = args.as_object().expect("args are an object").clone();
        self.client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
    }
}

fn with(probe: &Arc<Probe>) -> McpOptions {
    McpOptions::default().with_journal(probe.clone())
}

#[tokio::test]
async fn an_applied_edit_replies_only_once_the_journal_has_saved_it() {
    let (probe, entered, release) = Probe::gated();
    let h = Harness::connect(with(&probe)).await;

    let call = h.call("face_set_blend", json!({"character": "mira", "blend": 0.4}));
    tokio::pin!(call);
    let mut saving = tokio::task::spawn_blocking(move || entered.recv());
    tokio::select! {
        _ = &mut call => panic!("replied before the journal saved the change"),
        saw = &mut saving => { saw.expect("the save ran").expect("the save was announced"); }
    }
    // The change has applied, and the reply still waits for the save.
    assert_eq!(h.runner.blend(), Some(json!(0.4_f32)));
    assert!(tokio::time::timeout(Duration::from_millis(100), &mut call).await.is_err(), "replied before the save");

    release.send(()).expect("the save is waiting");
    let result = call.await;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(data(&result)["applied"], true);
    assert!(data(&result).get("warning").is_none() && !text(&result).contains("Warning"));
    assert_eq!(probe.syncs(), 1);
}

#[tokio::test]
async fn a_save_that_times_out_still_succeeds_with_a_warning() {
    let probe = Probe::new(None);
    probe.inner.script_sync(Some(SyncError::Timeout { durable: Lsn(0) }));
    probe.inner.script_status(JournalStatus::Behind { pending: 3 });
    let h = Harness::connect(with(&probe)).await;

    let result = h.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(data(&result)["applied"], true);
    assert_eq!(h.runner.blend(), Some(json!(0.4_f32)), "the change did apply");
    let warning = "applied, not yet saved: saving is behind, with 3 entries waiting";
    assert_eq!(data(&result)["warning"], warning);
    assert!(text(&result).starts_with(&format!("Warning: {warning}")), "{}", text(&result));
}

#[tokio::test]
async fn a_failing_journal_still_succeeds_and_names_the_failure() {
    let probe = Probe::new(None);
    probe.inner.script_sync(Some(SyncError::Failing { reason: "disk full".into() }));
    probe.inner.script_status(JournalStatus::Failing { reason: "disk full".into() });
    let h = Harness::connect(with(&probe)).await;

    let result = h.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(data(&result)["applied"], true);
    assert!(text(&result).starts_with("Warning: applied, not yet saved: saving is failing: disk full"));
    assert_eq!(data(&result)["warning"], "applied, not yet saved: saving is failing: disk full");
}

#[tokio::test]
async fn queries_and_previews_never_wait_for_the_journal() {
    // A failing save would show as a warning if anything asked for one.
    let probe = Probe::new(None);
    probe.inner.script_sync(Some(SyncError::Failing { reason: "disk full".into() }));
    let h = Harness::connect(with(&probe)).await;

    let query = h.call("character_describe_face", json!({"character": "mira"})).await;
    let preview = h.call("face_set_blend", json!({"character": "mira", "blend": 0.4, "dry_run": true})).await;
    assert_eq!(query.is_error, Some(false));
    assert_eq!(data(&preview)["applied"], false);
    assert!(!text(&query).contains("Warning") && !text(&preview).contains("Warning"));
    assert_eq!(h.runner.blend(), None, "a preview must not change the project");
    assert_eq!(probe.syncs(), 0);
}

#[tokio::test]
async fn transactions_and_history_replies_wait_only_when_they_change_something() {
    let probe = Probe::new(None);
    let h = Harness::connect(with(&probe)).await;

    h.call("tx_begin", json!({"label": "group"})).await;
    assert_eq!(probe.syncs(), 0, "opening a transaction changes nothing saved");
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert_eq!(probe.syncs(), 1, "an applied edit waits for the save");
    h.call("tx_commit", json!({})).await;
    assert_eq!(probe.syncs(), 2, "a commit waits for the save");

    let undone = h.call("history_undo", json!({})).await;
    assert!(text(&undone).starts_with("Undid"), "{}", text(&undone));
    assert_eq!(probe.syncs(), 3, "an undo waits for the save");
    let redone = h.call("history_redo", json!({})).await;
    assert_eq!(data(&redone)["done"], true);
    assert_eq!(probe.syncs(), 4, "a redo waits for the save");
    h.call("history_undo", json!({})).await;
    let nothing = h.call("history_undo", json!({})).await;
    assert_eq!(text(&nothing), "Nothing to undo.");
    assert_eq!(probe.syncs(), 5, "an undo that changes nothing does not wait");

    h.call("tx_begin", json!({"label": "discard"})).await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.7})).await;
    let rolled_back = h.call("tx_rollback", json!({})).await;
    assert!(text(&rolled_back).starts_with("Rolled back"), "{}", text(&rolled_back));
    assert_eq!(probe.syncs(), 7, "an applied edit and a rollback each wait for the save");
}

#[tokio::test]
async fn without_a_journal_replies_do_not_wait_or_warn() {
    let h = Harness::connect(McpOptions::default()).await;

    let result = h.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert_eq!(data(&result)["applied"], true);
    assert_eq!(h.runner.blend(), Some(json!(0.4_f32)));
    assert!(data(&result).get("warning").is_none() && !text(&result).contains("Warning"));
}
