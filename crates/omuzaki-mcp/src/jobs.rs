//! Long jobs as MCP tasks (SEP-2663): a job op called by a client that declared the tasks extension starts through
//! `Jobs::start` and answers with a task handle. `tasks/get` reads the job, `tasks/cancel` cancels it, and a watcher
//! forwards the job's events as progress and task-status notifications.
//!
//! The task id is the job id (`job-7`), so the executor stays the only authority on what exists and who may see it:
//! there is no task table here to go stale.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_history_api::JournalSink;
use omuzaki_ops_api::{Event, Grants, JobId, JobInfo, JobState, Jobs, OpError};
use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolResult, DetailedTask, JsonObject, Notification, ProgressNotificationParam, ProgressToken,
    ServerNotification, Task, TaskPayload, TaskStatus, TaskStatusNotificationParams,
};
use rmcp::service::{Peer, RoleServer};
use serde_json::Value;

use crate::events::EventHub;
use crate::floats;
use crate::render;
use crate::session::{PushCursor, Reporter};

/// How long a client should wait between `tasks/get` polls, in milliseconds.
const POLL_INTERVAL_MS: u64 = 1000;

/// The prefix of a task id; the rest is the job number.
const TASK_PREFIX: &str = "job-";

/// The id of the task that stands for `job`.
pub(crate) fn task_id(job: JobId) -> String {
    format!("{TASK_PREFIX}{}", job.0)
}

/// The job a task id stands for.
///
/// # Errors
/// An `invalid params` protocol error if `id` is not one this server hands out.
pub(crate) fn job_of(id: &str) -> Result<JobId, McpError> {
    id.strip_prefix(TASK_PREFIX).and_then(|n| n.parse().ok()).map(JobId).ok_or_else(|| {
        McpError::invalid_params(format!("unknown task `{id}`: task ids look like `{TASK_PREFIX}7`"), None)
    })
}

/// The one place a job becomes a task: the status, with the job's result once it has one.
///
/// A job that failed is a *completed* task whose result is a tool error, as for any tool call (`failed` is for
/// protocol errors); a job that was cancelled is a cancelled task.
///
/// `created` is when the task was created (SEP-2663), which this server noted when it handed the handle out; the last
/// update is the time of this reply.
fn task_of(info: &JobInfo, tool: &str, verify: Option<&str>, created: &str, unsaved: Option<&str>) -> DetailedTask {
    let now = iso8601(SystemTime::now());
    let base = Task::new(task_id(info.id), TaskStatus::Working, created.to_owned(), now)
        .with_poll_interval_ms(POLL_INTERVAL_MS);
    let (message, payload) = match &info.state {
        JobState::Queued => (Some("waiting for a worker".to_owned()), TaskPayload::Working),
        JobState::Running => (info.stage.clone(), TaskPayload::Working),
        JobState::Held => {
            (Some("the work is done; the change lands when the session resumes".to_owned()), TaskPayload::Working)
        }
        JobState::Cancelled { .. } => (None, TaskPayload::Cancelled),
        state => {
            let result = match state {
                JobState::Done { .. } => {
                    let done = render::job_done(tool, info, verify);
                    match unsaved {
                        Some(warning) => render::with_warning(done, warning),
                        None => done,
                    }
                }
                JobState::Failed { error } => render::failure(tool, error, None),
                _ => render::failure(tool, &interrupted(), None),
            };
            (None, TaskPayload::Completed { result: object(&result) })
        }
    };
    let task = match message {
        Some(message) => base.with_status_message(message),
        None => base,
    };
    DetailedTask::new(task, payload)
}

