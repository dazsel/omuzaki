//! The automation engine (BR-11; design `docs/design/automation.md`).
//!
//! - [`Automation`]: the service the app builds once. It is the `EventSink` the platform adapters publish into and
//!   the `AutomationControl` the runtime ops call; one show runs at a time, on an engine thread of its own.
//! - The ops (in `ops`): `show.create`, `automation.add_graph`, `automation.remove_graph` on the document;
//!   `automation.start`, `automation.stop`, `automation.status`, `automation.values` and `automation.fire` on the
//!   running show.
//!
//! The node catalogue is `omuzaki-live-nodes`; the engine finds kinds through the registry and never names one.

omuzaki_core::define_link!();

mod compile;
mod core;
mod inbox;
pub mod ops;
mod service;

pub use service::Automation;
