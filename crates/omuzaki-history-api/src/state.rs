//! State references and the services that read branches and history (design 5.7 and 6.1).

use std::sync::Arc;

use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Service, TxId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::branch::{BranchError, BranchInfo, BranchName, Point, PointId};
use crate::{SnapshotError, SnapshotId};

/// A document state, for diff, compare, restore and branching (VC-02, VC-03). Transactions are those of the lane the
/// call runs on.
///
/// Serialised as `"now"`, `{"tx": 7}`, `{"snapshot": "…"}`, `{"point": "…"}` or `{"branch": "aria/oval-face"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "StateRefWire", into = "StateRefWire")]
#[schemars(with = "StateRefWire")]
#[non_exhaustive]
pub enum StateRef {
    /// The lane's live document: the op's own.
    Now,
    /// The state just after that committed transaction of this lane's branch, in this session or an earlier one.
    #[non_exhaustive]
    Tx {
        /// The transaction.
        tx: TxId,
    },
    /// That snapshot's tree.
    #[non_exhaustive]
    Snapshot {
        /// The snapshot.
        id: SnapshotId,
    },
    /// That point's snapshot's tree.
    #[non_exhaustive]
    Point {
        /// The point.
        id: PointId,
    },
    /// The branch's head point; for the op's own branch, [`StateRef::Now`].
    #[non_exhaustive]
    Branch {
        /// The branch.
        name: BranchName,
    },
}

impl StateRef {
    /// [`StateRef::Tx`].
    pub fn tx(tx: TxId) -> Self {
        Self::Tx { tx }
    }

    /// [`StateRef::Snapshot`].
    pub fn snapshot(id: SnapshotId) -> Self {
        Self::Snapshot { id }
    }

    /// [`StateRef::Point`].
    pub fn point(id: PointId) -> Self {
        Self::Point { id }
    }

    /// [`StateRef::Branch`].
    pub fn branch(name: BranchName) -> Self {
        Self::Branch { name }
    }
}

/// The wire form of [`StateRef`]: newtype variants give the short JSON forms (`{"tx": 7}`).
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "StateRef")]
enum StateRefWire {
    Now,
    Tx(TxId),
    Snapshot(SnapshotId),
    Point(PointId),
    Branch(BranchName),
}

impl From<StateRefWire> for StateRef {
    fn from(wire: StateRefWire) -> Self {
        match wire {
            StateRefWire::Now => Self::Now,
            StateRefWire::Tx(tx) => Self::Tx { tx },
            StateRefWire::Snapshot(id) => Self::Snapshot { id },
            StateRefWire::Point(id) => Self::Point { id },
            StateRefWire::Branch(name) => Self::Branch { name },
        }
    }
}

impl From<StateRef> for StateRefWire {
    fn from(state: StateRef) -> Self {
        match state {
            StateRef::Now => Self::Now,
            StateRef::Tx { tx } => Self::Tx(tx),
            StateRef::Snapshot { id } => Self::Snapshot(id),
            StateRef::Point { id } => Self::Point(id),
            StateRef::Branch { name } => Self::Branch(name),
        }
    }
}

/// Why a state could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HistoryError {
    /// The branch's journal and archive hold no such transaction.
    #[error("no transaction {} on this branch", .0.0)]
    UnknownTx(TxId),
    /// The snapshot could not be read.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    /// The branch or point could not be found or read.
    #[error(transparent)]
    Branch(#[from] BranchError),
    /// The journal or the archive could not be read.
    #[error("history storage failed: {0}")]
    Io(String),
}

/// What ops read of the branches (a [`Service`]): branches as refs and points, never another lane's live document.
pub trait BranchStore: Service {
    /// The branch of the lane the op runs on.
    ///
    /// # Errors
    /// [`BranchError::Io`] when the ref cannot be read.
    fn this(&self) -> Result<BranchInfo, BranchError>;

