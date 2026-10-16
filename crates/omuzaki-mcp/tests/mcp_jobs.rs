//! Job ops as MCP tasks (SEP-2663), against a fake job manager and a fake runner: a client that declares the tasks
//! extension gets a task handle, progress and status pushes, `tasks/get` and `tasks/cancel`; one that does not keeps
//! the blocking call.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::jobs::Fake;
use common::{data, text};
use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::{Actor, Grants, JobId, JobState, Jobs, OpError, Request, Runner, TxId};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities, ClientConfig, GetTaskParams,
    Implementation, ProgressNotificationParam, TaskPayload, TaskStatus, TaskStatusNotificationParams,
};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// A client that keeps the progress and task-status notifications it is sent.
struct TestClient {
    tasks: bool,
    progress: mpsc::UnboundedSender<ProgressNotificationParam>,
    status: mpsc::UnboundedSender<TaskStatusNotificationParams>,
}

impl ClientHandler for TestClient {
    fn get_info(&self) -> ClientConfig {
        let caps = if self.tasks {
            ClientCapabilities::builder().enable_tasks().build()
        } else {
            ClientCapabilities::default()
        };
        ClientConfig::new(caps, Implementation::new("test-client", "1.0"))
    }

    async fn on_progress(&self, params: ProgressNotificationParam, _context: NotificationContext<RoleClient>) {
        let _ = self.progress.send(params);
    }

    async fn on_task_status(&self, params: TaskStatusNotificationParams, _context: NotificationContext<RoleClient>) {
        let _ = self.status.send(params);
    }
}

struct Client {
    service: RunningService<RoleClient, TestClient>,
    progress: mpsc::UnboundedReceiver<ProgressNotificationParam>,
    status: mpsc::UnboundedReceiver<TaskStatusNotificationParams>,
}

impl Client {
    async fn connect(mcp: &McpServer, tasks: bool) -> Self {
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let serving = mcp.clone();
        tokio::spawn(async move { serving.serve(server_io).await });
        let (progress_tx, progress) = mpsc::unbounded_channel();
        let (status_tx, status) = mpsc::unbounded_channel();
        let service = TestClient { tasks, progress: progress_tx, status: status_tx }
            .serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize)
            .await
            .expect("client connects");
        Self { service, progress, status }
    }

    /// Calls `export_vrm` for mira; the client attaches the progress token itself.
    async fn export(&self) -> CallToolResponse {
        let params = CallToolRequestParams::new("export_vrm")
            .with_arguments(json!({"character": "mira"}).as_object().expect("object").clone());
        self.service.call_tool_once(params).await.expect("call")
    }

    async fn task(&self, id: &str) -> rmcp::model::DetailedTask {
        self.service.get_task(GetTaskParams::new(id)).await.expect("tasks/get").task
    }

    async fn next_progress(&mut self) -> ProgressNotificationParam {
        tokio::time::timeout(Duration::from_secs(5), self.progress.recv()).await.expect("in time").expect("open")
    }

    async fn next_status(&mut self) -> TaskStatusNotificationParams {
        tokio::time::timeout(Duration::from_secs(5), self.status.recv()).await.expect("in time").expect("open")
    }
}

fn server(fake: &Arc<Fake>) -> McpServer {
    let grants = Grants::agent_default().with(omuzaki_ops_api::Scope::Publish);
    McpServer::new(fake.clone(), McpOptions::with_grants(grants)).expect("server builds").with_jobs(fake.clone())
}

fn done() -> JobState {
    JobState::done(TxId(4), Some(9))
}

fn task_id(response: CallToolResponse) -> String {
    match response {
        CallToolResponse::Task(created) => created.task.task_id,
        other => panic!("expected a task, got {other:?}"),
    }
}

#[tokio::test]
async fn a_job_call_returns_a_task_that_reports_progress_running_and_completed() {
    let fake = Fake::new();
    let mut client = Client::connect(&server(&fake), true).await;
    let id = task_id(client.export().await);
    assert_eq!(id, "job-1");
    assert_eq!(client.task(&id).await.status(), TaskStatus::Working);

    fake.progress(1, "encoding", 0.5);
    let progress = client.next_progress().await;
    assert_eq!((progress.progress, progress.message.as_deref()), (0.5, Some("encoding")));

    fake.set_state(1, JobState::Running);
    assert_eq!(client.next_status().await.task.status(), TaskStatus::Working);
    fake.set_state(1, done());
    let finished = client.next_status().await.task;
    assert_eq!(finished.status(), TaskStatus::Completed);
    let TaskPayload::Completed { result } = client.task(&id).await.payload else { panic!("completed") };
    assert_eq!(result["structuredContent"]["ok"], true);
    assert_eq!(result["structuredContent"]["log"], 9);
    assert_eq!(*fake.started.lock().unwrap(), 1);
}

#[tokio::test]
async fn tasks_cancel_cancels_the_job_and_the_task_ends_cancelled() {
    let fake = Fake::new();
    let mut client = Client::connect(&server(&fake), true).await;
    let id = task_id(client.export().await);
    client.service.cancel_task(CancelTaskParams::new(&id)).await.expect("cancel");
    assert!(matches!(fake.job(JobId(1), &Grants::person()).unwrap().unwrap().state, JobState::Cancelled { .. }));
    assert_eq!(client.next_status().await.task.status(), TaskStatus::Cancelled);
    assert_eq!(client.task(&id).await.status(), TaskStatus::Cancelled);
}

