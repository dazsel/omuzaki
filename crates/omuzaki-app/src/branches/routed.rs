//! [`FolderRunner`]: the runner, session registry and job manager the person's surfaces hold. Each call goes to the
//! lane it is about: a person's to the folder lane (which a switch replaces, so a held runner follows it), an agent's
//! to the lane its session works on, a session id's to the lane that holds it. Session ids are unique across lanes
//! (one counter), so the lookup is exact.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{
    Actor, ApprovalId, Budget, Decision, EndReason, Ended, Event, Grants, JobId, JobInfo, Jobs, OpError, OpInfo,
    OpRecord, Outcome, PendingApproval, Request, Resolution, Runner, SessionId, SessionInfo, SessionSpec, SessionUndo,
    Sessions, Spend, Stopped, TxId,
};

use super::{HostBranches, OpenLane};

/// The person's view of every lane. See the [module docs](self).
#[derive(Clone)]
pub struct FolderRunner {
    branches: Arc<HostBranches>,
}

impl std::fmt::Debug for FolderRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderRunner").finish_non_exhaustive()
    }
}

impl FolderRunner {
    /// The router over `branches`' lanes.
    pub fn new(branches: Arc<HostBranches>) -> Self {
        Self { branches }
    }

    fn folder(&self) -> Arc<OpenLane> {
        self.branches.folder_lane()
    }

    fn of(&self, actor: &Actor) -> Arc<OpenLane> {
        self.branches.lane_of_actor(actor)
    }

    fn holding(&self, id: SessionId) -> Arc<OpenLane> {
        self.branches
            .lanes()
            .into_iter()
            .find(|lane| Sessions::session(lane.executor.as_ref(), id).is_some())
            .unwrap_or_else(|| self.folder())
    }
}

impl Runner for FolderRunner {
    fn ops(&self) -> Vec<OpInfo> {
        self.folder().executor.ops()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.of(&req.actor).executor.run(req)
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.of(actor).executor.begin(label, actor)
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.of(actor).executor.commit(tx, actor)
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.of(actor).executor.rollback(tx, actor)
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.of(actor).executor.undo(actor, grants)
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.of(actor).executor.redo(actor, grants)
    }

    /// The folder lane's events. After a switch ([`BranchEvent::Switched`](omuzaki_history_api::branch::BranchEvent))
    /// a surface subscribes again to follow the new folder lane.
    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.folder().executor.subscribe(grants)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.folder().executor.get(path, grants)
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.folder().executor.log(after_seq, limit, grants)
    }
}

impl Sessions for FolderRunner {
    fn open(&self, spec: SessionSpec) -> Result<SessionInfo, OpError> {
        self.folder().executor.open(spec)
    }

    /// Every lane's sessions, the folder lane's first.
    fn sessions(&self) -> Vec<SessionInfo> {
        self.branches.lanes().iter().flat_map(|lane| Sessions::sessions(lane.executor.as_ref())).collect()
    }

    fn session(&self, id: SessionId) -> Option<SessionInfo> {
        self.branches.lanes().iter().find_map(|lane| Sessions::session(lane.executor.as_ref(), id))
    }

    fn session_of(&self, actor: &Actor) -> Option<SessionInfo> {
        self.branches.lanes().iter().find_map(|lane| lane.executor.session_of(actor))
    }

    fn pause(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        self.holding(id).executor.pause(id, by)
    }

    fn resume(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        self.holding(id).executor.resume(id, by)
    }

    fn set_grants(&self, id: SessionId, grants: Grants, by: &Actor) -> Result<SessionInfo, OpError> {
        self.holding(id).executor.set_grants(id, grants, by)
    }

    fn set_budget(&self, id: SessionId, budget: Budget, by: &Actor) -> Result<SessionInfo, OpError> {
        self.holding(id).executor.set_budget(id, budget, by)
    }

    fn charge(&self, id: SessionId, spend: Spend) -> Result<SessionInfo, OpError> {
        self.holding(id).executor.charge(id, spend)
    }

    fn set_fence(&self, id: SessionId, fence: Vec<DocPath>, by: &Actor) -> Result<SessionInfo, OpError> {
        self.holding(id).executor.set_fence(id, fence, by)
    }

    fn end(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError> {
        self.holding(id).executor.end(id, reason)
    }

    fn undo_session(&self, id: SessionId, by: &Actor) -> Result<SessionUndo, OpError> {
        self.holding(id).executor.undo_session(id, by)
    }

    /// Every lane's pending approvals: the person approves an agent's request on any branch in the one window.
    fn approvals(&self) -> Vec<PendingApproval> {
        self.branches.lanes().iter().flat_map(|lane| lane.executor.approvals()).collect()
    }

    fn begin_refusal(&self, tx: TxId) -> Option<OpError> {
        self.folder().executor.begin_refusal(tx)
    }

    /// Resolves the approval on the lane it waits on: approval ids are unique across lanes (one counter).
    fn resolve(&self, id: ApprovalId, decision: Decision, by: &Actor) -> Result<Resolution, OpError> {
        let lane = self
            .branches
            .lanes()
            .into_iter()
            .find(|lane| lane.executor.approvals().iter().any(|a| a.id == id))
            .unwrap_or_else(|| self.folder());
        lane.executor.resolve(id, decision, by)
    }

    fn request_budget(&self, id: SessionId, needed: Spend) -> Result<(), OpError> {
        self.holding(id).executor.request_budget(id, needed)
    }

    fn request_pairing(&self, id: SessionId) -> Result<ApprovalId, OpError> {
        self.holding(id).executor.request_pairing(id)
    }

    fn stop(&self, id: SessionId, by: &Actor) -> Result<Stopped, OpError> {
        self.holding(id).executor.stop(id, by)
    }
}

/// Jobs run on the folder lane: job ids are numbered per lane, so the person's job manager is the folder's. An
/// agent's jobs run on its own lane through its MCP server.
impl Jobs for FolderRunner {
    fn start(&self, req: Request) -> Result<JobInfo, OpError> {
        self.folder().executor.start(req)
    }

    fn job(&self, id: JobId, grants: &Grants) -> Result<Option<JobInfo>, OpError> {
        self.folder().executor.job(id, grants)
    }

    fn jobs(&self, grants: &Grants) -> Result<Vec<JobInfo>, OpError> {
        self.folder().executor.jobs(grants)
    }

    fn cancel(&self, id: JobId, by: &Actor) -> Result<JobInfo, OpError> {
        self.folder().executor.cancel(id, by)
    }

    fn cancel_session(&self, session: SessionId, by: &Actor) -> Result<Vec<JobId>, OpError> {
        self.holding(session).executor.cancel_session(session, by)
    }

    fn wait(&self, id: JobId, timeout: Option<Duration>) -> Result<JobInfo, OpError> {
        self.folder().executor.wait(id, timeout)
    }
}
