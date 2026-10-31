//! Rendering any state of the project, not only now (#400, `docs/design/branches.md` 6.1 and 6.3): a transaction
//! `{"tx": 7}`, a snapshot, a point or another branch `{"branch": "aria/oval-face"}`. The state's tree is read through
//! the lane's [`HistoryView`] and rendered from a copy, so the live document never changes and the same picture comes
//! from the same state wherever it is asked for: `render.preview {state}`, `render.compare {a, b, shot}`, the studio's
//! branch compare and its merge view.

use std::sync::Arc;

use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Value, lookup};
use omuzaki_history_api::state::{BranchStore, HistoryError, HistoryView, StateRef};
use omuzaki_ops_api::{JobCtx, OpError, Services};

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
