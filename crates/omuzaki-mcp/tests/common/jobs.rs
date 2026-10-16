//! A fake job manager for the MCP tests: a runner whose jobs the test moves along by hand, announcing each change on the
//! event feed the way the executor does.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{
    Actor, CancelReason, Event, Grants, JobId, JobInfo, JobState, Jobs, Lane, OpError, OpInfo, OpRecord, Outcome,
    Request, Runner, SessionId, TxId,
};
use serde_json::Value;

use super::{FakeRunner, fake_runner};

/// The shared fake runner, plus a job manager whose jobs the test moves along by hand.
pub struct Fake {
    pub runner: FakeRunner,
    pub sinks: Mutex<Vec<Sender<Event>>>,
    pub jobs: Mutex<Vec<JobInfo>>,
    pub started: Mutex<u32>,
    /// When set, a job reaches this state inside `start`, before the call returns, as a very fast job does.
    pub finish_in_start: Mutex<Option<JobState>>,
    /// With `finish_in_start`: this many unrelated events follow the completion, enough to push it out of the hub's window.
    pub flood_in_start: Mutex<usize>,
}

impl Fake {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            runner: fake_runner(),
            sinks: Mutex::default(),
            jobs: Mutex::default(),
            started: Mutex::new(0),
            finish_in_start: Mutex::new(None),
            flood_in_start: Mutex::new(0),
        })
    }

    /// What the executor would keep for a finished `export.vrm` of `character`: the outcome of running the op.
    pub fn runner_outcome(&self, character: &str) -> Outcome {
        let request = Request {
            op: "export.vrm".to_owned(),
            args: serde_json::json!({"character": character}),
            mode: omuzaki_ops_api::Mode::Apply,
            actor: Actor::Agent { id: "outcome#0".to_owned(), model: None },
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        self.runner.run(request).expect("the fake runs the op")
    }

    pub fn emit(&self, event: &Event) {
        self.sinks.lock().unwrap().retain(|sink| sink.send(event.clone()).is_ok());
    }

    /// Moves job `id` to `state`, as the executor does, and announces it.
    pub fn set_state(&self, id: u64, state: JobState) -> JobInfo {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs.iter_mut().find(|j| j.id == JobId(id)).expect("job");
        job.state = state;
        let job = job.clone();
        drop(jobs);
        self.emit(&Event::job_changed(job.clone()));
        job
    }

    pub fn progress(&self, id: u64, stage: &str, fraction: f32) {
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
        drop(jobs);
        if let Some(state) = self.finish_in_start.lock().unwrap().clone() {
            self.set_state(job.id.0, state);
            for n in 0..*self.flood_in_start.lock().unwrap() {
                self.progress(99, "unrelated", (n % 100) as f32 / 100.0);
            }
            // Let the event feed take the announcement in, so it is buffered before the caller can watch for it.
            std::thread::sleep(Duration::from_millis(600));
        }
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
