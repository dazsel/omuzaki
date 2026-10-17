//! Tasks with sessions and a journal on: a job started as a task goes through the same holding, approval tracking and
//! durability handling as the blocking call (design `agent-sessions.md` §4.3, §7.1; journal ADR 0021).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::jobs::Fake;
use common::registry::Studio;
use common::{data, text};
use omuzaki_core::time::Seconds;
use omuzaki_history_api::{JournalStatus, SyncError};
use omuzaki_history_mock::RecordingSink;
use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::{
    Actor, ApprovalId, Decision, Grants, JobId, JobInfo, JobState, Jobs, Lane, OpError, Request, Runner, Scope,
    SessionId, Sessions, TxId,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig, GetTaskParams,
    Implementation, TaskPayload, TaskStatus,
};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};

/// A job manager over the registry stand-in: `start` is refused exactly as the registry refuses a run (paused, rate
/// limited, queued for approval), and otherwise the job is done at once with the op's outcome.
struct Starter {
    studio: Arc<Studio>,
    jobs: Mutex<Vec<JobInfo>>,
    /// A refusal the next `start` returns instead of running.
    refuse_next: Mutex<Option<OpError>>,
    /// Jobs start queued and finish when the test says so, announced on the feed.
    hold: std::sync::atomic::AtomicBool,
    /// The job runs without the stale-write guard, as a job over a part of the document nobody else changed does.
    unguarded: std::sync::atomic::AtomicBool,
    outcomes: Mutex<Vec<omuzaki_ops_api::Outcome>>,
}

impl Starter {
    /// Finishes held job `id` and announces it, as the executor does.
    fn finish(&self, id: u64) {
        let at = usize::try_from(id).unwrap() - 1;
        let mut jobs = self.jobs.lock().unwrap();
        let outcome = self.outcomes.lock().unwrap()[at].clone();
        let job = &mut jobs[at];
        job.state = JobState::done(TxId(1), Some(1)).with_outcome(Arc::new(outcome));
        self.studio.emit(&omuzaki_ops_api::Event::job_changed(job.clone()));
    }

    fn new(studio: &Arc<Studio>) -> Arc<Self> {
        Arc::new(Self {
            studio: studio.clone(),
            jobs: Mutex::default(),
            refuse_next: Mutex::default(),
            hold: std::sync::atomic::AtomicBool::new(false),
            unguarded: std::sync::atomic::AtomicBool::new(false),
            outcomes: Mutex::default(),
        })
    }
}

impl Jobs for Starter {
    fn start(&self, mut req: Request) -> Result<JobInfo, OpError> {
        if self.unguarded.load(std::sync::atomic::Ordering::SeqCst) {
            req.base = None;
        }
        if let Some(error) = self.refuse_next.lock().unwrap().take() {
            return Err(error);
        }
        let (actor, op) = (req.actor.clone(), req.op.clone());
        let outcome = self.studio.run(req)?;
        let mut jobs = self.jobs.lock().unwrap();
        let job = JobInfo {
            id: JobId(jobs.len() as u64 + 1),
            op,
            label: "export".to_owned(),
            actor,
            on_behalf_of: None,
            session: None::<SessionId>,
            cause: None,
            lane: Lane::Gpu,
            state: if self.hold.load(std::sync::atomic::Ordering::SeqCst) {
                JobState::Queued
            } else {
                JobState::done(TxId(1), Some(1)).with_outcome(Arc::new(outcome.clone()))
            },
            stage: None,
            fraction: 1.0,
            estimate: None,
            spent_usd_micros: 0,
            queued_at: Seconds(0.0),
            started_at: None,
            ended_at: None,
        };
        self.outcomes.lock().unwrap().push(outcome);
        jobs.push(job.clone());
        Ok(job)
    }

    fn job(&self, id: JobId, _grants: &Grants) -> Result<Option<JobInfo>, OpError> {
        Ok(self.jobs.lock().unwrap().iter().find(|j| j.id == id).cloned())
    }

    fn jobs(&self, _grants: &Grants) -> Result<Vec<JobInfo>, OpError> {
        Ok(self.jobs.lock().unwrap().clone())
    }

    fn cancel(&self, _id: JobId, _by: &Actor) -> Result<JobInfo, OpError> {
        Err(OpError::unsupported("not in this test"))
    }

    fn cancel_session(&self, _session: SessionId, _by: &Actor) -> Result<Vec<JobId>, OpError> {
        Ok(Vec::new())
    }

    fn wait(&self, id: JobId, _timeout: Option<Duration>) -> Result<JobInfo, OpError> {
        self.job(id, &Grants::person())?.ok_or_else(|| OpError::not_found("no such job"))
    }
}

