//! Contract for show automation (BR-11, BR-26; design `docs/design/automation.md`, ADR 0027).
//!
//! `omuzaki-live-api` is an API crate (§39 rule 1): data types, traits and pure functions that the engine
//! (`omuzaki-live`), the node catalogue (`omuzaki-live-nodes`), the event producers and the editor share. Callers name
//! items by path (`omuzaki_live_api::node::NodeKind`, ADR 0054 §2); only [`prelude`] gathers the ids and values.
//! - [`ids`]: the slug ids of shows, graphs, nodes, groups, variables and platforms; node kind ids, event kinds, port
//!   names and the `<node>.<port>` wire reference
//! - [`show`]: [`ShowDef`](show::ShowDef) over `shows/<id>`, its document [`paths`](show::paths), the show keys other
//!   features register ([`register_show_key!`]) and the structural [`check`](show::ShowDef::check) (design D2, D18,
//!   D21)
//! - [`graph`]: [`GraphDef`](graph::GraphDef), [`NodeDef`](graph::NodeDef) and the [`Wire`](graph::Wire) stored at its
//!   destination
//! - [`limits`]: every number of design D8, and the few others the engine and the ops read
//! - [`value`]: [`Val`](value::Val), [`PortType`](value::PortType) and the coercions of design D3
//! - [`node`]: ports, [`NodeKind`](node::NodeKind), [`Node`](node::Node), [`Input`](node::Input),
//!   [`NodeCx`](node::NodeCx), the registration [`register_node!`] and the errors (design D4)
//! - [`need`]: [`Need`](need::Need), what a node needs approved before it runs (design D11)
//! - [`event`]: the envelope [`LiveEvent`](event::LiveEvent), [`register_event_kind!`], the normalised platform
//!   payloads and [`EventSink`](event::EventSink) (design D9)
//! - [`services`]: [`AutomationControl`](services::AutomationControl) with its status and values types,
//!   [`Approvals`](services::Approvals), [`VariableStore`](services::VariableStore) and
//!   [`WallClock`](services::WallClock) (design D7, D10, D11, D13)
//! - [`l10n`]: the sentences this crate says to people, with the `live-api-` prefix
#![deny(missing_docs)]

omuzaki_core::define_link!();

pub mod event;
pub mod graph;
pub mod ids;
pub mod l10n;
pub mod limits;
pub mod need;
pub mod node;
pub mod services;
pub mod show;
pub mod value;

/// The names nearly every caller needs: the ids, [`Val`](crate::value::Val) and [`PortType`](crate::value::PortType)
/// (design D21). Everything else is named by its path.
pub mod prelude {
    pub use crate::ids::{
        EventKind, GraphId, GroupId, NodeId, NodeKindId, Platform, PortName, PortRef, ShowId, VarName,
    };
    pub use crate::value::{PortType, Val};
}

// Re-exports the registration macros expand to, so a crate that registers needs neither `inventory` nor
// `omuzaki-core` in its own manifest.
#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
