//! Job ops as MCP tasks (SEP-2663), against a fake job manager and a fake runner: a client that declares the tasks
//! extension gets a task handle, progress and status pushes, `tasks/get` and `tasks/cancel`; one that does not keeps
//! the blocking call.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{FakeRunner, data, fake_runner, text};
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::DocPath;
use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::{
    Actor, CancelReason, Event, Grants, JobId, JobInfo, JobState, Jobs, Lane, OpError, OpInfo, OpRecord, Outcome,
    Request, Runner, SessionId, TxId,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities, ClientConfig, GetTaskParams,
    Implementation, ProgressNotificationParam, TaskPayload, TaskStatus, TaskStatusNotificationParams,
};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// The shared fake runner, plus a job manager whose jobs the test moves along by hand.
struct Fake {
    runner: FakeRunner,
    sinks: Mutex<Vec<Sender<Event>>>,
    jobs: Mutex<Vec<JobInfo>>,
    started: Mutex<u32>,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            runner: fake_runner(),
            sinks: Mutex::default(),
            jobs: Mutex::default(),
            started: Mutex::new(0),
        })
    }

    fn emit(&self, event: &Event) {
        self.sinks.lock().unwrap().retain(|sink| sink.send(event.clone()).is_ok());
    }

    /// Moves job `id` to `state`, as the executor does, and announces it.
    fn set_state(&self, id: u64, state: JobState) -> JobInfo {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs.iter_mut().find(|j| j.id == JobId(id)).expect("job");
        job.state = state;
        let job = job.clone();
        drop(jobs);
        self.emit(&Event::job_changed(job.clone()));
        job
    }

    fn progress(&self, id: u64, stage: &str, fraction: f32) {
        self.emit(&Event::job_progress(JobId(id), stage, fraction));
    }
}

impl Runner for Fake {
    fn ops(&self) -> Vec<OpInfo> {
        self.runner.ops()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.runner.run(req)
    }
    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.runner.begin(label, actor)
    }
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.runner.commit(tx, actor)
    }
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.runner.rollback(tx, actor)
    }
    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.runner.undo(actor, grants)
    }
    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.runner.redo(actor, grants)
    }
    fn subscribe(&self, _grants: &Grants) -> Result<Receiver<Event>, OpError> {
        let (tx, rx) = channel();
        self.sinks.lock().unwrap().push(tx);
        Ok(rx)
    }
    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.runner.get(path, grants)
    }
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.runner.log(after_seq, limit, grants)
    }
}

impl Jobs for Fake {
    fn start(&self, req: Request) -> Result<JobInfo, OpError> {
        let mut jobs = self.jobs.lock().unwrap();
        *self.started.lock().unwrap() += 1;
        let job = JobInfo {
            id: JobId(jobs.len() as u64 + 1),
            op: req.op,
            label: "export".to_owned(),
            actor: req.actor,
            on_behalf_of: None,
            session: None::<SessionId>,
            cause: None,
            lane: Lane::Gpu,
            state: JobState::Queued,
            stage: None,
            fraction: 0.0,
            estimate: None,
            spent_usd_micros: 0,
            queued_at: Seconds(0.0),
            started_at: None,
            ended_at: None,
        };
        jobs.push(job.clone());
        Ok(job)
    }

    fn job(&self, id: JobId, _grants: &Grants) -> Result<Option<JobInfo>, OpError> {
        Ok(self.jobs.lock().unwrap().iter().find(|j| j.id == id).cloned())
    }

    fn jobs(&self, _grants: &Grants) -> Result<Vec<JobInfo>, OpError> {
        Ok(self.jobs.lock().unwrap().clone())
    }

    fn cancel(&self, id: JobId, by: &Actor) -> Result<JobInfo, OpError> {
        let owner = self.job(id, &Grants::person())?.ok_or_else(|| OpError::not_found("no such job"))?.actor;
        if &owner != by {
            return Err(OpError::denied("only the session that started a job may cancel it"));
        }
        Ok(self.set_state(id.0, JobState::Cancelled { by: Some(by.clone()), reason: CancelReason::Requested }))
    }

    fn cancel_session(&self, _session: SessionId, _by: &Actor) -> Result<Vec<JobId>, OpError> {
        Ok(Vec::new())
    }

    fn wait(&self, id: JobId, _timeout: Option<Duration>) -> Result<JobInfo, OpError> {
        self.job(id, &Grants::person())?.ok_or_else(|| OpError::not_found("no such job"))
    }
}

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
