//! The automation engine (BR-11; design `docs/design/automation.md`).
//!
//! - [`Automation`]: the service the app builds once. It is the `EventSink` the platform adapters publish into and
//!   the `AutomationControl` the runtime ops call; one show runs at a time, on an engine thread of its own.
//! - The ops (in `ops`): `show.create`, `automation.add_graph`, `automation.remove_graph` on the document;
//!   `automation.start`, `automation.stop`, `automation.status`, `automation.values` and `automation.fire` on the
//!   running show.
//!
//! - [`vars::DiskVariables`]: the values of `show` and `global` variables on disk, written atomically; the engine
//!   writes each file at most once a second and flushes on stop (design D10).
//!
//! - [`approvals::DiskApprovals`]: what a person approved per show on this machine (design D11), and
//!   [`approvals::ceiling`], the automation session's grants. Only the app's approval card writes it.
//!
//! - [`testing::ShowHarness`] (feature `testing`): a whole show on the real engine core, on fake time.
//!
//! The node catalogue is `omuzaki-live-nodes`; the engine finds kinds through the registry and never names one.

omuzaki_core::define_link! {
    omuzaki_live_api::link();
}

pub mod approvals;
pub mod compile;
mod core;
mod dispatch;
mod edits;
mod inbox;
mod l10n;
pub mod ops;
mod queries;
mod service;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod vars;

pub use service::Automation;