struct Tasks(tokio::sync::mpsc::UnboundedSender<rmcp::model::TaskStatusNotificationParams>);

impl ClientHandler for Tasks {
    async fn on_task_status(
        &self,
        params: rmcp::model::TaskStatusNotificationParams,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) {
        let _ = self.0.send(params);
    }

    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::builder().enable_tasks().build(),
            Implementation::new("claude-code", "1.0"),
        )
    }
}

struct Agent {
    client: RunningService<RoleClient, Tasks>,
    pushed: tokio::sync::mpsc::UnboundedReceiver<rmcp::model::TaskStatusNotificationParams>,
}

impl Agent {
    async fn connect(mcp: &McpServer) -> Self {
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let serving = mcp.clone();
        tokio::spawn(async move { serving.serve(server_io).await });
        let (tx, pushed) = tokio::sync::mpsc::unbounded_channel();
        let client =
            Tasks(tx).serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize).await.expect("connects");
        Self { client, pushed }
    }

    async fn export(&self) -> CallToolResponse {
        let params = CallToolRequestParams::new("export_vrm")
            .with_arguments(json!({"character": "mira"}).as_object().unwrap().clone());
        self.client.call_tool_once(params).await.expect("call")
    }

    async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let args = args.as_object().unwrap().clone();
        self.client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
    }
}

fn is_task(response: &CallToolResponse) -> bool {
    matches!(response, CallToolResponse::Task(_))
}

fn studio_server(studio: &Arc<Studio>, publish: bool) -> McpServer {
    let grants = if publish { Grants::agent_default().with(Scope::Publish) } else { Grants::agent_default() };
    let options = McpOptions::with_grants(grants).with_sessions(studio.clone(), Studio::person());
    McpServer::new(studio.clone(), options).unwrap().with_jobs(Starter::new(studio))
}

#[tokio::test]
async fn a_task_start_is_held_through_a_pause_like_a_blocking_call() {
    let studio = Studio::new();
    studio.person_holds(Grants::person());
    let agent = Agent::connect(&studio_server(&studio, true)).await;
    let id = studio.session_of(&studio.only_actor()).unwrap().id;
    studio.pause(id, &Studio::person()).unwrap();
    let resumer = {
        let studio = studio.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            studio.resume(id, &Studio::person()).unwrap();
        })
    };
    let response = agent.export().await;
    resumer.await.unwrap();
    assert!(is_task(&response), "a paused session's task start waits for the resume: {response:?}");
}

#[tokio::test]
async fn a_task_start_is_slowed_by_a_short_rate_limit_like_a_blocking_call() {
    let studio = Studio::new();
    studio.person_holds(Grants::person());
    studio.limit_next(1, 0.05);
    let agent = Agent::connect(&studio_server(&studio, true)).await;
    let response = agent.export().await;
    assert!(is_task(&response), "a short rate limit is waited out: {response:?}");
}

#[tokio::test]
async fn an_approval_resolved_before_approval_wait_is_still_known_for_a_task_start() {
    let studio = Studio::new();
    // The agent lacks `publish`, which the person holds: the start is queued for approval.
    let agent = Agent::connect(&studio_server(&studio, false)).await;
    let CallToolResponse::Complete(queued) = agent.export().await else { panic!("a refusal is a tool result") };
    assert_eq!(data(&queued)["detail"]["kind"], "queued", "{}", text(&queued));
    studio.resolve(ApprovalId(1), Decision::Approve, &Studio::person()).expect("resolves");
    let waited = agent.call("approval_wait", json!({"id": 1, "timeout_seconds": 5})).await;
    assert_eq!(waited.is_error, Some(false), "{}", text(&waited));
    assert!(text(&waited).contains("Approved by"), "{}", text(&waited));
}