/// [`task_of`] with the durability handling a blocking call gets: a job that applied a change is not reported done until
/// the journal has saved it, and if saving fails or times out the result leads with "applied, not yet saved". Every
/// place a task is reported (`tasks/get`, the handle, status pushes) goes through here.
pub(crate) async fn task_saved(
    journal: Option<Arc<dyn JournalSink>>,
    reporter: Option<&Reporter>,
    push: Option<&mut PushCursor>,
    info: &JobInfo,
    tool: &str,
    verify: Option<&str>,
    created: &str,
) -> DetailedTask {
    let applied = matches!(info.state, JobState::Done { seq: Some(_), .. });
    let unsaved = if applied { crate::session::unsaved_warning(journal).await } else { None };
    let Some(reporter) = reporter else { return task_of(info, tool, verify, created, unsaved.as_deref()) };
    let mut task = task_of(info, tool, verify, created, None);
    // A finished task's result gets what a blocking result gets: the heads-up about others' changes, the decisions on
    // this session's approvals, and the save warning in front.
    if let TaskPayload::Completed { result } = &task.payload
        && let Ok(call) = serde_json::from_value::<CallToolResult>(Value::Object(result.clone()))
    {
        let call = reporter.finish(call, unsaved, false, push).await;
        task.payload = TaskPayload::Completed { result: object(&call) };
    }
    task
}

/// What a job that was running at a crash reports: jobs are never re-run.
fn interrupted() -> OpError {
    OpError::internal("the job was interrupted before it finished and was not re-run")
        .hint("start it again if you still want it")
}

/// A tool result as the JSON object a completed task carries.
fn object(result: &rmcp::model::CallToolResult) -> JsonObject {
    match serde_json::to_value(result) {
        Ok(Value::Object(object)) => object,
        // A tool result always serialises to an object; an empty one says nothing wrong.
        _ => JsonObject::new(),
    }
}