    /// Every branch.
    ///
    /// # Errors
    /// [`BranchError::Io`] when the refs cannot be read.
    fn list(&self) -> Result<Vec<BranchInfo>, BranchError>;

    /// The branch called `name`.
    ///
    /// # Errors
    /// [`BranchError::NotFound`].
    fn branch(&self, name: &BranchName) -> Result<BranchInfo, BranchError>;

    /// The point `id`.
    ///
    /// # Errors
    /// [`BranchError::PointNotFound`].
    fn point(&self, id: &PointId) -> Result<Point, BranchError>;

    /// The document tree of point `id`; implementations cache it.
    ///
    /// # Errors
    /// [`BranchError::PointNotFound`], or [`BranchError::Io`] when its snapshot cannot be read.
    fn tree(&self, id: &PointId) -> Result<Arc<Value>, BranchError>;
}

/// The tree of any state of this lane's branch (a [`Service`]); `Now` is the op's own document.
pub trait HistoryView: Service {
    /// The document tree at `at`.
    ///
    /// # Errors
    /// [`HistoryError::UnknownTx`] for a transaction the journal does not hold, [`HistoryError::Snapshot`] or
    /// [`HistoryError::Branch`] when what the reference names cannot be found, [`HistoryError::Io`].
    fn tree(&self, at: &StateRef) -> Result<Arc<Value>, HistoryError>;
}

#[cfg(test)]
mod tests {
    use omuzaki_store_api::ContentHash;
    use serde_json::{Value, json};

    use super::*;

    fn point_id() -> PointId {
        PointId(ContentHash::of(b"point"))
    }

    fn snapshot_id() -> SnapshotId {
        SnapshotId(ContentHash::of(b"snapshot"))
    }

    fn name() -> BranchName {
        BranchName::new("aria/oval-face").unwrap_or_else(|_| BranchName::main())
    }

    #[test]
    fn every_form_serialises_as_documented_and_reads_back() {
        let cases = [
            (StateRef::Now, json!("now")),
            (StateRef::tx(TxId(7)), json!({"tx": 7})),
            (StateRef::snapshot(snapshot_id()), json!({"snapshot": snapshot_id().to_string()})),
            (StateRef::point(point_id()), json!({"point": point_id().to_string()})),
            (StateRef::branch(name()), json!({"branch": "aria/oval-face"})),
        ];
        for (state, wire) in cases {
            assert_eq!(serde_json::to_value(&state).ok(), Some(wire.clone()), "{state:?}");
            assert_eq!(serde_json::from_value::<StateRef>(wire).ok(), Some(state));
        }
    }

    #[test]
    fn malformed_references_are_refused() {
        for bad in [
            json!("then"),
            json!({"tx": "seven"}),
            json!({"branch": "Aria"}),
            json!({"point": "abc"}),
            json!({"tx": 1, "now": null}),
            json!({}),
        ] {
            assert!(serde_json::from_value::<StateRef>(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_schema_lists_every_form() {
        let schema = serde_json::to_value(schemars::schema_for!(StateRef)).unwrap_or_default().to_string();
        for form in ["now", "tx", "snapshot", "point", "branch"] {
            assert!(schema.contains(&format!("\"{form}\"")), "{form} missing: {schema}");
        }
        assert!(schema.contains("StateRef"), "{schema}");
        let _: Value = serde_json::from_str(&schema).unwrap_or_default();
    }

    #[test]
    fn history_errors_wrap_what_failed() {
        let from_snapshot = HistoryError::from(SnapshotError::NotFound(snapshot_id()));
        assert!(from_snapshot.to_string().contains("no snapshot"), "{from_snapshot}");
        let from_branch = HistoryError::from(BranchError::PointNotFound(point_id()));
        assert!(from_branch.to_string().contains("no point"), "{from_branch}");
        assert!(HistoryError::UnknownTx(TxId(9)).to_string().contains('9'));
    }
}
