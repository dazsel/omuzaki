//! The branch ops (`docs/design/branches.md` 6 and 7): `branch.list` and `branch.status` read the branches as refs and
//! points, and `branch.merge` merges another branch's point into this lane's document, unit by unit, as one undoable
//! step. They read other branches only through [`BranchStore`](omuzaki_history_api::state::BranchStore), never another
//! lane's live document, so an op never waits on another lane.

mod list;
mod merge;

use omuzaki_history_api::branch::BranchError;
use omuzaki_ops_api::OpError;

/// The merge op's name: the host marks a waiting review merged when one applies.
pub const MERGE_OP: &str = "branch.merge";

/// A branch error as the op error an agent reads, with the hint where there is one.
pub(crate) fn branch_error(error: BranchError) -> OpError {
    OpError::from(error)
}