/// `time` as an ISO 8601 UTC timestamp. The executor's job times are on a monotonic clock, so a task carries the wall
/// time of the reply instead.
fn iso8601(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Hinnant's algorithm), for days after the epoch.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// When each task was created, noted at the moment its handle is handed out. A task this server didn't create (the
/// process restarted since) reads as created now: the executor's own times are on a monotonic clock, not wall time.
#[derive(Default)]
pub(crate) struct TaskTimes(std::sync::Mutex<std::collections::BTreeMap<u64, String>>);

/// How many creation times are kept; the oldest go first.
const KEPT_TIMES: usize = 1024;

impl TaskTimes {
    /// The creation time of `job`, noting now as it if this is the first time it is asked for.
    pub(crate) fn of(&self, job: JobId) -> String {
        let mut times = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let created = times.entry(job.0).or_insert_with(|| iso8601(SystemTime::now())).clone();
        while times.len() > KEPT_TIMES {
            times.pop_first();
        }
        created
    }
}

/// What a watcher needs to follow one job and talk to its client.
pub(crate) struct JobWatch {
    pub hub: Arc<EventHub>,
    /// The host's journal, which a finished job's result waits for.
    pub journal: Option<Arc<dyn JournalSink>>,
    /// The executor's job manager, the authority a watcher re-reads when events have left the hub's window.
    pub jobs: Arc<dyn Jobs>,
    /// The session's result processing, for the results the pushes carry.
    pub reporter: Option<Reporter>,
    /// The hub's position from before the job started, so no event of the job can fall in the gap.
    pub after: u64,
    /// Where the pushes are up to in telling the client about others' changes and approval decisions: a copy of the
    /// session's cursors from when the task began, and then the watcher's own.
    pub push: PushCursor,
    pub peer: Peer<RoleServer>,
    pub token: Option<ProgressToken>,
    pub job: JobInfo,
    pub tool: String,
    /// The "check afterwards" advice the blocking result carries.
    pub verify: Option<String>,
    /// When the task was created, for every status push.
    pub created: String,
}

impl JobWatch {
    /// Forwards the job's events until it ends or the client goes away. The hub coalesces a burst into one wake, so a
    /// fast job sends the newest progress and status, not every step. If events fell out of the hub's window unseen
    /// the watcher re-reads the job from the executor (see `events`); `tasks/get` always reads the executor.
    pub(crate) async fn run(self) {
        let Self {
            hub,
            journal,
            jobs,
            reporter,
            mut after,
            push: mut cursor,
            peer,
            token,
            mut job,
            tool,
            verify,
            created,
        } = self;
        let mut arrivals = hub.watch();
        let mut sent = job.fraction;
        // `cursor` is this watcher's own place in the feed: reporting a push never moves the session's (see `events`).
        // Events are read before each wait, not after it: a job that finished before this watcher subscribed (inside
        // `Jobs::start`, say) has its events buffered already, and a watch only wakes for what arrives after it.
        loop {
            if peer.is_transport_closed() {
                return;
            }
            let mut progress = None;
            let mut changed = false;
            if hub.lost_after(after) {
                // Events left the window before they were read; the job may have ended in them. The executor knows
                // where it is now, and everything in the window is older than that, so it is skipped.
                let latest = hub.latest();
                let grants = reporter.as_ref().and_then(Reporter::grants);
                if let Some(grants) = grants
                    && let Ok(now) = read_job(Arc::clone(&jobs), job.id, grants).await
                {
                    (job, changed) = (now, true);
                }
                after = latest;
            }
            for (n, event) in hub.entries_after(after) {
                after = n;
                match event {
                    Event::JobChanged { job: now, .. } if now.id == job.id => (job, changed) = (now, true),
                    Event::JobProgress { job: id, stage, fraction, .. } if id == job.id => {
                        progress = Some((stage, fraction));
                    }
                    _ => {}
                }
            }
            if let (Some(token), Some((stage, fraction))) = (&token, progress)
                && fraction > sent
            {
                sent = fraction;
                let param = ProgressNotificationParam::new(token.clone(), floats::widened(fraction))
                    .with_total(1.0)
                    .with_message(stage);
                if peer.notify_progress(param).await.is_err() {
                    return;
                }
            }
            if changed {
                let task = task_saved(
                    journal.clone(),
                    reporter.as_ref(),
                    Some(&mut cursor),
                    &job,
                    &tool,
                    verify.as_deref(),
                    &created,
                )
                .await;
                let status = Notification::new(TaskStatusNotificationParams::new(task));
                if peer.send_notification(ServerNotification::TaskStatusNotification(status)).await.is_err() {
                    return;
                }
            }
            if ended(&job) || arrivals.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Whether the job is past changing.
fn ended(job: &JobInfo) -> bool {
    !matches!(job.state, JobState::Queued | JobState::Running | JobState::Held)
}

/// Runs blocking executor work off the async threads, turning a panic into an `internal` error instead of a dead
/// session.
pub(crate) async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, OpError> {
    tokio::task::spawn_blocking(work).await.map_err(|e| {
        OpError::internal(format!("the operation panicked or was cancelled: {e}"))
            .hint("this is a bug in Omuzaki: tell the user, and don't retry unchanged")
    })
}

/// `Jobs::job`, off the async threads.
///
/// # Errors
/// `NotFound` for an unknown job, or the executor's refusal (it needs `read`).
pub(crate) async fn read_job(jobs: Arc<dyn Jobs>, id: JobId, grants: Grants) -> Result<JobInfo, OpError> {
    off_thread(move || jobs.job(id, &grants))
        .await??
        .ok_or_else(|| OpError::not_found(format!("no task `{}`", task_id(id))))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn task_ids_round_trip_and_reject_strangers() {
        assert_eq!(job_of(&task_id(JobId(7))).unwrap(), JobId(7));
        for bad in ["7", "job-", "job-x", "task-7", ""] {
            assert!(job_of(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn timestamps_are_iso_8601_utc() {
        let at = |secs| iso8601(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_782_400 + 86_399), "2000-02-29T23:59:59Z");
        assert_eq!(at(1_784_000_000), "2026-07-14T03:33:20Z");
    }
}
