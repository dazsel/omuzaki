//! The host's branches as each lane's MCP server reaches them (`docs/design/branches.md` 8.1): a handle every lane gets
//! when it opens, bound to the lane manager once that exists (the folder lane opens before it does). It holds the
//! manager weakly, so a lane's server never keeps the project open.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, OnceLock, Weak};

use omuzaki_history_api::branch::{
    BranchError, BranchEvent, BranchId, BranchInfo, BranchName, Branches, Lane, NewBranch, PointId, Restored,
    ReviewDecision, Switched,
};
use omuzaki_history_api::state::StateRef;
use omuzaki_mcp::{LaneServers, McpServer};
use omuzaki_ops_api::{Actor, Grants, OpError};

use super::HostBranches;

/// The lane manager, once bound.
#[derive(Default)]
pub(crate) struct BranchesHandle {
    host: OnceLock<Weak<HostBranches>>,
}

impl BranchesHandle {
    /// Binds the handle to the lane manager; a second call does nothing.
    pub(crate) fn bind(&self, host: &Arc<HostBranches>) {
        let _ = self.host.set(Arc::downgrade(host));
    }

    fn get(&self) -> Result<Arc<HostBranches>, BranchError> {
        self.host
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| BranchError::Io("the project's branches aren't open (it is opening or closing)".to_owned()))
    }
}

impl Branches for BranchesHandle {
    fn list(&self) -> Vec<BranchInfo> {
        self.get().map(|host| host.list()).unwrap_or_default()
    }

    fn folder(&self) -> BranchId {
        self.get().map_or_else(|_| BranchId::main(), |host| host.folder())
    }

    fn create(&self, new: NewBranch, by: &Actor) -> Result<BranchInfo, BranchError> {
        self.get()?.create(new, by)
    }

    fn checkpoint(
        &self,
        branch: &BranchId,
        label: &str,
        ready: Option<String>,
        by: &Actor,
    ) -> Result<PointId, BranchError> {
        self.get()?.checkpoint(branch, label, ready, by)
    }

    fn switch(&self, to: &BranchId, by: &Actor) -> Result<Switched, BranchError> {
        self.get()?.switch(to, by)
    }

    fn settled(&self) -> Result<(), BranchError> {
        self.get()?.settled()
    }

    fn rename(
        &self,
        branch: &BranchId,
        name: BranchName,
        title: Option<String>,
        by: &Actor,
    ) -> Result<BranchInfo, BranchError> {
        self.get()?.rename(branch, name, title, by)
    }

    fn delete(&self, branch: &BranchId, by: &Actor) -> Result<(), BranchError> {
        self.get()?.delete(branch, by)
    }

    fn undelete(&self, branch: &BranchId, by: &Actor) -> Result<BranchInfo, BranchError> {
        self.get()?.undelete(branch, by)
    }

    fn review(&self, branch: &BranchId, decision: ReviewDecision, by: &Actor) -> Result<BranchInfo, BranchError> {
        self.get()?.review(branch, decision, by)
    }

    fn restore(&self, branch: &BranchId, to: StateRef, by: &Actor, grants: &Grants) -> Result<Restored, BranchError> {
        self.get()?.restore(branch, to, by, grants)
    }

    fn lane(&self, branch: &BranchId) -> Result<Lane, BranchError> {
        self.get()?.lane(branch)
    }

    fn subscribe(&self) -> Receiver<BranchEvent> {
        match self.get() {
            Ok(host) => host.subscribe(),
            // Nothing to follow: the sender goes at once, so the receiver reads as ended.
            Err(_) => channel().1,
        }
    }
}

impl LaneServers for BranchesHandle {
    fn server(&self, branch: &BranchId) -> Result<McpServer, OpError> {
        let host = self.get()?;
        let held = Branches::lane(host.as_ref(), branch)?;
        let lane = host.open_lane(branch).ok_or_else(|| OpError::not_found(format!("no open lane for {branch}")))?;
        // `held` kept the lane from closing until the server is taken; a session opened on it keeps it open from here.
        drop(held);
        Ok(lane.mcp.clone())
    }
}