#[tokio::test]
async fn a_failed_job_is_a_tool_error_and_cancelled_carries_its_hint() {
    let fake = Fake::new();
    let client = Client::connect(&server(&fake), true).await;
    let id = task_id(client.export().await);
    fake.set_state(1, JobState::Failed { error: OpError::cancelled(JobId(1)) });
    let TaskPayload::Completed { result } = client.task(&id).await.payload else { panic!("completed") };
    assert_eq!(result["isError"], true);
    let body = &result["structuredContent"];
    assert_eq!((body["ok"].clone(), body["code"].clone()), (json!(false), json!("cancelled")));
    assert_eq!(body["hint"], "the job was cancelled; start it again if you still want it");
}

#[tokio::test]
async fn a_client_without_tasks_still_gets_a_blocking_result() {
    let fake = Fake::new();
    let client = Client::connect(&server(&fake), false).await;
    let args = json!({"character": "mira"}).as_object().unwrap().clone();
    let result =
        client.service.call_tool(CallToolRequestParams::new("export_vrm").with_arguments(args)).await.expect("call");
    assert_eq!(data(&result)["ok"], true);
    assert_eq!(*fake.started.lock().unwrap(), 0, "no job was started: the runner waited for the op");
}

#[tokio::test]
async fn another_sessions_cancel_gets_the_executors_refusal_unchanged() {
    let fake = Fake::new();
    let mcp = server(&fake);
    let owner = Client::connect(&mcp, true).await;
    let other = Client::connect(&mcp, true).await;
    let id = task_id(owner.export().await);
    let refused = other.service.cancel_task(CancelTaskParams::new(&id)).await.expect_err("refused");
    assert!(refused.to_string().contains("only the session that started a job may cancel it"), "{refused}");
    assert_eq!(fake.job(JobId(1), &Grants::person()).unwrap().unwrap().state, JobState::Queued);
}

#[tokio::test]
async fn another_sessions_tasks_get_is_not_found_and_leaks_nothing() {
    let fake = Fake::new();
    let mcp = server(&fake);
    let owner = Client::connect(&mcp, true).await;
    let other = Client::connect(&mcp, true).await;
    let id = task_id(owner.export().await);
    fake.set_state(1, done());
    let refused = other.service.get_task(GetTaskParams::new(&id)).await.expect_err("another session's task is hidden");
    assert!(refused.to_string().contains("no task"), "{refused}");
    // The owner still reads it.
    assert_eq!(owner.task(&id).await.status(), TaskStatus::Completed);
}

#[tokio::test]
async fn job_events_are_not_reported_as_document_changes() {
    let fake = Fake::new();
    let mcp = server(&fake);
    let owner = Client::connect(&mcp, true).await;
    let watcher = Client::connect(&mcp, false).await;
    owner.export().await;
    fake.progress(1, "encoding", 0.5);
    fake.set_state(1, done());
    mcp.wait_for_log(2).await;
    let result = watcher.service.call_tool(CallToolRequestParams::new("project_ping")).await.expect("call");
    assert!(data(&result).get("since_your_last_call").is_none(), "{}", text(&result));
}

#[tokio::test]
async fn a_finished_task_renders_the_same_result_as_the_blocking_call() {
    // Separate runners, so the log numbers of the two results start from the same place.
    let (fake, waiting, executor) = (Fake::new(), Fake::new(), Fake::new());
    let tasks = Client::connect(&server(&fake), true).await;
    let blocking = Client::connect(&server(&waiting), false).await;
    let args = json!({"character": "mira"}).as_object().unwrap().clone();
    let waited = blocking
        .service
        .call_tool(CallToolRequestParams::new("export_vrm").with_arguments(args.clone()))
        .await
        .expect("call");

    // What the executor keeps when the job ends: the outcome of the same op with the same arguments.
    let request = Request {
        op: "export.vrm".to_owned(),
        args: Value::Object(args),
        mode: omuzaki_ops_api::Mode::Apply,
        actor: Actor::Agent { id: "test-client#1".to_owned(), model: None },
        grants: Grants::agent_default().with(omuzaki_ops_api::Scope::Publish),
        cause: None,
        tx: None,
        base: None,
    };
    let outcome = executor.run(request).expect("the fake runs the op");
    let id = task_id(tasks.export().await);
    fake.set_state(1, done().with_outcome(Arc::new(outcome)));

    let TaskPayload::Completed { result } = tasks.task(&id).await.payload else { panic!("completed") };
    let waited = serde_json::to_value(&waited).unwrap();
    assert_eq!(result["structuredContent"], waited["structuredContent"]);
    assert_eq!(result["content"], waited["content"]);
}

#[tokio::test]
async fn a_job_that_finishes_inside_start_still_pushes_its_completion() {
    let fake = Fake::new();
    *fake.finish_in_start.lock().unwrap() = Some(done());
    let mut client = Client::connect(&server(&fake), true).await;
    let id = task_id(client.export().await);
    // The announcement was in the feed before the watcher began; no later event will wake it.
    assert_eq!(client.next_status().await.task.status(), TaskStatus::Completed);
    assert_eq!(client.task(&id).await.status(), TaskStatus::Completed);
}

#[tokio::test]
async fn a_completion_pushed_out_of_the_event_window_is_still_reported() {
    let fake = Fake::new();
    *fake.finish_in_start.lock().unwrap() = Some(done());
    // More events than the hub keeps (256) follow the completion before the watcher looks.
    *fake.flood_in_start.lock().unwrap() = 400;
    let mut client = Client::connect(&server(&fake), true).await;
    let id = task_id(client.export().await);
    assert_eq!(client.next_status().await.task.status(), TaskStatus::Completed);
    assert_eq!(client.task(&id).await.status(), TaskStatus::Completed);
}
