//! The Broadcast workspace's automation panels (`docs/design/automation.md` D10, D11, D17; `docs/design/ux/shell.md`
//! section 3): the show's rules as a table ([`rules`]), its graphs in a node editor ([`graph`]) with the selected
//! node's [`inspector`], and the running show's [`values`].
//!
//! The panels hold nothing of their own. They read the project's default show from the document and ask the engine
//! what it makes of it (`automation.describe`, `automation.status`, `automation.values`), and every change is an
//! `automation.*` or `show.*` op run as the person, so an agent building a show over MCP and a person in the studio
//! see and undo the same edits. Two things are deliberately not ops, because an op's arguments are logged and an
//! agent could call it: allowing a show's risky needs on this machine ([`ShowApprover`]) and storing a secret the show
//! sends ([`Keychain`](crate::settings::connectors::Keychain)). Both are reached only through view models the app
//! hands the shell.

pub mod graph;
pub mod inspector;
pub mod model;
pub mod needs;
pub mod registration;
pub mod rules;
pub mod text;
pub mod values;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use omuzaki_live::Automation;
use omuzaki_live::approvals::DiskApprovals;
use omuzaki_live_api::ids::{GraphId, NodeId, ShowId};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::services::ShowKey;
use omuzaki_ops_api::Actor;
use omuzaki_ui_api::panel::PanelCtx;

/// Which graph the editor shows and which node its inspector edits, shared by the Broadcast panels (the Rules table
/// opens a graph in the editor). The shell's state, not the document's: no op changes it.
#[derive(Clone, Debug, Default)]
pub struct ShowFocus(Rc<RefCell<Focus>>);

#[derive(Debug, Default)]
struct Focus {
    graph: Option<GraphId>,
    node: Option<NodeId>,
    generation: u64,
}

impl ShowFocus {
    /// The focus the shell shares as a view model, or one of the panel's own when it shares none.
    pub fn from_ctx(ctx: &PanelCtx<'_>) -> Self {
        ctx.view::<Self>().cloned().unwrap_or_default()
    }

    /// The graph the editor shows.
    pub fn graph(&self) -> Option<GraphId> {
        self.0.borrow().graph.clone()
    }

    /// The node the inspector edits.
    pub fn node(&self) -> Option<NodeId> {
        self.0.borrow().node.clone()
    }

    /// Shows `graph` in the editor, nothing selected.
    pub fn open(&self, graph: Option<GraphId>) {
        let mut f = self.0.borrow_mut();
        if f.graph != graph {
            f.graph = graph;
            f.node = None;
            f.generation += 1;
        }
    }

    /// Selects `node` of the open graph (or none).
    pub fn select(&self, node: Option<NodeId>) {
        let mut f = self.0.borrow_mut();
        if f.node != node {
            f.node = node;
            f.generation += 1;
        }
    }

    /// A number that changes whenever the graph or the node does.
    pub fn generation(&self) -> u64 {
        self.0.borrow().generation
    }
}

/// Allows a show's needs on this machine, as the person (D11): the approval card's "Allow on this machine". Never an
/// op and never an MCP tool, so no agent can approve what its own show needs.
pub trait Approve: Send + Sync {
    /// Approves `needs` for `show`, as `by` asked, and arms the running show with them.
    ///
    /// # Errors
    /// Why it could not be saved, in words.
    fn allow(&self, show: &ShowId, needs: &[Need], by: &Actor) -> Result<(), String>;
}

/// The approver the app hands the shell as a view model.
#[derive(Clone)]
pub struct ShowApprover(pub Arc<dyn Approve>);

impl std::fmt::Debug for ShowApprover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShowApprover").finish_non_exhaustive()
    }
}

/// The app's approver: the approvals file in its data folder, then a re-arm of the running show.
pub struct DiskApprover {
    automation: Arc<Automation>,
    approvals: Arc<DiskApprovals>,
    /// The project folder's real path, which keys the approvals as the engine does.
    project: String,
}

impl DiskApprover {
    /// Approves into `approvals` for the shows of the project at `project`, re-arming `automation`.
    pub fn new(automation: Arc<Automation>, approvals: Arc<DiskApprovals>, project: impl Into<String>) -> Self {
        Self { automation, approvals, project: project.into() }
    }
}

impl Approve for DiskApprover {
    fn allow(&self, show: &ShowId, needs: &[Need], by: &Actor) -> Result<(), String> {
        let key = ShowKey::new(self.project.clone(), show.clone());
        self.approvals.allow(&key, needs, &[], by).map_err(|e| e.to_string())?;
        self.automation.rearm();
        Ok(())
    }
}