#[tokio::test]
async fn a_finished_task_warns_when_the_journal_cannot_save_its_change() {
    let fake = Fake::new();
    let journal = Arc::new(RecordingSink::new());
    journal.script_sync(Some(SyncError::Failing { reason: "disk full".into() }));
    journal.script_status(JournalStatus::Failing { reason: "disk full".into() });
    let options = McpOptions::with_grants(Grants::agent_default().with(Scope::Publish)).with_journal(journal);
    let mcp = McpServer::new(fake.clone(), options).unwrap().with_jobs(fake.clone());
    let agent = Agent::connect(&mcp).await;
    let CallToolResponse::Task(created) = agent.export().await else { panic!("a task") };
    let id = created.task.task_id;
    fake.set_state(1, JobState::done(TxId(4), Some(9)));
    let mut warned = None;
    for _ in 0..100 {
        let task = agent.client.get_task(GetTaskParams::new(&id)).await.expect("tasks/get").task;
        if task.status() == TaskStatus::Completed {
            warned = Some(task.payload);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let TaskPayload::Completed { result } = warned.expect("the task completes") else { panic!("completed") };
    let first = result["content"][0]["text"].as_str().unwrap();
    assert!(first.starts_with("Warning: applied, not yet saved"), "{first}");
    assert!(result["structuredContent"]["warning"].is_string());
}

#[tokio::test]
async fn a_task_result_carries_the_document_heads_up_a_blocking_result_carries() {
    let studio = Studio::new();
    studio.person_holds(Grants::person());
    let mcp = studio_server(&studio, true);
    let agent = Agent::connect(&mcp).await;
    let CallToolResponse::Task(created) = agent.export().await else { panic!("a task") };
    // Another actor edits after the agent's last call and before it reads the task result.
    studio.person_sets_blend(0.9);
    let task = agent.client.get_task(GetTaskParams::new(&created.task.task_id)).await.expect("tasks/get").task;
    let TaskPayload::Completed { result } = task.payload else { panic!("completed") };
    assert!(result["structuredContent"]["since_your_last_call"].is_object(), "{result:?}");
}

#[tokio::test]
async fn a_task_result_carries_the_decision_on_an_approval_the_agent_was_waiting_for() {
    use omuzaki_ops_api::{ErrorCode, ErrorDetail, Need, Resolution, Resolved};
    let studio = Studio::new();
    studio.person_holds(Grants::person());
    let starter = Starter::new(&studio);
    // The first start is queued for approval #77, as the executor would.
    let mut queued = OpError::conflict("queued for a person")
        .with_detail(ErrorDetail::Queued { approval: ApprovalId(77), need: Need::Scope { scope: Scope::Publish } });
    queued.code = ErrorCode::Waiting;
    *starter.refuse_next.lock().unwrap() = Some(queued);
    let grants = Grants::agent_default().with(Scope::Publish);
    let options = McpOptions::with_grants(grants).with_sessions(studio.clone(), Studio::person());
    let mcp = McpServer::new(studio.clone(), options).unwrap().with_jobs(starter);
    let agent = Agent::connect(&mcp).await;
    let CallToolResponse::Complete(refused) = agent.export().await else { panic!("a refusal is a tool result") };
    assert_eq!(data(&refused)["detail"]["kind"], "queued", "{}", text(&refused));

    // The person decides while the agent does something else; then a task finishes.
    studio.emit(&omuzaki_ops_api::Event::approval_resolved(Resolution {
        id: ApprovalId(77),
        by: Some(Studio::person()),
        result: Resolved::Rejected { comment: Some("not now".to_owned()) },
        at: Seconds(1.0),
    }));
    let CallToolResponse::Task(created) = agent.export().await else { panic!("a task") };
    let task = agent.client.get_task(GetTaskParams::new(&created.task.task_id)).await.expect("tasks/get").task;
    let TaskPayload::Completed { result } = task.payload else { panic!("completed") };
    let first = result["content"].to_string();
    assert!(first.contains("approvals you were waiting for"), "{result:?}");
    assert_eq!(result["structuredContent"]["approvals"][0]["decision"], "rejected", "{result:?}");
}

#[tokio::test]
async fn polling_after_a_status_push_still_gets_the_full_completion() {
    let studio = Studio::new();
    studio.person_holds(Grants::person());
    let starter = Starter::new(&studio);
    starter.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let grants = Grants::agent_default().with(Scope::Publish);
    let options = McpOptions::with_grants(grants).with_sessions(studio.clone(), Studio::person());
    let mcp = McpServer::new(studio.clone(), options).unwrap().with_jobs(starter.clone());
    let mut agent = Agent::connect(&mcp).await;
    let CallToolResponse::Task(created) = agent.export().await else { panic!("a task") };
    studio.person_sets_blend(0.9);
    starter.finish(1);
    // The client gets the push and ignores it; it polls.
    let pushed = tokio::time::timeout(Duration::from_secs(5), agent.pushed.recv()).await.expect("in time").unwrap();
    assert_eq!(pushed.task.status(), TaskStatus::Completed);
    let task = agent.client.get_task(GetTaskParams::new(&created.task.task_id)).await.expect("tasks/get").task;
    let TaskPayload::Completed { result } = task.payload else { panic!("completed") };
    assert!(
        result["structuredContent"]["since_your_last_call"].is_object(),
        "the push must not have consumed the heads-up: {result:?}"
    );
}

#[tokio::test]
async fn an_approval_chained_inside_a_failed_decision_can_be_waited_on_after_it_resolves() {
    use omuzaki_ops_api::{ErrorCode, ErrorDetail, Need, Resolution, Resolved};
    let studio = Studio::new();
    studio.person_holds(Grants::person());
    let starter = Starter::new(&studio);
    let queued = |id: u64| {
        let mut error = OpError::conflict("queued for a person")
            .with_detail(ErrorDetail::Queued { approval: ApprovalId(id), need: Need::Scope { scope: Scope::Publish } });
        error.code = ErrorCode::Waiting;
        error
    };
    *starter.refuse_next.lock().unwrap() = Some(queued(77));
    let options = McpOptions::default().with_sessions(studio.clone(), Studio::person());
    let mcp = McpServer::new(studio.clone(), options).unwrap().with_jobs(starter);
    let agent = Agent::connect(&mcp).await;
    agent.export().await;
    let decide = |id: u64, result: Resolved| {
        studio.emit(&omuzaki_ops_api::Event::approval_resolved(Resolution {
            id: ApprovalId(id),
            by: Some(Studio::person()),
            result,
            at: Seconds(1.0),
        }));
    };
    // A is approved, then fails because it needs approval B; the agent is shown B.
    decide(77, Resolved::Failed { error: queued(78) });
    let first = agent.call("approval_wait", json!({"id": 77})).await;
    assert!(text(&first).contains("failed"), "{}", text(&first));
    // B is decided before the agent waits on it.
    decide(78, Resolved::Rejected { comment: None });
    let second = agent.call("approval_wait", json!({"id": 78, "timeout_seconds": 5})).await;
    assert!(text(&second).contains("The person rejected this"), "{}", text(&second));
}

#[tokio::test]
async fn a_change_and_a_decision_that_arrived_before_the_task_began_are_in_its_first_push_and_in_the_poll() {
    use omuzaki_ops_api::{ErrorCode, ErrorDetail, Need, Resolution, Resolved};
    let studio = Studio::new();
    studio.person_holds(Grants::person());
    let starter = Starter::new(&studio);
    starter.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    starter.unguarded.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut queued = OpError::conflict("queued for a person")
        .with_detail(ErrorDetail::Queued { approval: ApprovalId(77), need: Need::Scope { scope: Scope::Publish } });
    queued.code = ErrorCode::Waiting;
    *starter.refuse_next.lock().unwrap() = Some(queued);
    let grants = Grants::agent_default().with(Scope::Publish);
    let options = McpOptions::with_grants(grants).with_sessions(studio.clone(), Studio::person());
    let mcp = McpServer::new(studio.clone(), options).unwrap().with_jobs(starter.clone());
    let mut agent = Agent::connect(&mcp).await;
    let CallToolResponse::Complete(refused) = agent.export().await else { panic!("a refusal is a tool result") };
    assert_eq!(data(&refused)["detail"]["kind"], "queued", "{}", text(&refused));

    // After the agent's last result and before its next task begins: another actor edits, and the person decides.
    studio.person_sets_blend(0.9);
    studio.emit(&omuzaki_ops_api::Event::approval_resolved(Resolution {
        id: ApprovalId(77),
        by: Some(Studio::person()),
        result: Resolved::Rejected { comment: Some("not now".to_owned()) },
        at: Seconds(1.0),
    }));
    let CallToolResponse::Task(created) = agent.export().await else { panic!("a task") };
    starter.finish(1);

    let pushed = tokio::time::timeout(Duration::from_secs(5), agent.pushed.recv()).await.expect("in time").unwrap();
    assert_eq!(pushed.task.status(), TaskStatus::Completed);
    let TaskPayload::Completed { result } = pushed.task.payload else { panic!("completed push") };
    assert!(result["structuredContent"]["since_your_last_call"].is_object(), "push, edit: {result:?}");
    assert_eq!(result["structuredContent"]["approvals"][0]["decision"], "rejected", "push, decision: {result:?}");

    // The client ignored the push; the poll says the same, because the push consumed nothing of the session's.
    let task = agent.client.get_task(GetTaskParams::new(&created.task.task_id)).await.expect("tasks/get").task;
    let TaskPayload::Completed { result } = task.payload else { panic!("completed") };
    assert!(result["structuredContent"]["since_your_last_call"].is_object(), "poll, edit: {result:?}");
    assert_eq!(result["structuredContent"]["approvals"][0]["decision"], "rejected", "poll, decision: {result:?}");
}
