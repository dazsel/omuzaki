//! The services the engine provides and uses (design D7, D10, D11, D13), each an object-safe `Send + Sync` trait that
//! the app wires in through `omuzaki_ops_api::Services`:
//! - [`AutomationControl`]: what the runtime ops (`automation.start`, `stop`, `status`, `values`, ...) call; the
//!   engine implements it. Its [`Status`] and [`ValuesSnapshot`] are what those queries return.
//! - [`Approvals`]: what the person approved for a show on this machine; the approvals store implements it.
//! - [`VariableStore`]: where variable values persist; the disk stores implement it.
//! - [`WallClock`]: wall time, which tests fake; [`SystemWallClock`] reads the OS.

mod approvals;
mod control;
mod variables;
mod wall;

pub use approvals::{Approvals, ApprovedProgram, ShowApprovals, ShowKey};
pub use control::{
    AutomationControl, Dropped, EventRow, Firing, GraphStatus, GroupRow, InertNode, InertReason, NodeRef, RunInfo,
    Status, SuspendReason, Suspension, ValuesSnapshot, VarRow, shown,
};
pub use variables::{PersistScope, VarStoreError, VariableStore};
pub use wall::{SystemWallClock, WallClock};
