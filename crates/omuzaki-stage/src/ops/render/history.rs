//! Rendering any state of the project, not only now (#400, `docs/design/branches.md` 6.1 and 6.3): a transaction
//! `{"tx": 7}`, a snapshot, a point or another branch `{"branch": "aria/oval-face"}`. The state's tree is read through
//! the lane's [`HistoryView`] and rendered from a copy, so the live document never changes and the same picture comes
//! from the same state wherever it is asked for: `render.preview {state}`, `render.compare {a, b, shot}`, the studio's
//! branch compare and its merge view.

use std::sync::Arc;

use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Value, lookup};
use omuzaki_history_api::state::{BranchStore, HistoryError, HistoryView, StateRef};
use omuzaki_ops_api::{Actor, CancelFlag, JobCtx, JobDoc, JobId, JobSignal, Op, OpError, Services};

use super::preview::{Preview, PreviewResult};

/// A state's tree, read like a document.
#[derive(Clone, Debug)]
pub struct StateDoc {
    root: Arc<Value>,
}

impl StateDoc {
    /// The tree `root` as a document.
    pub fn new(root: Arc<Value>) -> Self {
        Self { root }
    }

    /// The whole tree.
    pub fn root(&self) -> &Value {
        &self.root
    }
}

impl DocRead for StateDoc {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        lookup(&self.root, path)
    }

    fn version(&self) -> DocVersion {
        DocVersion(0)
    }
}

impl JobDoc for StateDoc {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(self);
    }
}

/// The tree `state` names, from `services`' history; `None` for the op's own document (no state, `now`, or this
/// lane's own branch).
///
/// # Errors
/// [`OpError::unavailable`] when the services read no history (a document that isn't a project), and
/// [`OpError::not_found`] or [`OpError::unavailable`] when the state can't be read.
pub fn state_doc(services: &Services, state: Option<&StateRef>) -> Result<Option<StateDoc>, OpError> {
    let Some(state) = state.filter(|s| **s != StateRef::Now) else { return Ok(None) };
    if let StateRef::Branch { name, .. } = state
        && let Some(store) = services.get::<dyn BranchStore>()
        && store.this().is_ok_and(|this| this.name == *name)
    {
        return Ok(None);
    }
    let history = services.get::<dyn HistoryView>().ok_or_else(|| {
        OpError::unavailable("history").hint("past states and other branches render only in a project folder")
    })?;
    history.tree(state).map(|root| Some(StateDoc::new(root))).map_err(|e| history_error(state, &e))
}

/// Runs `f` over the state's tree when there is one, else over the job's own document.
pub fn read<R>(job: &JobCtx<'_>, state: Option<&StateDoc>, f: impl FnOnce(&dyn DocRead) -> R) -> R {
    match state {
        Some(doc) => f(doc),
        None => job.read(f),
    }
}

/// `preview` of the character as `tree` holds it, rendered with `services` outside any runner: the studio's pictures
/// of branches and of a merge's two sides, which must not add steps to the history they show. The picture goes to
/// the services' store, like `render.preview`'s.
///
/// # Errors
/// What `render.preview`'s work reports: a character the tree doesn't have, a shot that doesn't frame, a missing
/// service, a failed render.
pub fn picture_of(tree: Arc<Value>, preview: &Preview, services: &Services) -> Result<PreviewResult, OpError> {
    let doc = StateDoc::new(tree);
    let actor = Actor::System;
    let job = JobCtx::new(JobId(0), &actor, &doc, &Quiet).with_services(services);
    let preview = Preview { state: None, ..preview.clone() };
    let value = preview.work(&job)?;
    serde_json::from_value(value).map_err(|e| OpError::internal(format!("reading the picture back: {e}")))
}

/// A job signal for a picture nobody watches: no progress, never cancelled, spends nothing.
struct Quiet;

impl JobSignal for Quiet {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancel_flag(&self) -> CancelFlag {
        CancelFlag::new()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Err(OpError::denied("a picture of a state spends nothing"))
    }
}

fn history_error(state: &StateRef, error: &HistoryError) -> OpError {
    let what = serde_json::to_string(state).unwrap_or_default();
    match error {
        HistoryError::UnknownTx(_) | HistoryError::Branch(_) | HistoryError::Snapshot(_) => {
            OpError::not_found(format!("the state {what} can't be read: {error}"))
                .hint("history.list lists transactions; branch.list lists branches")
        }
        _ => OpError::unavailable("history").hint(format!("reading {what}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_and_no_state_read_the_ops_own_document() {
        let services = Services::default();
        assert!(state_doc(&services, None).unwrap_or_else(|e| panic!("{e}")).is_none());
        assert!(state_doc(&services, Some(&StateRef::Now)).unwrap_or_else(|e| panic!("{e}")).is_none());
        let past = StateRef::tx(omuzaki_ops_api::TxId(3));
        let refused = state_doc(&services, Some(&past)).err().unwrap_or_else(|| panic!("no history, no past state"));
        assert!(refused.to_string().contains("history"), "{refused}");
    }
}
